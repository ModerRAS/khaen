//! [NONRT]/[IO] Linux ALSA duplex（core-v1 §5.4 / §6.1 / §6.4 / §11.9）—— WBS T17 写域。
//!
//! 本模块交付 **duplex 协商 / actual 参数 / 启动恢复状态机 / fake backend 离线**：
//!
//! - **稳定设备身份**：`DeviceSelector` 用 ALSA card **id/name**（如 `UltraLitemk5`）
//!   而不是写死 `hw:4,0`；`find_card_index` 从 `/proc/asound/cards` 文本解析并拒绝
//!   NotFound/Ambiguous（§5.4：禁止假 card4 永远不变）。
//! - **duplex 协商**：`StreamCaps`（两方向独立能力）+ `DuplexRequest` → `negotiate`
//!   一次定出 `HwParamsPlan`（format 优先级、48k/96k、period ∈ {64,128}、buffer 为
//!   period 整数倍、方向间 period/buffer/rate/format 必须一致，见 core-v1 §11.9 单
//!   `KhaenHwParamsV1` 语义）。
//! - **actual 参数**：`RawActual`（backend 协商后的真实 hwparams）→ `validate_actual`
//!   → `DuplexActual{ hw: KhaenHwParamsV1, ... }`；**actual period ∉ {64,128} 直接
//!   ERR_DEVICE_PARAMS，禁止静默夹取/内部拆分**（§6.1 I11；负向对照见 tests/T17）。
//!   `hwparams_hash` 给出 owner 交接/重开证据用的确定性哈希。
//! - **启动/恢复状态机**：`DuplexSession` 驱动
//!   Closed → Opening → Negotiating → Configuring → Starting → Running；
//!   启动失败有界重试（`RecoveryPolicy::startup_attempts`），失败回滚并释放 owner；
//!   运行期 RT_E_DEVICE_LOST/xrun 进入 Recovering（**静音**，只允许
//!   `write_silence`），按 Suspend/Prepare/Resume 有界恢复；成功**强制新 epoch**
//!   （§5.2），重试耗尽进入 Muted（保持设备静音，不自动恢复，只能完整重新准入）。
//! - **§5.4 DeviceOwner**：`DeviceOwner` 单 owner（Playback/Calibration 互斥）+
//!   交接窗口超时；每次变更记录 EV_DEVICE_OWNER_CHANGE 与前后 actual hwparams hash。
//! - **fake backend 离线**：`FakeBackend`（按 T01 E1 证据 MOTU 20ch capture / 22ch
//!   playback @48k S24_3LE period128/buffer1024）完全离线、可脚本注入 open/apply/start/
//!   recover/IO 故障；测试不需要声卡。
//! - **Linux 真实后端**（T90 target-specific `alsa` 0.12 依赖）：`#[cfg(target_os="linux")]` 的
//!   `LinuxAlsaBackend` 用 alsa crate 打开 stable-ID 解析出的 `hw:<card>,<device>`（板端即
//!   `hw:4,0`）、协商 **S24_3LE 20ch capture / 22ch playback**、period/buffer（T01 E1 参考
//!   128/1024，arecord 建议 1024/16384 由请求决定）、读取 xrun/suspended/disconnected 状态并按
//!   Suspend/Prepare/Resume 恢复、close 释放设备；**非 Linux target 不编译该模块**，继续用
//!   fake backend；Windows/macOS/CI 不链接 libasound。
//!
//! ## S1 运行期不打开硬件（WBS T17 硬约束）
//!
//! T90 已按 Lead 批准加入 **target-specific** `alsa = 0.12`（仅 Linux target 编译/链接；
//! Windows/macOS/CI 不编译）。真实打开路径只在 `#[cfg(target_os="linux")]` 的
//! `LinuxAlsaBackend` 中，并且仍由两道闸门关闭：
//!
//! 1. `PcmDuplexBackend::is_hardware()` + `HwOpenGate`：**所有**真实硬件 backend
//!    （Linux 真实后端与未绑定占位 `NoHwBackend`）默认 Denied，`start()` 在解析设备/
//!    打开前即返回 `HwOpenDenied`；只有 e2e-runner 授权专窗显式 `HwOpenGate::allowed()`
//!    才可能继续。
//! 2. 代码级扫描（tests/T17）对**去除注释后的源码**断言：无 `extern "C"`/FFI、无直接
//!    `/dev/snd` 打开、无 `std::fs` 设备访问；Linux 模块只经 alsa crate 的 safe wrapper。
//!
//! S1 开发期同时运行的旧栈/服务不受本模块影响：本模块不启停服务、不改 CueMix、
//! 不碰板端；Windows 上只跑 fake backend 离线模型测试。
//!
//! ## 错误映射（core-v1 §9.2 冻结码；不新增码）
//!
//! | `AlsaError` | `RtStatus` | 语义/动作 |
//! |---|---|---|
//! | `Rate` | 104 ERR_RATE | 非 48k/96k 或设备不支持 |
//! | `BlockSize` | 105 ERR_BLOCK_SIZE | 请求 period 非 64/128 |
//! | `Channels` | 106 ERR_CHANNELS | 请求/actual 通道越界或不足 |
//! | `StrideAlign` | 109 ERR_STRIDE_ALIGN | stride 不可表达 |
//! | `BadLayout` | 103 ERR_BAD_LAYOUT | 结构/缓冲区长度/算术溢出 |
//! | `DeviceParams` | 118 ERR_DEVICE_PARAMS | **actual period/rate/channels/buffer 不满足** |
//! | `DeviceBusy` / `OwnerTransitionTimeout` | 117 ERR_DEVICE_BUSY | owner 冲突/交接超时 |
//! | `SwitchBusy` | 120 ERR_SWITCH_BUSY | 状态机非法迁移/重复启动/重入 |
//! | `Unsupported` / `BackendUnavailable` / `HwOpenDenied` | 122 ERR_UNSUPPORTED | 格式/绑定/硬件闸门拒绝（fail closed） |
//! | `CardNotFound` / `CardAmbiguous` | 118 ERR_DEVICE_PARAMS | 稳定 ID 无法唯一解析→拒绝启动 |
//! | `Xrun` / `DeviceLost` / `RecoveryExhausted` | 407 RT_E_DEVICE_LOST | 期间静音、进入/维持恢复状态机；恢复需新 epoch |
//!
//! 本文件是 [NONRT]/[IO] 模块（不是 RT-MODULE）：允许在启动/恢复期分配（本实现除
//! `std::env` 读闸门外不分配）、允许 ALSA 恢复 syscall；**不得**进入 DSP worker 调用图。
//! 稳态热路径（`io_read`/`io_write`/`write_silence`）只用固定数组与 Copy 值。
//!
//! 来源：`spec/core-v1.md` §5.4/§6.1/§6.4/§9.2/§11.9；`scratch/lead/planner/
//! DSP-HOST-DECISION.md` §5/§6；`evidence/baseline/board-evidence/E1-DIGITAL-INPUT.md`
//! 与 `e1-duplex-probe-20261116-220517.txt`（MOTU 实测 actual 参数）。

use khaen_core::abi::{sample_format, KhaenHwParamsV1, ABI_VERSION};
use khaen_core::events::EV_DEVICE_OWNER_CHANGE;
use khaen_core::rt::{
    RtStatus, BLOCK_SIZES, MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ,
};

// ---------------------------------------------------------------------------
// 常量（T01 E1 实测基线 + core-v1 硬边界）
// ---------------------------------------------------------------------------

/// 默认 period（core-v1 §3.2 生产 128；B 来源链 = actual period）。
pub const DEFAULT_PERIOD_FRAMES: u32 = 128;
/// 默认 buffer（T01 E1：MOTU chunk256 → actual period128/buffer1024）。
pub const DEFAULT_BUFFER_FRAMES: u32 = 1024;
/// T01 E1 实测：MOTU UltraLite-mk5 USB capture 20 通道。
pub const MOTU_CAPTURE_CHANNELS: u16 = 20;
/// T01 E1 实测：MOTU UltraLite-mk5 USB playback 22 通道。
pub const MOTU_PLAYBACK_CHANNELS: u16 = 22;
/// T01 E1 实测基线采样率。
pub const MOTU_BASELINE_RATE: u32 = 48_000;
/// 单个 period 的最大设备字节数（128 帧 × 22 通道 × 4 B；S24_3LE/S32_LE 上界）。
pub const MAX_PERIOD_BYTES: usize = 128 * MAX_OUTPUT_CHANNELS * 4;
/// 状态机迁移日志固定容量（非 RT；溢出只计数不分配）。
pub const TRANSITION_LOG: usize = 64;
/// owner 变更日志固定容量（§5.4 证据）。
pub const OWNER_CHANGE_LOG: usize = 16;
/// fake backend 调用日志固定容量。
pub const FAKE_CALL_LOG: usize = 128;

/// 采样格式 → 每样本字节数；不属于设备格式/未知返回 `None`。
pub const fn sample_bytes(format: u16) -> Option<u32> {
    match format {
        sample_format::S24_3LE_INTERLEAVED => Some(3),
        sample_format::S32_LE_INTERLEAVED => Some(4),
        sample_format::S16_LE_INTERLEAVED => Some(2),
        sample_format::F32_INTERLEAVED => Some(4),
        _ => None,
    }
}

/// 设备可交付的采样格式（§11.9；F32_PLANAR 仅 core 内部，F64 仅 reference）。
pub const fn device_format_allowed(format: u16, allow_file_fake: bool) -> bool {
    match format {
        sample_format::S24_3LE_INTERLEAVED
        | sample_format::S32_LE_INTERLEAVED
        | sample_format::S16_LE_INTERLEAVED => true,
        // F32_INTERLEAVED 允许（file/fake backend）；真实 ALSA 路径由调用方置 false。
        sample_format::F32_INTERLEAVED => allow_file_fake,
        _ => false,
    }
}

/// 是否 core-v1 §3.2 runtime 允许的 period（64/128）。
pub const fn is_runtime_period(period_frames: u32) -> bool {
    period_frames == BLOCK_SIZES[0] as u32 || period_frames == BLOCK_SIZES[1] as u32
}

/// 是否 core-v1 §3.2 runtime 允许的采样率（48k/96k）。
pub const fn is_runtime_rate(sample_rate: u32) -> bool {
    sample_rate == SAMPLE_RATES_HZ[0] || sample_rate == SAMPLE_RATES_HZ[1]
}

// ---------------------------------------------------------------------------
// 稳定设备身份（§5.4：设备稳定 ID，禁止写死 card4）
// ---------------------------------------------------------------------------

/// `/proc/asound/cards` 的一行卡片条目（借用输入文本，无分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardEntry<'a> {
    /// ALSA card index（0..=31）。
    pub index: u8,
    /// ALSA card id（`[...]` 内文本，稳定标识，如 `UltraLitemk5`）。
    pub id: &'a str,
    /// driver 文本（`:` 后 ` - ` 前）。
    pub driver: &'a str,
    /// 设备名（` - ` 后文本，可能为空）。
    pub name: &'a str,
}

/// 稳定身份查找键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardLookup<'a> {
    /// 按 ALSA card id 精确匹配（推荐）。
    ById(&'a str),
    /// 按 card 名精确匹配，其次按 id 精确匹配（兜底）。
    ByName(&'a str),
}

/// 稳定身份解析失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardLookupError {
    /// 未找到匹配卡片。
    NotFound,
    /// 匹配到多个卡片（拒绝猜测）。
    Ambiguous {
        /// 命中数量（饱和到 u8）。
        count: u8,
    },
}

/// 解析 `/proc/asound/cards` 的单条卡片行；非卡片行（表头/缩进续行）返回 `None`。
///
/// 例：` 4 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5`
pub fn parse_card_entry(line: &str) -> Option<CardEntry<'_>> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || !trimmed.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let open = trimmed.find('[')?;
    let index: u8 = trimmed.get(..open)?.trim().parse().ok()?;
    let after_open = trimmed.get(open + 1..)?;
    let close_rel = after_open.find(']')?;
    let close = open + 1 + close_rel;
    let id = trimmed.get(open + 1..close)?.trim();
    if id.is_empty() {
        return None;
    }
    let rest = trimmed.get(close + 1..)?.trim_start().strip_prefix(':')?;
    let rest = rest.trim();
    let (driver, name) = match rest.split_once(" - ") {
        Some((driver, name)) => (driver.trim(), name.trim()),
        None => (rest, ""),
    };
    Some(CardEntry {
        index,
        id,
        driver,
        name,
    })
}

/// 在 `/proc/asound/cards` 文本中唯一定位 card index（禁止猜测/回退 card4）。
pub fn find_card_index(cards_text: &str, lookup: CardLookup<'_>) -> Result<u8, CardLookupError> {
    let mut found: Option<u8> = None;
    let mut count: u8 = 0;
    for line in cards_text.lines() {
        let entry = match parse_card_entry(line) {
            Some(entry) => entry,
            None => continue,
        };
        let matched = match lookup {
            CardLookup::ById(id) => entry.id == id,
            CardLookup::ByName(name) => entry.name == name || entry.id == name,
        };
        if matched {
            count = count.saturating_add(1);
            if found.is_none() {
                found = Some(entry.index);
            }
        }
    }
    match (found, count) {
        (Some(index), 1) => Ok(index),
        (Some(_), _) => Err(CardLookupError::Ambiguous { count }),
        _ => Err(CardLookupError::NotFound),
    }
}

/// 已解析的设备地址（`hw:<card>,<device>`），不假设任何固定 index。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedDevice {
    /// ALSA card index（由稳定 id/name 解析）。
    pub card_index: u8,
    /// ALSA device index（同一 card 下）。
    pub device_index: u8,
}

impl ResolvedDevice {
    /// 由 `/proc/asound/cards` 文本 + 稳定选择器解析设备地址。
    pub fn from_cards(cards_text: &str, selector: &DeviceSelector<'_>) -> Result<Self, AlsaError> {
        match find_card_index(cards_text, selector.card) {
            Ok(card_index) => Ok(Self {
                card_index,
                device_index: selector.device,
            }),
            Err(CardLookupError::NotFound) => Err(AlsaError::CardNotFound),
            Err(CardLookupError::Ambiguous { .. }) => Err(AlsaError::CardAmbiguous),
        }
    }

    /// 把 `hw:<card>,<device>` 名称写入调用方缓冲（无分配）；返回字节数。
    pub fn write_hw_name(&self, out: &mut [u8]) -> Result<usize, AlsaError> {
        let mut pos = 0usize;
        for byte in b"hw:" {
            if out.get(pos).is_none() {
                return Err(AlsaError::BadLayout);
            }
            out[pos] = *byte;
            pos += 1;
        }
        let rest = out.get_mut(pos..).ok_or(AlsaError::BadLayout)?;
        pos += write_dec_u8(self.card_index, rest)?;
        if out.get(pos).is_none() {
            return Err(AlsaError::BadLayout);
        }
        out[pos] = b',';
        pos += 1;
        let rest = out.get_mut(pos..).ok_or(AlsaError::BadLayout)?;
        pos += write_dec_u8(self.device_index, rest)?;
        Ok(pos)
    }
}

/// 十进制 u8 写入（无分配，供 `hw:` 名称与证据使用）。
fn write_dec_u8(value: u8, out: &mut [u8]) -> Result<usize, AlsaError> {
    let mut digits = [0u8; 3];
    let mut count = 0usize;
    let mut rest = value;
    loop {
        digits[count] = b'0' + (rest % 10);
        count += 1;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if out.len() < count {
        return Err(AlsaError::BadLayout);
    }
    for i in 0..count {
        out[i] = digits[count - 1 - i];
    }
    Ok(count)
}

/// 稳定设备选择器：card 身份 + device index。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceSelector<'a> {
    /// card 稳定身份查找键。
    pub card: CardLookup<'a>,
    /// 同一 card 下的 device index。
    pub device: u8,
}

impl<'a> DeviceSelector<'a> {
    /// 由稳定 card id 构造（推荐：`UltraLitemk5`）。
    pub const fn by_id(card_id: &'a str, device: u8) -> Self {
        Self {
            card: CardLookup::ById(card_id),
            device,
        }
    }

    /// 由 card 名/id 兜底构造。
    pub const fn by_name(name: &'a str, device: u8) -> Self {
        Self {
            card: CardLookup::ByName(name),
            device,
        }
    }
}

// ---------------------------------------------------------------------------
// 错误（映射 core-v1 §9.2 冻结码；不新增码）
// ---------------------------------------------------------------------------

/// T17 设备层错误（Copy；非 RT 边界可用，映射到冻结 `RtStatus`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlsaError {
    /// 请求采样率不受支持（104）。
    Rate,
    /// 请求 period 非 64/128（105）。
    BlockSize,
    /// 通道数越界/不足（106）。
    Channels,
    /// stride 不可表达（109）。
    StrideAlign,
    /// 结构/缓冲区长度/算术错误（103）。
    BadLayout,
    /// actual 参数不满足（118）。
    DeviceParams,
    /// owner 冲突（117）。
    DeviceBusy,
    /// owner 交接窗口超时（117）。
    OwnerTransitionTimeout,
    /// 状态机非法迁移/重复启动（120）。
    SwitchBusy,
    /// 稳定设备身份未找到（118：无法安全打开）。
    CardNotFound,
    /// 稳定设备身份不唯一（118：拒绝猜测）。
    CardAmbiguous,
    /// 格式/能力不支持（122）。
    Unsupported,
    /// 硬件后端未绑定（122；T17 不含 libasound 绑定）。
    BackendUnavailable,
    /// 硬件打开闸门拒绝（122；fail closed）。
    HwOpenDenied,
    /// ALSA xrun/EPIPE（407：静音并进入恢复状态机）。
    Xrun,
    /// 设备丢失（407）。
    DeviceLost,
    /// 恢复重试耗尽（407：进入 Muted，等完整重新 admission）。
    RecoveryExhausted,
}

impl AlsaError {
    /// 映射到 core-v1 §9.2 冻结错误码。
    pub const fn status(self) -> RtStatus {
        match self {
            AlsaError::Rate => RtStatus::ERR_RATE,
            AlsaError::BlockSize => RtStatus::ERR_BLOCK_SIZE,
            AlsaError::Channels => RtStatus::ERR_CHANNELS,
            AlsaError::StrideAlign => RtStatus::ERR_STRIDE_ALIGN,
            AlsaError::BadLayout => RtStatus::ERR_BAD_LAYOUT,
            AlsaError::DeviceParams => RtStatus::ERR_DEVICE_PARAMS,
            AlsaError::DeviceBusy | AlsaError::OwnerTransitionTimeout => RtStatus::ERR_DEVICE_BUSY,
            AlsaError::SwitchBusy => RtStatus::ERR_SWITCH_BUSY,
            AlsaError::CardNotFound | AlsaError::CardAmbiguous => RtStatus::ERR_DEVICE_PARAMS,
            AlsaError::Unsupported | AlsaError::BackendUnavailable | AlsaError::HwOpenDenied => {
                RtStatus::ERR_UNSUPPORTED
            }
            AlsaError::Xrun | AlsaError::DeviceLost | AlsaError::RecoveryExhausted => {
                RtStatus::RT_E_DEVICE_LOST
            }
        }
    }

    /// 稳定名称（证据/日志用；不含 payload，非 RT 格式化）。
    pub const fn name(self) -> &'static str {
        match self {
            AlsaError::Rate => "ERR_RATE",
            AlsaError::BlockSize => "ERR_BLOCK_SIZE",
            AlsaError::Channels => "ERR_CHANNELS",
            AlsaError::StrideAlign => "ERR_STRIDE_ALIGN",
            AlsaError::BadLayout => "ERR_BAD_LAYOUT",
            AlsaError::DeviceParams => "ERR_DEVICE_PARAMS",
            AlsaError::DeviceBusy => "ERR_DEVICE_BUSY",
            AlsaError::OwnerTransitionTimeout => "ERR_OWNER_TRANSITION_TIMEOUT",
            AlsaError::SwitchBusy => "ERR_SWITCH_BUSY",
            AlsaError::CardNotFound => "ERR_CARD_NOT_FOUND",
            AlsaError::CardAmbiguous => "ERR_CARD_AMBIGUOUS",
            AlsaError::Unsupported => "ERR_UNSUPPORTED",
            AlsaError::BackendUnavailable => "ERR_BACKEND_UNAVAILABLE",
            AlsaError::HwOpenDenied => "ERR_HW_OPEN_DENIED",
            AlsaError::Xrun => "RT_E_XRUN",
            AlsaError::DeviceLost => "RT_E_DEVICE_LOST",
            AlsaError::RecoveryExhausted => "RT_E_RECOVERY_EXHAUSTED",
        }
    }

    /// 设备级可恢复（进入恢复状态机）。
    pub const fn device_recoverable(self) -> bool {
        matches!(self, AlsaError::Xrun | AlsaError::DeviceLost)
    }

    /// 启动阶段是否值得重试（设备级瞬时故障）。
    pub const fn startup_retryable(self) -> bool {
        self.device_recoverable()
    }
}

// ---------------------------------------------------------------------------
// 能力与请求（duplex 协商输入）
// ---------------------------------------------------------------------------

/// 流方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamDirection {
    /// 设备 capture（麦克风/数字输入）。
    Capture,
    /// 设备 playback（DAC 输出）。
    Playback,
}

impl StreamDirection {
    /// 稳定名称。
    pub const fn name(self) -> &'static str {
        match self {
            StreamDirection::Capture => "capture",
            StreamDirection::Playback => "playback",
        }
    }
}

/// period/buffer 支持区间（含 step；`min/max` 必须落在 `min + k*step`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueRange {
    /// 最小值（含）。
    pub min: u32,
    /// 最大值（含）。
    pub max: u32,
    /// 步长（0 归一为 1）。
    pub step: u32,
}

impl ValueRange {
    /// 构造（step=0 归一为 1）。
    pub const fn new(min: u32, max: u32, step: u32) -> Self {
        Self {
            min,
            max,
            step: if step == 0 { 1 } else { step },
        }
    }

    /// 值是否在区间且对齐 step。
    pub fn contains(&self, value: u32) -> bool {
        value >= self.min && value <= self.max && (value - self.min) % self.step == 0
    }
}

/// 通道数区间（含端点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelRange {
    /// 最小通道数（含）。
    pub min: u16,
    /// 最大通道数（含）。
    pub max: u16,
}

impl ChannelRange {
    /// 构造。
    pub const fn new(min: u16, max: u16) -> Self {
        Self { min, max }
    }
}

/// 单方向设备能力（来自 backend/ALSA hw_params 查询；fake 离线提供）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamCaps {
    /// 方向。
    pub direction: StreamDirection,
    /// 通道数范围。
    pub channels: ChannelRange,
    /// 支持的 sample_format 位集（bit = §11.9 枚举值）。
    pub format_mask: u32,
    /// 支持的采样率位集（bit0=48k、bit1=96k）。
    pub rate_mask: u16,
    /// period 范围。
    pub period: ValueRange,
    /// buffer 范围。
    pub buffer: ValueRange,
    /// 是否只支持 interleaved（v1 ALSA access=RW_INTERLEAVED 固定）。
    pub interleaved: bool,
}

/// `format_mask`：S24_3LE。
pub const FORMAT_MASK_S24_3LE: u32 = 1u32 << (sample_format::S24_3LE_INTERLEAVED as u32);
/// `format_mask`：S32_LE。
pub const FORMAT_MASK_S32_LE: u32 = 1u32 << (sample_format::S32_LE_INTERLEAVED as u32);
/// `format_mask`：S16_LE。
pub const FORMAT_MASK_S16_LE: u32 = 1u32 << (sample_format::S16_LE_INTERLEAVED as u32);
/// `format_mask`：F32 interleaved（file/fake）。
pub const FORMAT_MASK_F32_INTERLEAVED: u32 = 1u32 << (sample_format::F32_INTERLEAVED as u32);
/// `rate_mask`：48k。
pub const RATE_MASK_48K: u16 = 1;
/// `rate_mask`：96k。
pub const RATE_MASK_96K: u16 = 2;

impl StreamCaps {
    /// 构造。
    pub const fn new(
        direction: StreamDirection,
        channels: ChannelRange,
        format_mask: u32,
        rate_mask: u16,
        period: ValueRange,
        buffer: ValueRange,
        interleaved: bool,
    ) -> Self {
        Self {
            direction,
            channels,
            format_mask,
            rate_mask,
            period,
            buffer,
            interleaved,
        }
    }

    /// 是否支持某 sample_format（§11.9 枚举值）。
    pub fn supports_format(&self, format: u16) -> bool {
        if format >= 32 {
            return false;
        }
        self.format_mask & (1u32 << (format as u32)) != 0
    }

    /// 是否支持某采样率（48k/96k）。
    pub fn supports_rate(&self, sample_rate: u32) -> bool {
        if sample_rate == SAMPLE_RATES_HZ[0] {
            self.rate_mask & RATE_MASK_48K != 0
        } else if sample_rate == SAMPLE_RATES_HZ[1] {
            self.rate_mask & RATE_MASK_96K != 0
        } else {
            false
        }
    }
}

/// duplex 协商请求（control 侧给出，非 RT）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplexRequest {
    /// 目标采样率（48k/96k）。
    pub sample_rate: u32,
    /// 首选 period（必须 64/128；B 的候选来源）。
    pub period_frames: u32,
    /// 期望 buffer（≥ period；最终按 period 整数倍取）。
    pub buffer_frames: u32,
    /// 最少 capture 通道（设备 actual 必须 ≥）。
    pub min_input_channels: u16,
    /// 最少 playback 通道（设备 actual 必须 ≥）。
    pub min_output_channels: u16,
    /// sample_format 优先级（§11.9 枚举值；取第一个双方都支持的）。
    pub format_priority: [u16; 3],
    /// 是否允许 F32_INTERLEAVED（仅 fake/file backend）。
    pub allow_file_fake_formats: bool,
    /// 是否启用 capture sanitize（写入 HwParams.flags bit2；语义归 T22）。
    pub capture_sanitize: bool,
}

impl DuplexRequest {
    /// 默认优先级：S24_3LE（MOTU 实测）> S32_LE > S16_LE。
    pub const DEFAULT_FORMAT_PRIORITY: [u16; 3] = [
        sample_format::S24_3LE_INTERLEAVED,
        sample_format::S32_LE_INTERLEAVED,
        sample_format::S16_LE_INTERLEAVED,
    ];

    /// 构造：48k/96k + 首选 period/buffer + 最少 2/2 通道。
    pub const fn new(sample_rate: u32, period_frames: u32, buffer_frames: u32) -> Self {
        Self {
            sample_rate,
            period_frames,
            buffer_frames,
            min_input_channels: 2,
            min_output_channels: 2,
            format_priority: Self::DEFAULT_FORMAT_PRIORITY,
            allow_file_fake_formats: false,
            capture_sanitize: true,
        }
    }

    /// T01 E1 实测 MOTU 基线：48k / period 128 / buffer 1024。
    pub const fn motu_baseline() -> Self {
        Self::new(
            MOTU_BASELINE_RATE,
            DEFAULT_PERIOD_FRAMES,
            DEFAULT_BUFFER_FRAMES,
        )
    }
}

impl Default for DuplexRequest {
    fn default() -> Self {
        Self::motu_baseline()
    }
}

/// 协商结果（要下发给 backend 的 hw_params 计划）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwParamsPlan {
    /// 采样率。
    pub sample_rate: u32,
    /// sample_format（§11.9 枚举值）。
    pub sample_format: u16,
    /// period（∈ {64,128}）。
    pub period_frames: u32,
    /// buffer（period 的整数倍）。
    pub buffer_frames: u32,
    /// capture 通道（设备 actual 必须 ≥）。
    pub capture_channels: u16,
    /// playback 通道（设备 actual 必须 ≥）。
    pub playback_channels: u16,
}

/// backend 协商后的单方向实际参数（`RawActual` 的一半）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamActual {
    /// actual 通道数。
    pub channels: u16,
    /// actual sample_format。
    pub sample_format: u16,
    /// actual 采样率。
    pub sample_rate: u32,
    /// actual period（必须 ∈ {64,128}，否则 ERR_DEVICE_PARAMS）。
    pub period_frames: u32,
    /// actual buffer（≥ period）。
    pub buffer_frames: u32,
}

/// backend 协商后的 duplex 实际参数（T17 校验前的原始值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawActual {
    /// capture 方向 actual。
    pub capture: StreamActual,
    /// playback 方向 actual。
    pub playback: StreamActual,
}

/// 校验通过的 duplex actual（冻结 `KhaenHwParamsV1` + 计划 + 变更标记 + 哈希）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplexActual {
    /// 冻结 ABI 硬件参数（§11.9；B 来源链 = `hw.period_frames`）。
    pub hw: KhaenHwParamsV1,
    /// 下发给 backend 的计划（对比 actual 判断是否需要重建 plan/新 epoch）。
    pub planned: HwParamsPlan,
    /// actual period 与计划不同（仍 ∈ {64,128}；调用方需 plan 重建 + 新 epoch）。
    pub period_changed: bool,
    /// actual buffer 与计划不同。
    pub buffer_changed: bool,
    /// actual hwparams 确定性哈希（§5.4 owner 交接/重开证据）。
    pub hash: u64,
}

impl DuplexActual {
    /// 是否必须先重建 plan/进入新 epoch（period/buffer 变化）。
    pub const fn requires_new_epoch(&self) -> bool {
        self.period_changed || self.buffer_changed
    }

    /// 每样本字节数。
    pub fn bytes_per_sample(&self) -> Result<u32, AlsaError> {
        sample_bytes(self.hw.sample_format).ok_or(AlsaError::Unsupported)
    }

    /// 单 period 设备字节数（T22 format / T26 I/O 缓冲预分配使用）。
    pub fn period_bytes(&self, direction: StreamDirection) -> Result<usize, AlsaError> {
        let bytes = self.bytes_per_sample()?;
        let channels = match direction {
            StreamDirection::Capture => self.hw.n_in,
            StreamDirection::Playback => self.hw.n_out,
        } as u32;
        let total = channels
            .checked_mul(self.hw.period_frames)
            .ok_or(AlsaError::BadLayout)?
            .checked_mul(bytes)
            .ok_or(AlsaError::BadLayout)?;
        Ok(total as usize)
    }

    /// 校验设备宽度（T57/overlay：role 最大通道 index 必须 < actual 通道数）。
    pub fn validate_width(&self, required: &RequiredDeviceWidth) -> Result<(), AlsaError> {
        if self.hw.n_in < required.min_input_channels
            || self.hw.n_in <= required.max_input_channel_index
            || self.hw.n_out < required.min_output_channels
            || self.hw.n_out <= required.max_output_channel_index
        {
            return Err(AlsaError::Channels);
        }
        Ok(())
    }
}

/// 设备宽度要求（由 overlay role 映射给出；T17 只校验不解析映射）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiredDeviceWidth {
    /// 最少 capture 通道。
    pub min_input_channels: u16,
    /// 最少 playback 通道。
    pub min_output_channels: u16,
    /// capture role 使用的最大 0-based 通道 index。
    pub max_input_channel_index: u16,
    /// playback role 使用的最大 0-based 通道 index。
    pub max_output_channel_index: u16,
}

impl RequiredDeviceWidth {
    /// 构造。
    pub const fn new(
        min_input_channels: u16,
        min_output_channels: u16,
        max_input_channel_index: u16,
        max_output_channel_index: u16,
    ) -> Self {
        Self {
            min_input_channels,
            min_output_channels,
            max_input_channel_index,
            max_output_channel_index,
        }
    }

    /// T01 E1 + WBS §3：Toslink FL/FR = capture ch12/13；4.1.2 输出 role 最大 index=9。
    pub const fn toslink_412() -> Self {
        Self::new(14, 10, 13, 9)
    }
}

// ---------------------------------------------------------------------------
// duplex 协商（纯函数；fake/真实 backend 共用）
// ---------------------------------------------------------------------------

/// 从两个方向的能力 + 请求算出 `HwParamsPlan`。
///
/// 规则（core-v1 §3.2/§6.1/§11.9）：
/// - 请求 period 不是 64/128 → ERR_BLOCK_SIZE（禁止静默夹取）；
/// - rate 必须是 48k/96k 且双方支持，否则 ERR_RATE；
/// - format 取 `format_priority` 中双方都支持且允许来自设备的第一项，否则 ERR_UNSUPPORTED；
/// - period 优先请求值，其次另一 runtime 值（64↔128）；都不支持 → ERR_DEVICE_PARAMS；
/// - buffer 取 ≥ max(请求值, period) 的 period 整数倍，落在双方区间内；
/// - 通道：max(请求最少值, 设备最小) 且 ≤ 设备最大、≤ §3.1 上界。
pub fn negotiate(
    capture: &StreamCaps,
    playback: &StreamCaps,
    request: &DuplexRequest,
) -> Result<HwParamsPlan, AlsaError> {
    if !is_runtime_period(request.period_frames) {
        return Err(AlsaError::BlockSize);
    }
    if !is_runtime_rate(request.sample_rate) {
        return Err(AlsaError::Rate);
    }
    if !capture.supports_rate(request.sample_rate) || !playback.supports_rate(request.sample_rate) {
        return Err(AlsaError::Rate);
    }
    if !capture.interleaved || !playback.interleaved {
        return Err(AlsaError::Unsupported);
    }

    let mut chosen_format: Option<u16> = None;
    for format in request.format_priority {
        if !device_format_allowed(format, request.allow_file_fake_formats) {
            continue;
        }
        if capture.supports_format(format) && playback.supports_format(format) {
            chosen_format = Some(format);
            break;
        }
    }
    let sample_format = match chosen_format {
        Some(format) => format,
        None => return Err(AlsaError::Unsupported),
    };

    let capture_channels = choose_channels(
        request.min_input_channels,
        &capture.channels,
        MAX_INPUT_CHANNELS as u16,
    )?;
    let playback_channels = choose_channels(
        request.min_output_channels,
        &playback.channels,
        MAX_OUTPUT_CHANNELS as u16,
    )?;

    let alternate_period = if request.period_frames == BLOCK_SIZES[0] as u32 {
        BLOCK_SIZES[1] as u32
    } else {
        BLOCK_SIZES[0] as u32
    };
    let mut period_frames = 0u32;
    for candidate in [request.period_frames, alternate_period] {
        if capture.period.contains(candidate) && playback.period.contains(candidate) {
            period_frames = candidate;
            break;
        }
    }
    if period_frames == 0 {
        return Err(AlsaError::DeviceParams);
    }

    let buffer_frames = choose_buffer(&capture.buffer, &playback.buffer, request, period_frames)?;

    Ok(HwParamsPlan {
        sample_rate: request.sample_rate,
        sample_format,
        period_frames,
        buffer_frames,
        capture_channels,
        playback_channels,
    })
}

/// 选通道数：max(请求, 设备最小)，但不得超设备最大/全局上界。
fn choose_channels(
    requested_min: u16,
    range: &ChannelRange,
    global_max: u16,
) -> Result<u16, AlsaError> {
    let max = range.max.min(global_max);
    if range.min == 0 || max < range.min || requested_min > max {
        return Err(AlsaError::Channels);
    }
    Ok(requested_min.max(range.min))
}

/// 选 buffer：≥ max(请求, period) 的 period 整数倍，且双方区间都接受。
fn choose_buffer(
    capture: &ValueRange,
    playback: &ValueRange,
    request: &DuplexRequest,
    period_frames: u32,
) -> Result<u32, AlsaError> {
    let min = capture.min.max(playback.min);
    let max = capture.max.min(playback.max);
    if max < period_frames || max < min {
        return Err(AlsaError::DeviceParams);
    }
    let target = request.buffer_frames.max(period_frames);
    let mut candidate = round_up_to_period(target, period_frames)?;
    if candidate > max {
        // 请求值超过硬件上界：取 ≤ max 的最大 period 整数倍（不是拒绝，也不是夹到任意值）。
        candidate = (max / period_frames) * period_frames;
        if candidate < period_frames {
            return Err(AlsaError::DeviceParams);
        }
    }
    if candidate < min {
        candidate = round_up_to_period(min, period_frames)?;
    }
    while candidate <= max {
        if capture.contains(candidate) && playback.contains(candidate) {
            return Ok(candidate);
        }
        candidate = match candidate.checked_add(period_frames) {
            Some(next) => next,
            None => return Err(AlsaError::DeviceParams),
        };
    }
    Err(AlsaError::DeviceParams)
}

/// 向上取整到 period 的整数倍。
fn round_up_to_period(value: u32, period_frames: u32) -> Result<u32, AlsaError> {
    if period_frames == 0 {
        return Err(AlsaError::BadLayout);
    }
    let rounded = value
        .checked_add(period_frames - 1)
        .ok_or(AlsaError::BadLayout)?
        / period_frames
        * period_frames;
    Ok(rounded.max(period_frames))
}

// ---------------------------------------------------------------------------
// actual 校验（actual period 硬规则 / 单 KhaenHwParamsV1 语义 / stride / hash）
// ---------------------------------------------------------------------------

/// 校验 backend 的 actual 参数并构造冻结 `KhaenHwParamsV1`。
///
/// **硬规则**：两方向 actual period 必须 ∈ {64,128}（否则 ERR_DEVICE_PARAMS，不得
/// 静默夹取）；两方向 rate/format/period/buffer 必须一致（`KhaenHwParamsV1` 只有一组）；
/// actual 通道 ≥ 计划且 ≤ §3.1 上界；buffer ≥ period 且为 period 整数倍。
pub fn validate_actual(
    plan: &HwParamsPlan,
    raw: &RawActual,
    request: &DuplexRequest,
) -> Result<DuplexActual, AlsaError> {
    validate_stream_actual(
        &raw.capture,
        plan.sample_rate,
        plan.capture_channels,
        MAX_INPUT_CHANNELS as u16,
        request,
    )?;
    validate_stream_actual(
        &raw.playback,
        plan.sample_rate,
        plan.playback_channels,
        MAX_OUTPUT_CHANNELS as u16,
        request,
    )?;

    if raw.capture.period_frames != raw.playback.period_frames
        || raw.capture.buffer_frames != raw.playback.buffer_frames
        || raw.capture.sample_rate != raw.playback.sample_rate
        || raw.capture.sample_format != raw.playback.sample_format
    {
        return Err(AlsaError::DeviceParams);
    }

    let hw = build_hw_params(raw, request)?;
    let period_changed = hw.period_frames != plan.period_frames;
    let buffer_changed = hw.buffer_frames != plan.buffer_frames;
    Ok(DuplexActual {
        hw,
        planned: *plan,
        period_changed,
        buffer_changed,
        hash: hwparams_hash(&hw),
    })
}

fn validate_stream_actual(
    actual: &StreamActual,
    expected_rate: u32,
    required_channels: u16,
    max_channels: u16,
    request: &DuplexRequest,
) -> Result<(), AlsaError> {
    if actual.sample_rate != expected_rate || !is_runtime_rate(actual.sample_rate) {
        return Err(AlsaError::Rate);
    }
    if !device_format_allowed(actual.sample_format, request.allow_file_fake_formats) {
        return Err(AlsaError::Unsupported);
    }
    if actual.channels == 0 || actual.channels < required_channels || actual.channels > max_channels
    {
        return Err(AlsaError::Channels);
    }
    if !is_runtime_period(actual.period_frames) {
        // §6.1 I11：actual period 非 64/128 = 拒绝启动/重开，禁止静默夹取。
        return Err(AlsaError::DeviceParams);
    }
    if actual.buffer_frames < actual.period_frames
        || actual.buffer_frames % actual.period_frames != 0
    {
        return Err(AlsaError::DeviceParams);
    }
    Ok(())
}

fn build_hw_params(raw: &RawActual, request: &DuplexRequest) -> Result<KhaenHwParamsV1, AlsaError> {
    let bytes = sample_bytes(raw.capture.sample_format).ok_or(AlsaError::Unsupported)?;
    let in_stride = raw
        .capture
        .channels
        .checked_mul(raw.capture.period_frames as u16)
        .and_then(|value| value.checked_mul(bytes as u16))
        .ok_or(AlsaError::StrideAlign)? as u32;
    let out_stride = raw
        .playback
        .channels
        .checked_mul(raw.playback.period_frames as u16)
        .and_then(|value| value.checked_mul(bytes as u16))
        .ok_or(AlsaError::StrideAlign)? as u32;
    // §11.9：stride ≥ period_frames × 样本字节；本实现取整个 interleaved period 字节跨度。
    let min_stride = raw
        .capture
        .period_frames
        .checked_mul(bytes)
        .ok_or(AlsaError::StrideAlign)?;
    if in_stride < min_stride || out_stride < min_stride {
        return Err(AlsaError::StrideAlign);
    }
    let mut flags = 0u32;
    if request.capture_sanitize {
        flags |= 1 << 2;
    }
    Ok(KhaenHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: raw.capture.sample_format,
        n_in: raw.capture.channels,
        n_out: raw.playback.channels,
        sample_rate: raw.capture.sample_rate,
        period_frames: raw.capture.period_frames,
        buffer_frames: raw.capture.buffer_frames,
        in_stride_bytes: in_stride,
        out_stride_bytes: out_stride,
        flags,
        reserved: [0; 4],
    })
}

/// actual hwparams 确定性哈希（FNV-1a 64，覆盖 §11.9 语义字段，不含 reserved）。
pub fn hwparams_hash(hw: &KhaenHwParamsV1) -> u64 {
    let mut canonical = [0u8; 32];
    let mut offset = 0usize;
    put_u16(&mut canonical, &mut offset, hw.abi_version);
    put_u16(&mut canonical, &mut offset, hw.sample_format);
    put_u16(&mut canonical, &mut offset, hw.n_in);
    put_u16(&mut canonical, &mut offset, hw.n_out);
    put_u32(&mut canonical, &mut offset, hw.sample_rate);
    put_u32(&mut canonical, &mut offset, hw.period_frames);
    put_u32(&mut canonical, &mut offset, hw.buffer_frames);
    put_u32(&mut canonical, &mut offset, hw.in_stride_bytes);
    put_u32(&mut canonical, &mut offset, hw.out_stride_bytes);
    put_u32(&mut canonical, &mut offset, hw.flags);
    fnv1a64(&canonical)
}

fn put_u16(buf: &mut [u8], offset: &mut usize, value: u16) {
    let bytes = value.to_le_bytes();
    if let Some(slot) = buf.get_mut(*offset..*offset + 2) {
        slot.copy_from_slice(&bytes);
    }
    *offset += 2;
}

fn put_u32(buf: &mut [u8], offset: &mut usize, value: u32) {
    let bytes = value.to_le_bytes();
    if let Some(slot) = buf.get_mut(*offset..*offset + 4) {
        slot.copy_from_slice(&bytes);
    }
    *offset += 4;
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut index = 0usize;
    while let Some(byte) = bytes.get(index) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    hash
}

// ---------------------------------------------------------------------------
// 硬件闸门（S1 运行期默认不打开硬件）
// ---------------------------------------------------------------------------

/// 硬件打开策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwPolicy {
    /// 拒绝（默认；S1 开发期/生产默认）。
    Denied,
    /// 显式放行（仅 e2e-runner 授权窗口使用；仍受 backend 是否绑定限制）。
    Allowed,
}

/// 硬件打开闸门：硬件 backend 的 `start()` 在解析/打开设备前必须通过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwOpenGate {
    policy: HwPolicy,
}

/// 闸门环境变量名（只有显式 `1/true/yes/allow` 放行）。
pub const HW_ALLOW_ENV: &str = "KHAEN_ALSA_ALLOW_HW";

impl HwOpenGate {
    /// 默认拒绝。
    pub const fn denied() -> Self {
        Self {
            policy: HwPolicy::Denied,
        }
    }

    /// 显式放行（只允许授权专窗代码构造；文档/证据记录来源）。
    pub const fn allowed() -> Self {
        Self {
            policy: HwPolicy::Allowed,
        }
    }

    /// 由环境变量文本值构造（纯函数，便于测试；未知值一律 Denied）。
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed == "1"
                    || trimmed.eq_ignore_ascii_case("true")
                    || trimmed.eq_ignore_ascii_case("yes")
                    || trimmed.eq_ignore_ascii_case("allow")
                {
                    Self::allowed()
                } else {
                    Self::denied()
                }
            }
            None => Self::denied(),
        }
    }

    /// 读取进程环境（[NONRT] 启动期一次；缺失/畸形 = 拒绝）。
    pub fn from_env() -> Self {
        let value = std::env::var(HW_ALLOW_ENV).ok();
        Self::from_env_value(value.as_deref())
    }

    /// 当前策略。
    pub const fn policy(&self) -> HwPolicy {
        self.policy
    }

    /// 闸门检查（Denied → `HwOpenDenied`；fail closed）。
    pub const fn check(&self) -> Result<(), AlsaError> {
        match self.policy {
            HwPolicy::Allowed => Ok(()),
            HwPolicy::Denied => Err(AlsaError::HwOpenDenied),
        }
    }
}

impl Default for HwOpenGate {
    fn default() -> Self {
        Self::denied()
    }
}

// ---------------------------------------------------------------------------
// backend seam（real Linux 后端 = 未来 integrator/Lead 依赖裁决后实现）
// ---------------------------------------------------------------------------

/// 恢复阶段（ALSA `snd_pcm_recover` 语义：suspend/prepare/resume）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverStage {
    /// 挂起/停流。
    Suspend,
    /// prepare。
    Prepare,
    /// 恢复运行。
    Resume,
}

/// 固定恢复阶段顺序（重试从 Suspend 重新开始）。
pub const RECOVER_STAGES: [RecoverStage; 3] = [
    RecoverStage::Suspend,
    RecoverStage::Prepare,
    RecoverStage::Resume,
];

/// duplex PCM backend 接口。fake backend 离线；真实 Linux 后端未绑定（见模块头）。
pub trait PcmDuplexBackend {
    /// 是否真实硬件 backend（true 时受 `HwOpenGate` 约束）。
    fn is_hardware(&self) -> bool {
        false
    }
    /// 打开设备（非 RT 启动期）。
    fn open(&mut self, device: &ResolvedDevice) -> Result<(), AlsaError>;
    /// 查询单方向能力。
    fn stream_caps(&mut self, direction: StreamDirection) -> Result<StreamCaps, AlsaError>;
    /// 应用 hw_params 并返回 actual（真实后端 = snd_pcm_hw_params + get_*）。
    fn apply_hw_params(&mut self, plan: &HwParamsPlan) -> Result<RawActual, AlsaError>;
    /// 启动流（prepared → running）。
    fn start(&mut self) -> Result<(), AlsaError>;
    /// 读一个 capture period（[IO] 预算内）。
    fn read_period(&mut self, dst: &mut [u8]) -> Result<usize, AlsaError>;
    /// 写一个 playback period（[IO] 预算内）。
    fn write_period(&mut self, src: &[u8]) -> Result<usize, AlsaError>;
    /// drain（停流前）。
    fn drain(&mut self) -> Result<(), AlsaError>;
    /// 恢复阶段。
    fn recover(&mut self, stage: RecoverStage) -> Result<(), AlsaError>;
    /// 关闭（幂等）。
    fn close(&mut self);
}

/// 硬件后端“未绑定/不可用”占位（与 Linux 真实后端并列的另一个 `is_hardware()==true`
/// 实现）：用于没有可用 libasound 绑定的场景/授权前的 fail-closed 语义。
///
/// 本类型不打开任何设备，即使 gate 放行也只返回 `BackendUnavailable`；Linux 真实后端在
/// `#[cfg(target_os="linux")]` 的 `LinuxAlsaBackend` 中（T90 依赖 `alsa` 0.12）。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHwBackend;

impl PcmDuplexBackend for NoHwBackend {
    fn is_hardware(&self) -> bool {
        true
    }
    fn open(&mut self, _device: &ResolvedDevice) -> Result<(), AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn stream_caps(&mut self, _direction: StreamDirection) -> Result<StreamCaps, AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn apply_hw_params(&mut self, _plan: &HwParamsPlan) -> Result<RawActual, AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn start(&mut self) -> Result<(), AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn read_period(&mut self, _dst: &mut [u8]) -> Result<usize, AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn write_period(&mut self, _src: &[u8]) -> Result<usize, AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn drain(&mut self) -> Result<(), AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn recover(&mut self, _stage: RecoverStage) -> Result<(), AlsaError> {
        Err(AlsaError::BackendUnavailable)
    }
    fn close(&mut self) {}
}

// ---------------------------------------------------------------------------
// 平台 backend 选择与 ALSA errno 映射（跨平台纯逻辑；Windows 可测）
// ---------------------------------------------------------------------------

/// 平台可用的 duplex backend 种类（T17/T90）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// 离线 fake backend（不打开硬件；Windows/macOS/CI 与所有离线测试）。
    Fake,
    /// Linux 真实 libasound backend（`alsa` 0.12；仅 Linux target 编译）。
    LinuxAlsa,
}

impl BackendKind {
    /// 该 backend 是否代表真实硬件（true 时 `HwOpenGate` 默认拒绝）。
    pub const fn is_hardware(self) -> bool {
        matches!(self, BackendKind::LinuxAlsa)
    }

    /// 稳定名称。
    pub const fn name(self) -> &'static str {
        match self {
            BackendKind::Fake => "fake",
            BackendKind::LinuxAlsa => "linux-alsa",
        }
    }
}

/// 本 target 的默认平台 backend：Linux = `LinuxAlsa`，其它 = `Fake`。
///
/// 注意：默认种类只表示“可实现的能力”；真正打开硬件仍必须先通过 `HwOpenGate`。
#[cfg(target_os = "linux")]
pub const fn platform_backend_kind() -> BackendKind {
    BackendKind::LinuxAlsa
}

/// 本 target 的默认平台 backend：Linux = `LinuxAlsa`，其它 = `Fake`。
#[cfg(not(target_os = "linux"))]
pub const fn platform_backend_kind() -> BackendKind {
    BackendKind::Fake
}

/// backend 种类是否硬件（与 `BackendKind::is_hardware` 等价的自由函数，便于 T26 使用）。
pub const fn backend_kind_is_hardware(kind: BackendKind) -> bool {
    kind.is_hardware()
}

/// ALSA errno（POSIX 值，正/负都可）→ T17 `AlsaError`（core-v1 §9.2 冻结码映射）。
///
/// 规则：EPIPE(-32)/ESTRPIPE(-86) 是可恢复流事件；ENODEV/ENXIO/EIO/EBADFD 视为设备丢失；
/// EBUSY 是设备独占冲突；EINVAL 视为参数不满足；ENOMEM 视为布局/容量失败；
/// ENOSYS/EOPNOTSUPP 为不支持；未知 errno 保守按设备丢失（可进入恢复状态机）处理。
pub const fn alsa_errno_to_error(errno: i32) -> AlsaError {
    let code = if errno < 0 {
        errno.saturating_neg()
    } else {
        errno
    };
    match code {
        16 => AlsaError::DeviceBusy,              // EBUSY
        19 | 6 | 5 | 77 => AlsaError::DeviceLost, // ENODEV / ENXIO / EIO / EBADFD
        22 => AlsaError::DeviceParams,            // EINVAL
        12 => AlsaError::BadLayout,               // ENOMEM（预分配/缓冲区不足）
        32 => AlsaError::Xrun,                    // EPIPE
        86 => AlsaError::DeviceLost,              // ESTRPIPE（suspend，恢复需重开流）
        38 | 95 => AlsaError::Unsupported,        // ENOSYS / EOPNOTSUPP
        _ => AlsaError::DeviceLost,
    }
}

// ---------------------------------------------------------------------------
// Linux 真实 libasound backend（T90 `alsa = 0.12`；仅 target_os = linux 编译）
// ---------------------------------------------------------------------------

/// Linux 真实 duplex backend（capture + playback 各自一个 `PCM`；板端目标 `hw:4,0`）。
///
/// **S1 运行期不打开硬件**：本类型 `is_hardware()==true`，只能由显式
/// `HwOpenGate::allowed()` 的授权专窗代码构造并 `open()`；默认 gate 在 `start()`
/// 阶段即拒绝，不会触碰 PCM。Windows/macOS 不编译本模块（依赖 target-specific）。
///
/// 语义映射（与 `FakeBackend` 相同 trait 契约）：
/// - `open`：按 stable-ID 解析出的 `hw:<card>,<device>` 分别打开 capture/playback
///   （非 block 模式；ALSA `hw` 设备通常独占，EBUSY → `ERR_DEVICE_BUSY`），
///   失败时释放已打开的一侧；
/// - `stream_caps`：从 `HwParams::any` 读取 channels/period/buffer 范围，用
///   `test_format`/`test_rate`/`test_access` 探测 S24_3LE/S32_LE/S16_LE、
///   48k/96k、RW_INTERLEAVED；
/// - `apply_hw_params`：对两侧分别 set_access/format/channels/rate/period_near/
///   buffer_near，然后 `hw_params` 并回读 actual（period/buffer 由 T17 校验为
///   64/128 与 period 整数倍，不在此夹取）；
/// - `start`：capture 显式 `snd_pcm_start`；playback 留待首次 writei 自动启动，
///   避免空 buffer 立即 underrun；
/// - `read_period`/`write_period`：先读 `state()`：XRun → prepare 并返回
///   `AlsaError::Xrun`；Suspended/Disconnected/Open/Setup → `DeviceLost`；其它状态
///   走 `io_bytes().readi/writei`（返回帧数 → 字节数；短读/短写原样返回）；
/// - `recover`：Suspend = 两侧 drop，Prepare = 两侧 prepare，Resume = capture start
///   （playback 由首次写自动启动）；
/// - `drain`：playback drain + capture drop；`close`：两侧置 None（PCM Drop 关闭 fd）。
#[cfg(target_os = "linux")]
pub struct LinuxAlsaBackend {
    capture: Option<alsa::pcm::PCM>,
    playback: Option<alsa::pcm::PCM>,
    capture_actual: Option<StreamActual>,
    playback_actual: Option<StreamActual>,
}

/// Linux PCM 流状态快照（证据/诊断用；不是 RT 状态机）。
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxStreamState {
    /// SND_PCM_STATE_OPEN。
    Open,
    /// SND_PCM_STATE_SETUP。
    Setup,
    /// SND_PCM_STATE_PREPARED。
    Prepared,
    /// SND_PCM_STATE_RUNNING。
    Running,
    /// SND_PCM_STATE_XRUN。
    XRun,
    /// SND_PCM_STATE_DRAINING。
    Draining,
    /// SND_PCM_STATE_PAUSED。
    Paused,
    /// SND_PCM_STATE_SUSPENDED。
    Suspended,
    /// SND_PCM_STATE_DISCONNECTED。
    Disconnected,
}

#[cfg(target_os = "linux")]
impl LinuxAlsaBackend {
    /// 未打开状态。
    pub const fn new() -> Self {
        Self {
            capture: None,
            playback: None,
            capture_actual: None,
            playback_actual: None,
        }
    }

    /// 是否持有 PCM 句柄。
    pub fn is_open(&self) -> bool {
        self.capture.is_some() && self.playback.is_some()
    }

    /// capture 侧状态快照。
    pub fn capture_state(&self) -> Option<LinuxStreamState> {
        self.capture
            .as_ref()
            .map(|pcm| map_stream_state(pcm.state()))
    }

    /// playback 侧状态快照。
    pub fn playback_state(&self) -> Option<LinuxStreamState> {
        self.playback
            .as_ref()
            .map(|pcm| map_stream_state(pcm.state()))
    }

    fn pcm(&self, direction: StreamDirection) -> Result<&alsa::pcm::PCM, AlsaError> {
        let slot = match direction {
            StreamDirection::Capture => self.capture.as_ref(),
            StreamDirection::Playback => self.playback.as_ref(),
        };
        slot.ok_or(AlsaError::BackendUnavailable)
    }
}

#[cfg(target_os = "linux")]
impl Default for LinuxAlsaBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// ALSA `Error` → `AlsaError`（Linux only；取 errno 绝对值后走跨平台映射）。
#[cfg(target_os = "linux")]
fn map_alsa_error(error: alsa::Error) -> AlsaError {
    alsa_errno_to_error(error.errno())
}

/// SS11.9 sample_format → alsa `Format`（Linux only）。
#[cfg(target_os = "linux")]
fn alsa_format(format: u16) -> Result<alsa::pcm::Format, AlsaError> {
    match format {
        sample_format::S24_3LE_INTERLEAVED => Ok(alsa::pcm::Format::S243LE),
        sample_format::S32_LE_INTERLEAVED => Ok(alsa::pcm::Format::S32LE),
        sample_format::S16_LE_INTERLEAVED => Ok(alsa::pcm::Format::S16LE),
        sample_format::F32_INTERLEAVED => Ok(alsa::pcm::Format::FloatLE),
        _ => Err(AlsaError::Unsupported),
    }
}

/// alsa `Format` → SS11.9 sample_format（Linux only）。
#[cfg(target_os = "linux")]
fn sample_format_of(format: alsa::pcm::Format) -> Result<u16, AlsaError> {
    match format {
        alsa::pcm::Format::S243LE => Ok(sample_format::S24_3LE_INTERLEAVED),
        alsa::pcm::Format::S32LE => Ok(sample_format::S32_LE_INTERLEAVED),
        alsa::pcm::Format::S16LE => Ok(sample_format::S16_LE_INTERLEAVED),
        alsa::pcm::Format::FloatLE => Ok(sample_format::F32_INTERLEAVED),
        _ => Err(AlsaError::Unsupported),
    }
}

/// i64 帧数 → u32（负值/溢出 = 布局错误）。
#[cfg(target_os = "linux")]
fn frames_to_u32(value: alsa::pcm::Frames) -> Result<u32, AlsaError> {
    u32::try_from(value).map_err(|_| AlsaError::BadLayout)
}

/// 交错帧字节数 = 通道 × 每样本字节。
#[cfg(target_os = "linux")]
fn frame_bytes(actual: &StreamActual) -> Result<usize, AlsaError> {
    let bytes = sample_bytes(actual.sample_format).ok_or(AlsaError::Unsupported)? as usize;
    let channels = actual.channels as usize;
    let total = channels.checked_mul(bytes).ok_or(AlsaError::BadLayout)?;
    if total == 0 {
        return Err(AlsaError::BadLayout);
    }
    Ok(total)
}

/// 打开单个 PCM（非 block；hw 设备独占时 EBUSY → DeviceBusy）。
#[cfg(target_os = "linux")]
fn open_pcm(name: &str, direction: alsa::Direction) -> Result<alsa::pcm::PCM, AlsaError> {
    alsa::pcm::PCM::new(name, direction, false).map_err(map_alsa_error)
}

/// 查询单方向 `StreamCaps`（HwParams::any 探测；不改变设备状态）。
#[cfg(target_os = "linux")]
fn query_stream_caps(
    pcm: &alsa::pcm::PCM,
    direction: StreamDirection,
) -> Result<StreamCaps, AlsaError> {
    use alsa::pcm::Access;
    let params = alsa::pcm::HwParams::any(pcm).map_err(map_alsa_error)?;
    let channels = ChannelRange::new(
        u16::try_from(params.get_channels_min().map_err(map_alsa_error)?)
            .map_err(|_| AlsaError::BadLayout)?,
        u16::try_from(params.get_channels_max().map_err(map_alsa_error)?)
            .map_err(|_| AlsaError::BadLayout)?,
    );
    let period = ValueRange::new(
        frames_to_u32(params.get_period_size_min().map_err(map_alsa_error)?)?,
        frames_to_u32(params.get_period_size_max().map_err(map_alsa_error)?)?,
        1,
    );
    let buffer = ValueRange::new(
        frames_to_u32(params.get_buffer_size_min().map_err(map_alsa_error)?)?,
        frames_to_u32(params.get_buffer_size_max().map_err(map_alsa_error)?)?,
        1,
    );
    let mut format_mask = 0u32;
    for (format, id) in [
        (
            alsa::pcm::Format::S243LE,
            sample_format::S24_3LE_INTERLEAVED,
        ),
        (alsa::pcm::Format::S32LE, sample_format::S32_LE_INTERLEAVED),
        (alsa::pcm::Format::S16LE, sample_format::S16_LE_INTERLEAVED),
    ] {
        if params.test_format(format).is_ok() {
            format_mask |= 1u32 << (id as u32);
        }
    }
    let mut rate_mask = 0u16;
    if params.test_rate(SAMPLE_RATES_HZ[0]).is_ok() {
        rate_mask |= RATE_MASK_48K;
    }
    if params.test_rate(SAMPLE_RATES_HZ[1]).is_ok() {
        rate_mask |= RATE_MASK_96K;
    }
    let interleaved = params.test_access(Access::RWInterleaved).is_ok();
    Ok(StreamCaps::new(
        direction,
        channels,
        format_mask,
        rate_mask,
        period,
        buffer,
        interleaved,
    ))
}

/// 对单个 PCM 应用计划并回读 actual。
#[cfg(target_os = "linux")]
fn configure_pcm(
    pcm: &alsa::pcm::PCM,
    plan: &HwParamsPlan,
    channels: u16,
) -> Result<StreamActual, AlsaError> {
    use alsa::pcm::Access;
    use alsa::ValueOr;
    let params = alsa::pcm::HwParams::any(pcm).map_err(map_alsa_error)?;
    params
        .set_access(Access::RWInterleaved)
        .map_err(map_alsa_error)?;
    params
        .set_format(alsa_format(plan.sample_format)?)
        .map_err(map_alsa_error)?;
    params
        .set_channels(channels as u32)
        .map_err(map_alsa_error)?;
    // 显式 set_rate（不是 near）：不静默改 rate；actual 再校验。
    params
        .set_rate(plan.sample_rate, ValueOr::Nearest)
        .map_err(map_alsa_error)?;
    let _ = params
        .set_period_size_near(plan.period_frames as alsa::pcm::Frames, ValueOr::Nearest)
        .map_err(map_alsa_error)?;
    let _ = params
        .set_buffer_size_near(plan.buffer_frames as alsa::pcm::Frames)
        .map_err(map_alsa_error)?;
    pcm.hw_params(&params).map_err(map_alsa_error)?;
    let current = pcm.hw_params_current().map_err(map_alsa_error)?;
    Ok(StreamActual {
        channels: u16::try_from(current.get_channels().map_err(map_alsa_error)?)
            .map_err(|_| AlsaError::BadLayout)?,
        sample_format: sample_format_of(current.get_format().map_err(map_alsa_error)?)?,
        sample_rate: current.get_rate().map_err(map_alsa_error)?,
        period_frames: frames_to_u32(current.get_period_size().map_err(map_alsa_error)?)?,
        buffer_frames: frames_to_u32(current.get_buffer_size().map_err(map_alsa_error)?)?,
    })
}

/// 读取 PCM 状态；xrun/suspended/disconnected 直接映射为恢复/设备错误。
#[cfg(target_os = "linux")]
fn check_stream_state(pcm: &alsa::pcm::PCM) -> Result<(), AlsaError> {
    use alsa::pcm::State;
    match pcm.state() {
        State::XRun => {
            // 先 prepare 让下一次调用可继续；session 会进入 Recovering（静音）。
            let _ = pcm.prepare();
            Err(AlsaError::Xrun)
        }
        State::Suspended | State::Disconnected | State::Open | State::Setup => {
            Err(AlsaError::DeviceLost)
        }
        _ => Ok(()),
    }
}

/// IO 错误 → AlsaError；可恢复错误先 prepare（避免下次调用继续 EPIPE）。
#[cfg(target_os = "linux")]
fn map_io_error(pcm: &alsa::pcm::PCM, error: alsa::Error) -> AlsaError {
    let mapped = map_alsa_error(error);
    if mapped.device_recoverable() {
        let _ = pcm.prepare();
    }
    mapped
}

/// SND_PCM_STATE_* → `LinuxStreamState`。
#[cfg(target_os = "linux")]
fn map_stream_state(state: alsa::pcm::State) -> LinuxStreamState {
    use alsa::pcm::State;
    match state {
        State::Open => LinuxStreamState::Open,
        State::Setup => LinuxStreamState::Setup,
        State::Prepared => LinuxStreamState::Prepared,
        State::Running => LinuxStreamState::Running,
        State::XRun => LinuxStreamState::XRun,
        State::Draining => LinuxStreamState::Draining,
        State::Paused => LinuxStreamState::Paused,
        State::Suspended => LinuxStreamState::Suspended,
        State::Disconnected => LinuxStreamState::Disconnected,
    }
}

#[cfg(target_os = "linux")]
impl PcmDuplexBackend for LinuxAlsaBackend {
    fn is_hardware(&self) -> bool {
        true
    }

    fn open(&mut self, device: &ResolvedDevice) -> Result<(), AlsaError> {
        if self.capture.is_some() || self.playback.is_some() {
            return Err(AlsaError::DeviceBusy);
        }
        let mut name_buffer = [0u8; 16];
        let length = device.write_hw_name(&mut name_buffer)?;
        let name =
            core::str::from_utf8(&name_buffer[..length]).map_err(|_| AlsaError::BadLayout)?;
        let capture = open_pcm(name, alsa::Direction::Capture)?;
        match open_pcm(name, alsa::Direction::Playback) {
            Ok(playback) => {
                self.capture = Some(capture);
                self.playback = Some(playback);
                Ok(())
            }
            Err(error) => {
                drop(capture);
                Err(error)
            }
        }
    }

    fn stream_caps(&mut self, direction: StreamDirection) -> Result<StreamCaps, AlsaError> {
        let pcm = self.pcm(direction)?;
        query_stream_caps(pcm, direction)
    }

    fn apply_hw_params(&mut self, plan: &HwParamsPlan) -> Result<RawActual, AlsaError> {
        let capture_actual = {
            let capture = self.capture.as_ref().ok_or(AlsaError::BackendUnavailable)?;
            configure_pcm(capture, plan, plan.capture_channels)?
        };
        let playback_actual = {
            let playback = self
                .playback
                .as_ref()
                .ok_or(AlsaError::BackendUnavailable)?;
            configure_pcm(playback, plan, plan.playback_channels)?
        };
        self.capture_actual = Some(capture_actual);
        self.playback_actual = Some(playback_actual);
        Ok(RawActual {
            capture: capture_actual,
            playback: playback_actual,
        })
    }

    fn start(&mut self) -> Result<(), AlsaError> {
        let capture = self.capture.as_ref().ok_or(AlsaError::BackendUnavailable)?;
        capture.start().map_err(map_alsa_error)?;
        // playback 由首次 writei 自动启动（避免空 buffer 立即 underrun；见 MANIFEST 偏差 D-07）。
        Ok(())
    }

    fn read_period(&mut self, dst: &mut [u8]) -> Result<usize, AlsaError> {
        let actual = self.capture_actual.ok_or(AlsaError::BackendUnavailable)?;
        let frame = frame_bytes(&actual)?;
        if dst.len() % frame != 0 {
            return Err(AlsaError::BadLayout);
        }
        let pcm = self.capture.as_ref().ok_or(AlsaError::BackendUnavailable)?;
        check_stream_state(pcm)?;
        let io = pcm.io_bytes();
        match io.readi(dst) {
            Ok(frames) => Ok(frames.saturating_mul(frame)),
            Err(error) => Err(map_io_error(pcm, error)),
        }
    }

    fn write_period(&mut self, src: &[u8]) -> Result<usize, AlsaError> {
        let actual = self.playback_actual.ok_or(AlsaError::BackendUnavailable)?;
        let frame = frame_bytes(&actual)?;
        if src.len() % frame != 0 {
            return Err(AlsaError::BadLayout);
        }
        let pcm = self
            .playback
            .as_ref()
            .ok_or(AlsaError::BackendUnavailable)?;
        check_stream_state(pcm)?;
        let io = pcm.io_bytes();
        match io.writei(src) {
            Ok(frames) => Ok(frames.saturating_mul(frame)),
            Err(error) => Err(map_io_error(pcm, error)),
        }
    }

    fn drain(&mut self) -> Result<(), AlsaError> {
        let result = match self.playback.as_ref() {
            Some(playback) => playback.drain().map_err(map_alsa_error),
            None => Err(AlsaError::BackendUnavailable),
        };
        if let Some(capture) = self.capture.as_ref() {
            let _ = capture.drop();
        }
        result
    }

    fn recover(&mut self, stage: RecoverStage) -> Result<(), AlsaError> {
        let capture = self.capture.as_ref().ok_or(AlsaError::BackendUnavailable)?;
        let playback = self
            .playback
            .as_ref()
            .ok_or(AlsaError::BackendUnavailable)?;
        match stage {
            RecoverStage::Suspend => {
                capture.drop().map_err(map_alsa_error)?;
                playback.drop().map_err(map_alsa_error)?;
                Ok(())
            }
            RecoverStage::Prepare => {
                capture.prepare().map_err(map_alsa_error)?;
                playback.prepare().map_err(map_alsa_error)?;
                Ok(())
            }
            RecoverStage::Resume => {
                capture.start().map_err(map_alsa_error)?;
                Ok(())
            }
        }
    }

    fn close(&mut self) {
        self.capture_actual = None;
        self.playback_actual = None;
        self.capture = None;
        self.playback = None;
    }
}

// ---------------------------------------------------------------------------
// fake backend（离线；T01 E1 MOTU 参数）
// ---------------------------------------------------------------------------

/// fake backend 调用记录（证据/断言用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeCall {
    /// open。
    Open,
    /// 能力查询。
    Caps(StreamDirection),
    /// apply hw_params。
    Apply,
    /// start。
    Start,
    /// read period（字节数）。
    Read(usize),
    /// write period（字节数）。
    Write(usize),
    /// drain。
    Drain,
    /// recover 阶段。
    Recover(RecoverStage),
    /// close。
    Close,
}

/// 离线 fake backend（不触碰 `std::fs` / `/dev/snd`，完全确定性）。
///
/// 默认能力 = T01 E1 实测 MOTU：capture 20ch / playback 22ch、S24_3LE @48k、
/// period 64..=2048、buffer 64..=16384；`apply_hw_params` 精确回显计划，可用
/// `actual_override` 模拟驱动 clamp/异常，用 `*_failures` 注入启动/恢复故障，
/// 用 `fail_next_read`/`fail_next_write` 注入 xrun/设备丢失。
#[derive(Clone, Debug)]
pub struct FakeBackend {
    /// capture 能力。
    pub capture_caps: StreamCaps,
    /// playback 能力。
    pub playback_caps: StreamCaps,
    /// 强制返回的 raw actual（None = 精确回显计划）。
    pub actual_override: Option<RawActual>,
    /// 前 N 次 open 返回 `DeviceLost`。
    pub open_failures: u8,
    /// 前 N 次 apply 返回 `DeviceLost`。
    pub apply_failures: u8,
    /// 前 N 次 start 返回 `DeviceLost`。
    pub start_failures: u8,
    /// 前 N 次 recover 失败（255 ≈ 始终失败，由策略上界终止）。
    pub recover_failures: u8,
    /// 下一次 read 返回的错误（take 语义）。
    pub next_read_error: Option<AlsaError>,
    /// 下一次 write 返回的错误（take 语义）。
    pub next_write_error: Option<AlsaError>,
    /// 是否已打开。
    pub opened: bool,
    /// 是否已启动。
    pub started: bool,
    /// 最近一次 plan。
    pub applied_plan: Option<HwParamsPlan>,
    /// 最近一次 actual。
    pub applied_actual: Option<RawActual>,
    /// 调用日志。
    calls: [FakeCall; FAKE_CALL_LOG],
    call_len: usize,
    /// read 次数。
    pub read_calls: u32,
    /// write 次数。
    pub write_calls: u32,
    /// 全零 write 次数（静音证据）。
    pub silence_writes: u32,
    /// drain 次数。
    pub drain_calls: u32,
    /// recover 调用次数。
    pub recover_calls: u32,
    /// close 调用次数。
    pub close_calls: u32,
}

impl FakeBackend {
    /// T01 E1 基线（MOTU UltraLite-mk5，card id `UltraLitemk5`）。
    pub fn motu_ultralite_mk5() -> Self {
        let capture_caps = StreamCaps::new(
            StreamDirection::Capture,
            ChannelRange::new(MOTU_CAPTURE_CHANNELS, MOTU_CAPTURE_CHANNELS),
            FORMAT_MASK_S24_3LE,
            RATE_MASK_48K,
            ValueRange::new(64, 2048, 1),
            ValueRange::new(64, 16384, 1),
            true,
        );
        let playback_caps = StreamCaps::new(
            StreamDirection::Playback,
            ChannelRange::new(MOTU_PLAYBACK_CHANNELS, MOTU_PLAYBACK_CHANNELS),
            FORMAT_MASK_S24_3LE,
            RATE_MASK_48K,
            ValueRange::new(64, 2048, 1),
            ValueRange::new(64, 16384, 1),
            true,
        );
        Self::new(capture_caps, playback_caps)
    }

    /// 由两方向能力构造。
    pub fn new(capture_caps: StreamCaps, playback_caps: StreamCaps) -> Self {
        Self {
            capture_caps,
            playback_caps,
            actual_override: None,
            open_failures: 0,
            apply_failures: 0,
            start_failures: 0,
            recover_failures: 0,
            next_read_error: None,
            next_write_error: None,
            opened: false,
            started: false,
            applied_plan: None,
            applied_actual: None,
            calls: [FakeCall::Close; FAKE_CALL_LOG],
            call_len: 0,
            read_calls: 0,
            write_calls: 0,
            silence_writes: 0,
            drain_calls: 0,
            recover_calls: 0,
            close_calls: 0,
        }
    }

    /// 设置强制 raw actual。
    pub fn with_actual_override(mut self, actual: RawActual) -> Self {
        self.actual_override = Some(actual);
        self
    }

    /// 记录一次调用（容量满后丢弃，仅饱和计数不分配）。
    fn record(&mut self, call: FakeCall) {
        if let Some(slot) = self.calls.get_mut(self.call_len) {
            *slot = call;
            self.call_len += 1;
        }
    }

    /// 调用日志切片。
    pub fn calls(&self) -> &[FakeCall] {
        &self.calls[..self.call_len.min(FAKE_CALL_LOG)]
    }

    /// 清空调用日志（保留计数）。
    pub fn clear_calls(&mut self) {
        self.call_len = 0;
    }

    /// 注入下一次 read 失败。
    pub fn fail_next_read(&mut self, error: AlsaError) {
        self.next_read_error = Some(error);
    }

    /// 注入下一次 write 失败。
    pub fn fail_next_write(&mut self, error: AlsaError) {
        self.next_write_error = Some(error);
    }

    /// 由计划精确回显 actual。
    fn exact_actual(plan: &HwParamsPlan) -> RawActual {
        let capture = StreamActual {
            channels: plan.capture_channels,
            sample_format: plan.sample_format,
            sample_rate: plan.sample_rate,
            period_frames: plan.period_frames,
            buffer_frames: plan.buffer_frames,
        };
        let playback = StreamActual {
            channels: plan.playback_channels,
            ..capture
        };
        RawActual { capture, playback }
    }
}

impl PcmDuplexBackend for FakeBackend {
    fn open(&mut self, _device: &ResolvedDevice) -> Result<(), AlsaError> {
        self.record(FakeCall::Open);
        if self.open_failures > 0 {
            self.open_failures -= 1;
            return Err(AlsaError::DeviceLost);
        }
        self.opened = true;
        Ok(())
    }

    fn stream_caps(&mut self, direction: StreamDirection) -> Result<StreamCaps, AlsaError> {
        self.record(FakeCall::Caps(direction));
        match direction {
            StreamDirection::Capture => Ok(self.capture_caps),
            StreamDirection::Playback => Ok(self.playback_caps),
        }
    }

    fn apply_hw_params(&mut self, plan: &HwParamsPlan) -> Result<RawActual, AlsaError> {
        self.record(FakeCall::Apply);
        if self.apply_failures > 0 {
            self.apply_failures -= 1;
            return Err(AlsaError::DeviceLost);
        }
        let actual = match self.actual_override {
            Some(raw) => raw,
            None => Self::exact_actual(plan),
        };
        self.applied_plan = Some(*plan);
        self.applied_actual = Some(actual);
        Ok(actual)
    }

    fn start(&mut self) -> Result<(), AlsaError> {
        self.record(FakeCall::Start);
        if self.start_failures > 0 {
            self.start_failures -= 1;
            return Err(AlsaError::DeviceLost);
        }
        self.started = true;
        Ok(())
    }

    fn read_period(&mut self, dst: &mut [u8]) -> Result<usize, AlsaError> {
        self.record(FakeCall::Read(dst.len()));
        if let Some(error) = self.next_read_error.take() {
            return Err(error);
        }
        if !self.started {
            return Err(AlsaError::DeviceLost);
        }
        self.read_calls += 1;
        let written = dst.len();
        for (index, byte) in dst.iter_mut().enumerate() {
            *byte = index as u8;
        }
        Ok(written)
    }

    fn write_period(&mut self, src: &[u8]) -> Result<usize, AlsaError> {
        self.record(FakeCall::Write(src.len()));
        if let Some(error) = self.next_write_error.take() {
            return Err(error);
        }
        if !self.started {
            return Err(AlsaError::DeviceLost);
        }
        self.write_calls += 1;
        if src.iter().all(|byte| *byte == 0) {
            self.silence_writes += 1;
        }
        Ok(src.len())
    }

    fn drain(&mut self) -> Result<(), AlsaError> {
        self.record(FakeCall::Drain);
        self.drain_calls += 1;
        self.started = false;
        Ok(())
    }

    fn recover(&mut self, stage: RecoverStage) -> Result<(), AlsaError> {
        self.record(FakeCall::Recover(stage));
        self.recover_calls += 1;
        if self.recover_failures > 0 {
            self.recover_failures -= 1;
            return Err(AlsaError::DeviceLost);
        }
        if !self.opened {
            return Err(AlsaError::DeviceLost);
        }
        Ok(())
    }

    fn close(&mut self) {
        self.record(FakeCall::Close);
        self.close_calls += 1;
        self.opened = false;
        self.started = false;
    }
}

// ---------------------------------------------------------------------------
// §5.4 DeviceOwner（Playback/Calibration 互斥 + 交接窗口 + hash 记录）
// ---------------------------------------------------------------------------

/// 设备 owner 种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    /// 日常播放 host（playback session）。
    Playback,
    /// 校准 daemon（扫频/采集）。
    Calibration,
}

impl OwnerKind {
    /// 稳定名称。
    pub const fn name(self) -> &'static str {
        match self {
            OwnerKind::Playback => "playback",
            OwnerKind::Calibration => "calibration",
        }
    }
}

/// owner 状态（§5.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerState {
    /// 无 owner（设备未打开）。
    Idle,
    /// 播放 host 持有。
    Playback,
    /// 校准 daemon 持有。
    Calibration,
    /// 交接窗口（旧 owner 已 release、新 owner 尚未 acquire）。
    Transition,
    /// 交接超时：必须 SafetyMute/FAIL，不得两个进程同时开设备。
    Stalled,
}

/// 一次 owner 变更记录（§5.4：EV_DEVICE_OWNER_CHANGE + 前后 actual hwparams hash）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerChange {
    /// = `EV_DEVICE_OWNER_CHANGE`。
    pub event: u16,
    /// 旧 owner（None = Idle）。
    pub from: Option<OwnerKind>,
    /// 新 owner（None = Idle/失败关闭）。
    pub to: Option<OwnerKind>,
    /// 变更前 actual hwparams hash（0 = 无 actual）。
    pub hw_hash_before: u64,
    /// 变更后 actual hwparams hash（0 = 尚未打开）。
    pub hw_hash_after: u64,
}

/// 设备唯一 owner（§5.4；离线模型 + 交接窗口）。
#[derive(Clone, Debug)]
pub struct DeviceOwner {
    state: OwnerState,
    current: Option<OwnerKind>,
    pending: Option<OwnerKind>,
    deadline_ns: u64,
    handover_from_hash: u64,
    changes: [OwnerChange; OWNER_CHANGE_LOG],
    change_len: usize,
    dropped_changes: u32,
}

impl DeviceOwner {
    /// 空 owner。
    pub const fn new() -> Self {
        Self {
            state: OwnerState::Idle,
            current: None,
            pending: None,
            deadline_ns: 0,
            handover_from_hash: 0,
            changes: [OwnerChange {
                event: EV_DEVICE_OWNER_CHANGE,
                from: None,
                to: None,
                hw_hash_before: 0,
                hw_hash_after: 0,
            }; OWNER_CHANGE_LOG],
            change_len: 0,
            dropped_changes: 0,
        }
    }

    /// 当前状态。
    pub const fn state(&self) -> OwnerState {
        self.state
    }

    /// 当前 owner。
    pub const fn current(&self) -> Option<OwnerKind> {
        self.current
    }

    /// 变更记录切片。
    pub fn changes(&self) -> &[OwnerChange] {
        &self.changes[..self.change_len.min(OWNER_CHANGE_LOG)]
    }

    /// 溢出丢弃的变更记录数（固定日志，不分配）。
    pub const fn dropped_changes(&self) -> u32 {
        self.dropped_changes
    }

    fn record(&mut self, change: OwnerChange) {
        if let Some(slot) = self.changes.get_mut(self.change_len) {
            *slot = change;
            self.change_len += 1;
        } else {
            self.dropped_changes = self.dropped_changes.saturating_add(1);
        }
    }

    /// 从 Idle 获取 owner；已有其他 owner → ERR_DEVICE_BUSY（不得抢占）。
    pub fn acquire(&mut self, kind: OwnerKind) -> Result<OwnerChange, AlsaError> {
        if self.state != OwnerState::Idle {
            return Err(AlsaError::DeviceBusy);
        }
        self.current = Some(kind);
        self.state = match kind {
            OwnerKind::Playback => OwnerState::Playback,
            OwnerKind::Calibration => OwnerState::Calibration,
        };
        let change = OwnerChange {
            event: EV_DEVICE_OWNER_CHANGE,
            from: None,
            to: Some(kind),
            hw_hash_before: 0,
            hw_hash_after: 0,
        };
        self.record(change);
        Ok(change)
    }

    /// 释放 owner（→ Idle）。
    pub fn release(&mut self) -> Result<OwnerChange, AlsaError> {
        let current = match self.current {
            Some(kind) => kind,
            None => return Err(AlsaError::SwitchBusy),
        };
        self.current = None;
        self.state = OwnerState::Idle;
        let change = OwnerChange {
            event: EV_DEVICE_OWNER_CHANGE,
            from: Some(current),
            to: None,
            hw_hash_before: 0,
            hw_hash_after: 0,
        };
        self.record(change);
        Ok(change)
    }

    /// 开始交接窗口：记录旧 actual hash，设置超时 deadline。
    pub fn begin_handover(
        &mut self,
        now_ns: u64,
        timeout_ns: u64,
        from_hash: u64,
    ) -> Result<(), AlsaError> {
        let current = match (self.state, self.current) {
            (OwnerState::Playback, Some(OwnerKind::Playback)) => OwnerKind::Playback,
            (OwnerState::Calibration, Some(OwnerKind::Calibration)) => OwnerKind::Calibration,
            _ => return Err(AlsaError::SwitchBusy),
        };
        self.pending = Some(match current {
            OwnerKind::Playback => OwnerKind::Calibration,
            OwnerKind::Calibration => OwnerKind::Playback,
        });
        self.deadline_ns = now_ns.saturating_add(timeout_ns);
        self.handover_from_hash = from_hash;
        self.state = OwnerState::Transition;
        Ok(())
    }

    /// 完成交接（窗口内）：记录 before/after actual hash。
    pub fn complete_handover(
        &mut self,
        now_ns: u64,
        after_hash: u64,
    ) -> Result<OwnerChange, AlsaError> {
        let from = match self.current {
            Some(kind) => kind,
            None => return Err(AlsaError::SwitchBusy),
        };
        if self.state != OwnerState::Transition {
            return Err(AlsaError::SwitchBusy);
        }
        let to = match self.pending {
            Some(kind) => kind,
            None => return Err(AlsaError::SwitchBusy),
        };
        if now_ns > self.deadline_ns {
            // §5.4：超时 → 必须 SafetyMute/FAIL，不得两个进程同时开设备。
            self.current = None;
            self.pending = None;
            self.state = OwnerState::Stalled;
            let change = OwnerChange {
                event: EV_DEVICE_OWNER_CHANGE,
                from: Some(from),
                to: None,
                hw_hash_before: self.handover_from_hash,
                hw_hash_after: 0,
            };
            self.record(change);
            return Err(AlsaError::OwnerTransitionTimeout);
        }
        self.current = Some(to);
        self.pending = None;
        self.state = match to {
            OwnerKind::Playback => OwnerState::Playback,
            OwnerKind::Calibration => OwnerState::Calibration,
        };
        let change = OwnerChange {
            event: EV_DEVICE_OWNER_CHANGE,
            from: Some(from),
            to: Some(to),
            hw_hash_before: self.handover_from_hash,
            hw_hash_after: after_hash,
        };
        self.record(change);
        Ok(change)
    }

    /// 显式失败关闭（超时/故障：释放 owner，Stalled）。
    pub fn fail_closed(&mut self) -> OwnerChange {
        let from = self.current;
        self.current = None;
        self.pending = None;
        self.state = OwnerState::Stalled;
        let change = OwnerChange {
            event: EV_DEVICE_OWNER_CHANGE,
            from,
            to: None,
            hw_hash_before: 0,
            hw_hash_after: 0,
        };
        self.record(change);
        change
    }
}

impl Default for DeviceOwner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 启动/恢复状态机
// ---------------------------------------------------------------------------

/// duplex session 状态（§5.1 Starting 子状态 + §5.2 新 epoch 规则）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplexState {
    /// 未打开/已回滚。
    Closed,
    /// 正在打开设备。
    Opening,
    /// 正在协商 hw_params。
    Negotiating,
    /// plan 已定，正在应用 actual。
    Configuring,
    /// 正在启动流。
    Starting,
    /// 正常运行。
    Running,
    /// 设备恢复中：**只能输出静音**（`write_silence`）。
    Recovering,
    /// 安全静音：设备保持，等待非 RT 完整重新 admission（不自动恢复）。
    Muted,
    /// 不可恢复 fault：保持静音/隔离，只能重启/重新 admission。
    Faulted,
}

/// 状态机迁移原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionKind {
    /// 打开设备。
    Open,
    /// 能力查询成功。
    CapsOk,
    /// hw_params 协商完成。
    Negotiated,
    /// actual 参数校验通过。
    Configured,
    /// 流启动成功。
    StartOk,
    /// 启动失败后重试。
    StartupRetry,
    /// 启动失败回滚。
    StartupFailed,
    /// 设备丢失。
    IoDeviceLost,
    /// xrun/EPIPE。
    IoXrun,
    /// 恢复阶段成功。
    RecoverStageOk,
    /// 恢复尝试失败。
    RecoverAttemptFailed,
    /// 恢复成功（新 epoch）。
    Recovered,
    /// 恢复重试耗尽（Muted）。
    RecoveryExhausted,
    /// 停止请求。
    StopRequested,
    /// 已停止（设备关闭）。
    Stopped,
    /// 交接给校准。
    OwnerHandover,
    /// 校准 actual 已登记。
    CalibrationOpened,
    /// 从静音/fault 重新准入。
    Reacquire,
    /// fault。
    Fault,
}

/// 一次状态迁移记录（固定容量，无分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    /// 起始状态。
    pub from: DuplexState,
    /// 目标状态。
    pub to: DuplexState,
    /// 原因。
    pub kind: TransitionKind,
    /// 迁移时 epoch（新 epoch 在成功后写入）。
    pub epoch_id: u64,
}

/// 有界启动/恢复策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryPolicy {
    /// 启动尝试次数（≥1；只有设备级瞬时错误才消耗重试）。
    pub startup_attempts: u8,
    /// 运行期恢复尝试次数（≥1；超过 → Muted）。
    pub recovery_attempts: u8,
}

impl RecoveryPolicy {
    /// 构造。
    pub const fn new(startup_attempts: u8, recovery_attempts: u8) -> Self {
        Self {
            startup_attempts,
            recovery_attempts,
        }
    }
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self::new(2, 3)
    }
}

/// 运行期恢复触发原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    /// 设备丢失。
    DeviceLost,
    /// xrun/EPIPE。
    Xrun,
}

/// 状态机动作（由 I/O 线程/control 消费）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// 非设备级事件，调用方自行计数/处理。
    None,
    /// 恢复进行中：只能输出静音，按 stage 继续 `recovery_step`。
    Recovering {
        /// 下一个恢复阶段 index（0..3）。
        stage: u8,
        /// 已失败次数。
        attempt: u8,
    },
    /// 恢复成功：必须使用新 epoch。
    Recovered {
        /// 新 epoch id。
        epoch_id: u64,
    },
    /// 恢复重试耗尽：已进入 Muted，必须输出静音并等完整重新 admission。
    Exhausted,
    /// 不可恢复 fault。
    Faulted,
}

/// 一次 duplex 会话（设备 owner + 启动/恢复状态机）。
pub struct DuplexSession<B: PcmDuplexBackend> {
    backend: B,
    gate: HwOpenGate,
    policy: RecoveryPolicy,
    owner: DeviceOwner,
    request: DuplexRequest,
    state: DuplexState,
    actual: Option<DuplexActual>,
    epoch_id: u64,
    mute: bool,
    recovery_attempt: u8,
    recovery_stage: u8,
    last_error: Option<AlsaError>,
    transitions: [Transition; TRANSITION_LOG],
    transition_len: usize,
    dropped_transitions: u32,
}

impl<B: PcmDuplexBackend> DuplexSession<B> {
    /// 构造（gate 显式传入；fake backend 不受 gate 约束）。
    pub fn new(
        backend: B,
        request: DuplexRequest,
        policy: RecoveryPolicy,
        gate: HwOpenGate,
    ) -> Self {
        Self {
            backend,
            gate,
            policy,
            owner: DeviceOwner::new(),
            request,
            state: DuplexState::Closed,
            actual: None,
            epoch_id: 0,
            mute: false,
            recovery_attempt: 0,
            recovery_stage: 0,
            last_error: None,
            transitions: [Transition {
                from: DuplexState::Closed,
                to: DuplexState::Closed,
                kind: TransitionKind::Open,
                epoch_id: 0,
            }; TRANSITION_LOG],
            transition_len: 0,
            dropped_transitions: 0,
        }
    }

    /// 离线默认构造（gate=Denied，默认策略；fake backend 可用）。
    pub fn offline(backend: B, request: DuplexRequest) -> Self {
        Self::new(
            backend,
            request,
            RecoveryPolicy::default(),
            HwOpenGate::denied(),
        )
    }

    /// 当前状态。
    pub const fn state(&self) -> DuplexState {
        self.state
    }

    /// 当前 actual（Running/Recovering/Muted/Faulted 时存在）。
    pub const fn actual(&self) -> Option<DuplexActual> {
        self.actual
    }

    /// 当前 epoch（新 epoch 在启动成功/恢复成功时递增）。
    pub const fn epoch_id(&self) -> u64 {
        self.epoch_id
    }

    /// 是否必须输出静音（Recovering/Muted/Faulted）。
    pub const fn mute_required(&self) -> bool {
        self.mute
    }

    /// owner 状态。
    pub const fn owner_state(&self) -> OwnerState {
        self.owner.state()
    }

    /// 当前 owner。
    pub const fn owner_kind(&self) -> Option<OwnerKind> {
        self.owner.current()
    }

    /// owner 变更记录。
    pub fn owner_changes(&self) -> &[OwnerChange] {
        self.owner.changes()
    }

    /// backend 只读引用（证据/调用日志）。
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// backend 可变引用（测试注入故障；生产由 session 独占）。
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// 状态迁移记录。
    pub fn transitions(&self) -> &[Transition] {
        &self.transitions[..self.transition_len.min(TRANSITION_LOG)]
    }

    /// 溢出丢弃的迁移记录数。
    pub const fn dropped_transitions(&self) -> u32 {
        self.dropped_transitions
    }

    /// 最近一次错误。
    pub const fn last_error(&self) -> Option<AlsaError> {
        self.last_error
    }

    /// 已消耗的恢复失败次数。
    pub const fn recovery_attempt(&self) -> u8 {
        self.recovery_attempt
    }

    fn push(&mut self, from: DuplexState, to: DuplexState, kind: TransitionKind) {
        if let Some(slot) = self.transitions.get_mut(self.transition_len) {
            *slot = Transition {
                from,
                to,
                kind,
                epoch_id: self.epoch_id,
            };
            self.transition_len += 1;
        } else {
            self.dropped_transitions = self.dropped_transitions.saturating_add(1);
        }
    }

    fn set_state(&mut self, next: DuplexState) {
        self.state = next;
    }

    fn bump_epoch(&mut self) -> Result<u64, AlsaError> {
        let next = self.epoch_id.checked_add(1).ok_or(AlsaError::BadLayout)?;
        self.epoch_id = next;
        Ok(next)
    }

    /// 启动 duplex（解析稳定设备 → 打开 → 协商 → actual 校验 → 启流）。
    ///
    /// 允许从 `Closed`/`Muted`/`Faulted` 进入（后两者 = 完整重新准入的外部前置条件
    /// 由 T18 保证）；运行中重复启动 → ERR_SWITCH_BUSY。启动失败回滚 `Closed` 并释放
    /// owner；设备级瞬时错误按 `startup_attempts` 有界重试。
    pub fn start(
        &mut self,
        device: &DeviceSelector<'_>,
        cards_text: &str,
    ) -> Result<DuplexActual, AlsaError> {
        self.start_impl(device, cards_text, None)
    }

    /// 启动并校验设备宽度（T01 E1 Toslink ch12/13、4.1.2 role 最大 index）。
    pub fn start_with_width(
        &mut self,
        device: &DeviceSelector<'_>,
        cards_text: &str,
        required: &RequiredDeviceWidth,
    ) -> Result<DuplexActual, AlsaError> {
        self.start_impl(device, cards_text, Some(required))
    }

    fn start_impl(
        &mut self,
        device: &DeviceSelector<'_>,
        cards_text: &str,
        required: Option<&RequiredDeviceWidth>,
    ) -> Result<DuplexActual, AlsaError> {
        match self.state {
            DuplexState::Closed | DuplexState::Muted | DuplexState::Faulted => {}
            _ => return Err(AlsaError::SwitchBusy),
        }
        if self.state != DuplexState::Closed {
            self.reset_after_mute();
        }
        if self.backend.is_hardware() {
            self.gate.check()?;
        }
        let resolved = ResolvedDevice::from_cards(cards_text, device)?;
        if let Err(error) = self.owner.acquire(OwnerKind::Playback) {
            self.last_error = Some(error);
            return Err(error);
        }

        let attempts = self.policy.startup_attempts.max(1);
        let mut last_error = AlsaError::DeviceLost;
        for attempt in 0..attempts {
            if attempt > 0 {
                self.push(
                    DuplexState::Closed,
                    DuplexState::Opening,
                    TransitionKind::StartupRetry,
                );
            }
            match self.try_start_once(&resolved, required, attempt == 0) {
                Ok(actual) => {
                    self.last_error = None;
                    return Ok(actual);
                }
                Err(error) => {
                    last_error = error;
                    self.last_error = Some(error);
                    self.backend.close();
                    self.set_state(DuplexState::Closed);
                    if !error.startup_retryable() {
                        break;
                    }
                }
            }
        }
        self.push(
            DuplexState::Closed,
            DuplexState::Closed,
            TransitionKind::StartupFailed,
        );
        self.backend.close();
        let _ = self.owner.release();
        self.actual = None;
        self.set_state(DuplexState::Closed);
        self.mute = false;
        Err(last_error)
    }

    fn reset_after_mute(&mut self) {
        let from = self.state;
        self.backend.close();
        let _ = self.owner.release();
        self.actual = None;
        self.set_state(DuplexState::Closed);
        self.mute = false;
        self.push(from, DuplexState::Closed, TransitionKind::Reacquire);
    }

    fn try_start_once(
        &mut self,
        resolved: &ResolvedDevice,
        required: Option<&RequiredDeviceWidth>,
        first_attempt: bool,
    ) -> Result<DuplexActual, AlsaError> {
        if first_attempt {
            self.push(
                DuplexState::Closed,
                DuplexState::Opening,
                TransitionKind::Open,
            );
        }
        self.set_state(DuplexState::Opening);
        self.backend.open(resolved)?;

        self.set_state(DuplexState::Negotiating);
        let capture_caps = self.backend.stream_caps(StreamDirection::Capture)?;
        let playback_caps = self.backend.stream_caps(StreamDirection::Playback)?;
        let plan = negotiate(&capture_caps, &playback_caps, &self.request)?;
        self.push(
            DuplexState::Opening,
            DuplexState::Negotiating,
            TransitionKind::CapsOk,
        );
        self.push(
            DuplexState::Negotiating,
            DuplexState::Configuring,
            TransitionKind::Negotiated,
        );
        self.set_state(DuplexState::Configuring);

        let raw = self.backend.apply_hw_params(&plan)?;
        let actual = validate_actual(&plan, &raw, &self.request)?;
        if let Some(width) = required {
            actual.validate_width(width)?;
        }
        self.push(
            DuplexState::Configuring,
            DuplexState::Starting,
            TransitionKind::Configured,
        );
        self.set_state(DuplexState::Starting);

        self.backend.start()?;
        let _epoch = self.bump_epoch()?;
        self.actual = Some(actual);
        self.set_state(DuplexState::Running);
        self.mute = false;
        self.recovery_attempt = 0;
        self.recovery_stage = 0;
        self.push(
            DuplexState::Starting,
            DuplexState::Running,
            TransitionKind::StartOk,
        );
        Ok(actual)
    }

    /// 正常停流：drain → close → 释放 owner。非运行态返回 ERR_SWITCH_BUSY。
    pub fn stop(&mut self) -> Result<(), AlsaError> {
        match self.state {
            DuplexState::Running
            | DuplexState::Recovering
            | DuplexState::Muted
            | DuplexState::Faulted => {}
            _ => return Err(AlsaError::SwitchBusy),
        }
        let from = self.state;
        self.push(from, DuplexState::Closed, TransitionKind::StopRequested);
        let drain_result = self.backend.drain();
        self.backend.close();
        let _ = self.owner.release();
        self.actual = None;
        self.set_state(DuplexState::Closed);
        self.mute = false;
        self.recovery_attempt = 0;
        self.recovery_stage = 0;
        self.push(from, DuplexState::Closed, TransitionKind::Stopped);
        match drain_result {
            Err(error) => {
                self.last_error = Some(error);
                Err(error)
            }
            Ok(()) => Ok(()),
        }
    }

    fn ensure_running(&self) -> Result<(), AlsaError> {
        if self.state == DuplexState::Running {
            Ok(())
        } else {
            Err(AlsaError::SwitchBusy)
        }
    }

    /// [IO] 读一个 capture period；xrun/设备丢失自动进入 `Recovering`（静音）并返回错误。
    pub fn io_read(&mut self, dst: &mut [u8]) -> Result<usize, AlsaError> {
        self.ensure_running()?;
        match self.backend.read_period(dst) {
            Ok(read) => Ok(read),
            Err(error) => {
                self.last_error = Some(error);
                if error.device_recoverable() {
                    let _ = self.begin_recovery(recovery_reason(error));
                } else {
                    self.fault();
                }
                Err(error)
            }
        }
    }

    /// [IO] 写一个 playback period；xrun/设备丢失自动进入 `Recovering`（静音）并返回错误。
    pub fn io_write(&mut self, src: &[u8]) -> Result<usize, AlsaError> {
        self.ensure_running()?;
        match self.backend.write_period(src) {
            Ok(written) => Ok(written),
            Err(error) => {
                self.last_error = Some(error);
                if error.device_recoverable() {
                    let _ = self.begin_recovery(recovery_reason(error));
                } else {
                    self.fault();
                }
                Err(error)
            }
        }
    }

    /// 运行期进入恢复（幂等：Recovering 中重复调用只返回当前进度）。
    pub fn begin_recovery(&mut self, reason: RecoveryReason) -> Result<RecoveryAction, AlsaError> {
        match self.state {
            DuplexState::Running => {
                self.mute = true;
                self.recovery_attempt = 0;
                self.recovery_stage = 0;
                let kind = match reason {
                    RecoveryReason::DeviceLost => TransitionKind::IoDeviceLost,
                    RecoveryReason::Xrun => TransitionKind::IoXrun,
                };
                self.push(DuplexState::Running, DuplexState::Recovering, kind);
                self.set_state(DuplexState::Recovering);
                Ok(RecoveryAction::Recovering {
                    stage: 0,
                    attempt: 0,
                })
            }
            DuplexState::Recovering => Ok(RecoveryAction::Recovering {
                stage: self.recovery_stage,
                attempt: self.recovery_attempt,
            }),
            _ => Err(AlsaError::SwitchBusy),
        }
    }

    /// 推进一步恢复：Suspend → Prepare → Resume；成功强制新 epoch。
    pub fn recovery_step(&mut self) -> RecoveryAction {
        if self.state != DuplexState::Recovering {
            return match self.state {
                DuplexState::Muted => RecoveryAction::Exhausted,
                DuplexState::Faulted => RecoveryAction::Faulted,
                _ => RecoveryAction::None,
            };
        }
        let stage = RECOVER_STAGES
            .get(self.recovery_stage as usize)
            .copied()
            .unwrap_or(RecoverStage::Resume);
        match self.backend.recover(stage) {
            Ok(()) => {
                self.recovery_stage = self.recovery_stage.saturating_add(1);
                self.push(
                    DuplexState::Recovering,
                    DuplexState::Recovering,
                    TransitionKind::RecoverStageOk,
                );
                if self.recovery_stage as usize >= RECOVER_STAGES.len() {
                    match self.bump_epoch() {
                        Ok(epoch) => {
                            self.set_state(DuplexState::Running);
                            self.mute = false;
                            self.recovery_attempt = 0;
                            self.recovery_stage = 0;
                            self.last_error = None;
                            self.push(
                                DuplexState::Recovering,
                                DuplexState::Running,
                                TransitionKind::Recovered,
                            );
                            RecoveryAction::Recovered { epoch_id: epoch }
                        }
                        Err(error) => {
                            self.last_error = Some(error);
                            self.fault();
                            RecoveryAction::Faulted
                        }
                    }
                } else {
                    RecoveryAction::Recovering {
                        stage: self.recovery_stage,
                        attempt: self.recovery_attempt,
                    }
                }
            }
            Err(error) => {
                self.last_error = Some(error);
                self.recovery_attempt = self.recovery_attempt.saturating_add(1);
                if !error.device_recoverable()
                    || self.recovery_attempt >= self.policy.recovery_attempts.max(1)
                {
                    self.set_state(DuplexState::Muted);
                    self.mute = true;
                    self.push(
                        DuplexState::Recovering,
                        DuplexState::Muted,
                        TransitionKind::RecoveryExhausted,
                    );
                    RecoveryAction::Exhausted
                } else {
                    self.recovery_stage = 0;
                    self.push(
                        DuplexState::Recovering,
                        DuplexState::Recovering,
                        TransitionKind::RecoverAttemptFailed,
                    );
                    RecoveryAction::Recovering {
                        stage: 0,
                        attempt: self.recovery_attempt,
                    }
                }
            }
        }
    }

    /// core-v1 §9.2 错误码 → 状态机动作（I/O 线程/control 消费）。
    ///
    /// 407 RT_E_DEVICE_LOST → 恢复；2xx/3xx → None（调用方计数/fallback）；
    /// 其余 4xx/非 RT 码 → 防御性 Faulted（不得继续输出）。
    pub fn note_rt_status(&mut self, status: RtStatus) -> RecoveryAction {
        if status.is_ok() {
            return RecoveryAction::None;
        }
        let code = status.code();
        if code == RtStatus::RT_E_DEVICE_LOST.code() {
            return match self.begin_recovery(RecoveryReason::DeviceLost) {
                Ok(action) => action,
                Err(_) => RecoveryAction::None,
            };
        }
        if (201..=399).contains(&code) {
            return RecoveryAction::None;
        }
        self.fault();
        RecoveryAction::Faulted
    }

    /// 静音输出一个 playback period（Recovering/Muted/Faulted 期间唯一允许的写）。
    pub fn write_silence(&mut self) -> Result<usize, AlsaError> {
        if !self.mute {
            return Err(AlsaError::SwitchBusy);
        }
        let actual = match self.actual {
            Some(actual) => actual,
            None => return Err(AlsaError::SwitchBusy),
        };
        let bytes = actual.period_bytes(StreamDirection::Playback)?;
        if bytes > MAX_PERIOD_BYTES {
            return Err(AlsaError::BadLayout);
        }
        let zeros = [0u8; MAX_PERIOD_BYTES];
        match self.backend.write_period(&zeros[..bytes]) {
            Ok(written) => Ok(written),
            Err(error) => {
                self.last_error = Some(error);
                if !error.device_recoverable() {
                    self.fault();
                }
                Err(error)
            }
        }
    }

    /// [IO/启动] 预填 playback 缓冲（整 period 静音），给块循环留抗抖动 cushion。
    ///
    /// T17 D-07：playback 设备由首次 `writei` 自动启动（空 buffer 显式 `snd_pcm_start`
    /// 会立即 underrun）。但若起播后只写 1 个 period 就进入稳态块循环，sw 缓冲永远只有
    /// 1 period 余量：块循环任意 > period 的抖动（T45 worker park 唤醒、T22 格式化、
    /// 调度）都会让设备先跑空 → 写-恢复-写交替（v5 板窗实测 underruns == alsa.blocks-2）。
    /// 这里在起播/恢复后用静音把缓冲填到 `buffer_frames/period_frames - 2` 个 period
    /// （留 2 period 头 room 给稳态 writei 阻塞），设备仍由 `writei` 触发启动（D-07 不变）。
    ///
    /// 仅 Running 允许；设备级错误按 T17 既有语义进入 Recovering 并返回错误（fail closed
    /// 由调用方决定）。不计入 host block 计数/bytes_written（预填不是音频内容）。
    pub fn prime_playback_silence(&mut self) -> Result<usize, AlsaError> {
        self.ensure_running()?;
        let actual = self.actual.ok_or(AlsaError::BackendUnavailable)?;
        let period = actual.hw.period_frames.max(1) as u32;
        let buffer_periods = actual.hw.buffer_frames / period;
        if buffer_periods <= 2 {
            return Ok(0);
        }
        let cushion_periods = buffer_periods - 2;
        let bytes = actual.period_bytes(StreamDirection::Playback)?;
        if bytes == 0 || bytes > MAX_PERIOD_BYTES {
            return Err(AlsaError::BadLayout);
        }
        let zeros = [0u8; MAX_PERIOD_BYTES];
        let mut written_total = 0usize;
        let mut index = 0u32;
        while index < cushion_periods {
            match self.backend.write_period(&zeros[..bytes]) {
                Ok(written) => {
                    written_total = written_total.saturating_add(written);
                    index += 1;
                }
                Err(error) => {
                    self.last_error = Some(error);
                    if error.device_recoverable() {
                        let _ = self.begin_recovery(recovery_reason(error));
                    } else {
                        self.fault();
                    }
                    return Err(error);
                }
            }
        }
        Ok(written_total)
    }

    fn fault(&mut self) {
        let from = self.state;
        self.set_state(DuplexState::Faulted);
        self.mute = true;
        self.push(from, DuplexState::Faulted, TransitionKind::Fault);
    }

    /// 暂停播放并把设备交接给校准 daemon（§5.4：必须 Draining→Stopped 后释放设备）。
    ///
    /// 返回播放 actual hwparams hash（交接前证据）；调用方随后用 `calibration_opened`
    /// 登记校准 actual（交接后 hash）完成 EV_DEVICE_OWNER_CHANGE。
    pub fn suspend_for_calibration(
        &mut self,
        now_ns: u64,
        timeout_ns: u64,
    ) -> Result<u64, AlsaError> {
        if self.state != DuplexState::Running {
            return Err(AlsaError::SwitchBusy);
        }
        let before_hash = match self.actual {
            Some(actual) => actual.hash,
            None => return Err(AlsaError::SwitchBusy),
        };
        self.push(
            DuplexState::Running,
            DuplexState::Closed,
            TransitionKind::OwnerHandover,
        );
        let drain_result = self.backend.drain();
        self.backend.close();
        self.actual = None;
        self.set_state(DuplexState::Closed);
        self.mute = false;
        self.owner.begin_handover(now_ns, timeout_ns, before_hash)?;
        if let Err(error) = drain_result {
            self.last_error = Some(error);
        }
        Ok(before_hash)
    }

    /// 登记校准 daemon 打开设备后的 actual hwparams（完成 owner 交接）。
    pub fn calibration_opened(
        &mut self,
        now_ns: u64,
        calibration_hw: &KhaenHwParamsV1,
    ) -> Result<OwnerChange, AlsaError> {
        if self.owner.state() != OwnerState::Transition {
            return Err(AlsaError::SwitchBusy);
        }
        let after_hash = hwparams_hash(calibration_hw);
        match self.owner.complete_handover(now_ns, after_hash) {
            Ok(change) => {
                self.push(
                    DuplexState::Closed,
                    DuplexState::Closed,
                    TransitionKind::CalibrationOpened,
                );
                Ok(change)
            }
            Err(error) => {
                self.last_error = Some(error);
                if error == AlsaError::OwnerTransitionTimeout {
                    self.set_state(DuplexState::Muted);
                    self.mute = true;
                    self.push(
                        DuplexState::Closed,
                        DuplexState::Muted,
                        TransitionKind::Fault,
                    );
                }
                Err(error)
            }
        }
    }

    /// 校准结束：释放 calibration owner 并重新启动播放（新 epoch）。
    pub fn resume_playback(
        &mut self,
        device: &DeviceSelector<'_>,
        cards_text: &str,
    ) -> Result<DuplexActual, AlsaError> {
        if self.owner.current() != Some(OwnerKind::Calibration) {
            return Err(AlsaError::SwitchBusy);
        }
        if let Err(error) = self.owner.release() {
            self.last_error = Some(error);
            return Err(error);
        }
        self.start(device, cards_text)
    }
}

/// 错误 → 恢复原因映射。
fn recovery_reason(error: AlsaError) -> RecoveryReason {
    match error {
        AlsaError::Xrun => RecoveryReason::Xrun,
        _ => RecoveryReason::DeviceLost,
    }
}

// 编译期静态断言（§11.9 / §3.1 / §3.2；断言在 RT 区外执行）。
const _: () = {
    assert!(MAX_PERIOD_BYTES == 11_264);
    assert!(TRANSITION_LOG > 0 && OWNER_CHANGE_LOG > 0 && FAKE_CALL_LOG > 0);
    assert!(is_runtime_period(64) && is_runtime_period(128));
    assert!(!is_runtime_period(256));
    assert!(is_runtime_rate(48_000) && is_runtime_rate(96_000));
    assert!(!is_runtime_rate(44_100));
    assert!(matches!(
        sample_bytes(sample_format::S24_3LE_INTERLEAVED),
        Some(3)
    ));
    assert!(matches!(
        sample_bytes(sample_format::S32_LE_INTERLEAVED),
        Some(4)
    ));
    assert!(sample_bytes(sample_format::F32_PLANAR).is_none());
    assert!(EV_DEVICE_OWNER_CHANGE == 31);
};

#[cfg(test)]
mod alsa_fake_tests {
    include!("../tests/T17/alsa_fake.rs");
}
