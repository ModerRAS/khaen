//! [NONRT]/[IO] AES67/Inferno 端点接入（core-v1 §5.4 / §6.4 / §9.2；compat A12–A18/K42–K47/DEC §6）——WBS T43 写域。
//!
//! 本模块把**已有**的 AoIP 接收端接进新 host 的 capture 路径，**不重写任何协议**：
//!
//! - **AES67 端点**：复用已有 aes67-daemon（RAVENNA/AES67，PTPv2 slave，B1 默认）
//!   与 MergingRavennaALSA.ko 暴露的 ALSA capture 卡。本模块只做端点选择/校验：
//!   从 /proc/asound/cards 文本按稳定 card id（RAVENNA，可用 KHAEN_RAVENNA_CARD_ID
//!   覆盖）解析出 hw:<card>,0（复用 T17 alsa::ResolvedDevice/find_card_index，
//!   禁止写死 card 号）；解析已有 daemon/SAP 交付的 **SDP** 文本（只读解析，不生成、
//!   不发送 RTSP/SAP/RTP，不实现 RTP 协议栈），校验 AES67 媒体面：L24/L16 @48k、
//!   ptime ≤1 ms、每 flow ≤8 通道、组播地址、ts-refclk PTPv2 时间基准。
//! - **Inferno/Dante 端点**：复用已有 inferno ALSA 插件（pcm.inferno, S32_LE、
//!   96k、16ch 容器）或 inferno2pipe 管道端点（native i32 交错 = little-endian
//!   S32_LE；本模块只校验帧长/帧数，不解析 Inferno 协议）。PTPv1 虚拟钟由已有
//!   statime（/tmp/ptp-usrvclock）提供，本模块只消费 T19 的 LockState/唯一
//!   servo owner 语义，不实现 PTP。
//! - **H16 逻辑 12 / 容器 16 分开**（DEC §1、compat A13/K46、docs/h16-aip-research.md C6/C8）：
//!   H16 的 16 TX 容器顺序固定为 1 L / 2 R / 3 C / 4 LFE / 5 Ls / 6 Rs / 7 Lrs / 8 Rrs /
//!   9 Lw / 10 Rw / 11 Ltf / 12 Rtf / 13 空 / 14 空 / 15 Ltr / 16 Rtr；7.1.4 实际有内容
//!   只有 12 路（1–8 + 11/12 + 15/16），9/10/13/14 在 7.1.4 下静音。接收端可用两条
//!   ≤8 通道 flow 组成 12ch（8+4）或 16ch（8+8）容器；SDP 只给每 flow 通道数/编码/
//!   速率，不给角色/顺序（C8），因此 **flow→H16 TX 通道映射必须显式配置**
//!   （H16FlowConfig），角色选择必须显式声明（plan_role_selection），未选择的
//!   已收逻辑通道必须显式 ignore，禁止静默下混/静默丢弃；目标 layout 缺少的接收角色
//!   必须拒收（不静默零填）。
//! - **format/rate 互斥**（compat A17/K45）：AES67 = 48k（SDP L24 线上 24bit）+ 设备
//!   S32_LE 容器；Inferno/Dante = 96k + S32_LE；两者不混用；period 只允许 64/128
//!   （core-v1 §3.2/§6.1 I11，复用 T17 alsa::is_runtime_period）。
//! - **失流 fake 模型**（兼容 A19/K54、WBS T43 DoD）：StreamMonitor 按实际 period
//!   的 T_block 门限检测“首包未到/停滞/持续失流”：短停滞 → 整块静音（Muted，
//!   同 epoch 可在数据恢复后继续）；持续失流/PTP 失锁/管道 EOF → Lost，调用方必须
//!   新 epoch 并经 reopen 才可恢复（禁止半块输出、禁止拿旧 epoch 历史拼接）。
//!   全部用脚本化 nanos/帧数驱动，**不打开硬件、不启动服务、不连板端**。
//! - **PTP 唯一来源**（compat A15/K44、DEC §5/§6）：AoipProtocol::Aes67 只接受
//!   aes67-daemon 内建 PTPv2 slave；Inferno 只接受 statime PTPv1 虚拟钟；
//!   PTP 未锁（LockState::Unlocked/Locking/Holdover）不得开始采集
//!   （ERR_ADMISSION_CLOCK）。PTP 锁 **不等同** USB/MOTU 同钟：本模块不建立
//!   第二个 clock servo，也不复制 ppm（servo 归 T19/ClockRuntime）。
//!
//! ## 错误映射（core-v1 §9.2 冻结码；不新增码）
//!
//! | AoipError | RtStatus | 语义/动作 |
//! |---|---|---|
//! | Rate | 104 ERR_RATE | 非 48k（AES67）/96k（Inferno）或 SDP 速率不符 |
//! | BlockSize | 105 ERR_BLOCK_SIZE | period 非 64/128 |
//! | Channels | 106 ERR_CHANNELS | 容器/flow 通道数与设备/描述不符 |
//! | Roles | 107 ERR_ROLES | 角色选择/非逻辑通道/未显式 ignore 的映射拒收 |
//! | BadLayout | 103 ERR_BAD_LAYOUT | 结构/数组/算术越界（含 epoch 0） |
//! | DeviceParams | 118 ERR_DEVICE_PARAMS | 稳定设备/端点在配置中不可唯一定位；SDP 畸形 |
//! | DeviceBusy | 117 ERR_DEVICE_BUSY | 同一端点重复 owner/重复开始 |
//! | SwitchBusy | 120 ERR_SWITCH_BUSY | 端点状态机非法迁移 |
//! | EpochMismatch | 204 RT_E_EPOCH_MISMATCH | 失流后未换新 epoch / 旧 epoch 恢复 |
//! | PtpUnavailable | 116 ERR_ADMISSION_CLOCK | PTP 未锁/来源不符（不得进 RT 主模式） |
//! | Unsupported | 122 ERR_UNSUPPORTED | 协议/媒体面/SDP 属性不可接受（含 BE pipe） |
//! | StreamLost | 407 RT_E_DEVICE_LOST | AoIP 流/端点未就绪或已丢失（静音+恢复状态机） |
//!
//! 控制面 A03 退出码（含 6=“AoIP 流未就绪”）由 T42/T44 映射；本层只返回冻结 RT 码。
//!
//! ## 真实性与纪律
//!
//! - 本文件是 [NONRT]/[IO] 模块（不是 RT-MODULE）：允许启动/恢复期分配（本实现不分配）、
//!   允许 IO 侧 syscall（本实现不含任何 syscall/FFI/文件/进程操作）；**不得**进入 DSP
//!   worker 调用图。稳态热路径（StreamMonitor::poll/on_data、plan_role_selection）
//!   只用固定数组与 Copy 值。
//! - S1 运行期：不打开 ALSA/Pipe、不启动/停止 aes67-daemon/statime、不改 CueMix、
//!   不连板端；真实端点接入归 T26 启动期（复用 T17 硬件闸门 HwOpenGate）。
//! - 未测项：H16 实机（未到货）、aes67-daemon（板端缺 5 库）、statime（板端 panic
//!   could not get clock identity）→ 真实 AES67/Dante 端到端 **NOT_VERIFIED/INCOMPLETE**；
//!   本模块只交付离线端点模型与 fake 失流语义。
//!
//! 来源：spec/compat-matrix.md A12–A18/K42–K47、spec/core-v1.md §5.4/§6.4/§9.2、
//! scratch/lead/planner/DSP-HOST-DECISION.md §6、docs/h16-aip-research.md C4/C6/C7/C8、
//! docs/h16-2ch-in-714.md §5、audio/overlay/etc/{aes67-daemon/daemon.conf,asound.conf,
//! statime/inferno-ptpv1.toml,camilladsp/aes67.yml,camilladsp/dante.yml}、
//! audio/overlay/usr/local/bin/khaen-mode（dante: inferno/S32_LE/96k；aes67:
//! hw:RAVENNA,0/S32_LE/48k）、rt-stack/evidence/baseline/board-evidence/
//! aoip-halfcheck-REPORT-20261116-1949.md、scratch/lead/inferno/inferno2pipe
//! （native i32 交错写出）。

use khaen_core::abi::sample_format;
use khaen_core::rt::{t_block_ns_ceil, RtStatus, BLOCK_SIZES, SAMPLE_RATES_HZ};

use crate::alsa::{self, is_runtime_period, is_runtime_rate, AlsaError, ResolvedDevice};
use crate::clock::{DiscontinuityReason, LockState};

// ---------------------------------------------------------------------------
// 常量（来源见模块头；不改 core-v1 冻结语义）
// ---------------------------------------------------------------------------

/// AES67 媒体面固定采样率（H16 AES67 模式固定 48k；daemon.conf sample_rate: 48000）。
pub const AES67_RATE_HZ: u32 = 48_000;
/// Inferno/Dante 媒体面采样率（A14/K42：96k；inferno2pipe 默认 48k，Dante 96k 需
/// SAMPLE_RATE=96000，本条只描述已部署 profile）。
pub const INFERNO_RATE_HZ: u32 = 96_000;
/// H16 的逻辑声道数（7.1.4：FL/FR/C/LFE/SL/SR/BL/BR/TFL/TFR/TRL/TRR）。
pub const H16_LOGICAL_CHANNELS: u16 = 12;
/// H16 的 TX 容器通道数（16ch）。
pub const H16_CONTAINER_CHANNELS: u16 = 16;
/// 每条 AES67/Dante multicast flow 的最大通道数（A13/K46）。
pub const AES67_MAX_CHANNELS_PER_FLOW: u16 = 8;
/// H16 最多接受的 flow 数（12ch 需 2 条；16ch 需 2 条；本模型最多 4 条以便扩展）。
pub const H16_MAX_FLOWS: usize = 4;
/// 单条 flow 最多通道数（固定数组容量）。
pub const H16_MAX_FLOW_CHANNELS: usize = 8;
/// AES67 媒体面 ptime 上限（µs）：≤1 ms（AES67 media profile）。
pub const AES67_PTIME_MAX_US: u32 = 1_000;
/// AES67 默认 PTPv2 domain（daemon.conf ptp_domain: 0）。
pub const AES67_PTP_DOMAIN_DEFAULT: u8 = 0;
/// AES67 默认 RTP 端口（daemon.conf rtp_port: 5004；仅作 profile 参考，不写死校验）。
pub const AES67_RTP_PORT_DEFAULT: u16 = 5_004;
/// RAVENNA ALSA 卡的稳定 card id 默认值（khaen-mode 用 KHAEN_RAVENNA_CARD_ID 覆盖）。
pub const AES67_CARD_ID_DEFAULT: &str = "RAVENNA";
/// Inferno ALSA 插件 PCM 名（/etc/asound.conf 定义 pcm.inferno；khaen-mode dante 模式）。
pub const INFERNO_ALSA_DEVICE: &str = "inferno";
/// Inferno PTPv1 虚拟钟导出路径（statime usrvclock-path 默认值）。
pub const INFERNO_CLOCK_PATH_DEFAULT: &str = "/tmp/ptp-usrvclock";
/// Inferno PTPv1 device id 文本长度（64bit MAC 派生 + 0000，16 个 hex 字符）。
pub const INFERNO_DEVICE_ID_HEX_LEN: usize = 16;
/// 默认 period（与 T17 一致；B 来源链 = actual period）。
pub const DEFAULT_PERIOD_FRAMES: u32 = 128;
/// 默认 buffer（T01 E1：MOTU chunk256 → actual 128/1024；AoIP 侧同初值）。
pub const DEFAULT_BUFFER_FRAMES: u32 = 1_024;
/// 失流停滞门限（period 数）：连续 N 个 block 无数据 → 整块静音（Muted）。
pub const DEFAULT_MAX_STALL_PERIODS: u32 = 3;
/// 持续失流倍数：停滞超过 2 × N 个 block → Lost（需新 epoch）。
pub const STREAM_LOST_MULTIPLIER: u32 = 2;
/// pipe 单次读取的最大字节数（128 帧 × 16 通道 × 4 B）。
pub const MAX_PIPE_CHUNK_BYTES: usize = 128 * H16_CONTAINER_CHANNELS as usize * 4;
/// H16 逻辑角色在 16ch 容器中的 slot（0-based；= 官方 TX 通道号 − 1；见模块头 C6）。
pub const H16_LOGICAL_SLOTS: [u8; H16_LOGICAL_CHANNELS as usize] =
    [0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 14, 15];
/// H16 16ch 容器中在 7.1.4 下无逻辑角色的 slot（0-based 8/9/12/13 = Lw/Rw/空/空）。
pub const H16_NON_LOGICAL_SLOTS: [u8; 4] = [8, 9, 12, 13];

const H16_ROLE_COUNT: usize = H16_LOGICAL_CHANNELS as usize;
const H16_CONTAINER_LEN: usize = H16_CONTAINER_CHANNELS as usize;

// ---------------------------------------------------------------------------
// 错误（映射 core-v1 §9.2 冻结码；不新增码）
// ---------------------------------------------------------------------------

/// T43 端点层错误（Copy；非 RT 边界可用，映射到冻结 RtStatus）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AoipError {
    /// 采样率不符合协议 profile / SDP（104）。
    Rate,
    /// period 非 64/128（105）。
    BlockSize,
    /// 通道数不符（106）。
    Channels,
    /// 角色选择/映射不可接受（107）。
    Roles,
    /// 结构/数组/算术错误（103）。
    BadLayout,
    /// 设备/SDP/端点参数不满足（118）。
    DeviceParams,
    /// 端点 owner 冲突/重复开始（117）。
    DeviceBusy,
    /// 端点状态机非法迁移（120）。
    SwitchBusy,
    /// 失流后未换新 epoch / 旧 epoch 恢复（204）。
    EpochMismatch,
    /// PTP 未锁或 PTP 来源与协议不符（116；不得进 RT 主模式）。
    PtpUnavailable,
    /// 协议/媒体面/SDP 属性/目标不可接受（122）。
    Unsupported,
    /// AoIP 流/端点未就绪或丢失（407；静音 + 恢复状态机）。
    StreamLost,
}

impl AoipError {
    /// 映射到 core-v1 §9.2 冻结错误码。
    pub const fn status(self) -> RtStatus {
        match self {
            AoipError::Rate => RtStatus::ERR_RATE,
            AoipError::BlockSize => RtStatus::ERR_BLOCK_SIZE,
            AoipError::Channels => RtStatus::ERR_CHANNELS,
            AoipError::Roles => RtStatus::ERR_ROLES,
            AoipError::BadLayout => RtStatus::ERR_BAD_LAYOUT,
            AoipError::DeviceParams => RtStatus::ERR_DEVICE_PARAMS,
            AoipError::DeviceBusy => RtStatus::ERR_DEVICE_BUSY,
            AoipError::SwitchBusy => RtStatus::ERR_SWITCH_BUSY,
            AoipError::EpochMismatch => RtStatus::RT_E_EPOCH_MISMATCH,
            AoipError::PtpUnavailable => RtStatus::ERR_ADMISSION_CLOCK,
            AoipError::Unsupported => RtStatus::ERR_UNSUPPORTED,
            AoipError::StreamLost => RtStatus::RT_E_DEVICE_LOST,
        }
    }

    /// 稳定名称（证据/日志用；非 RT 格式化）。
    pub const fn name(self) -> &'static str {
        match self {
            AoipError::Rate => "ERR_RATE",
            AoipError::BlockSize => "ERR_BLOCK_SIZE",
            AoipError::Channels => "ERR_CHANNELS",
            AoipError::Roles => "ERR_ROLES",
            AoipError::BadLayout => "ERR_BAD_LAYOUT",
            AoipError::DeviceParams => "ERR_DEVICE_PARAMS",
            AoipError::DeviceBusy => "ERR_DEVICE_BUSY",
            AoipError::SwitchBusy => "ERR_SWITCH_BUSY",
            AoipError::EpochMismatch => "RT_E_EPOCH_MISMATCH",
            AoipError::PtpUnavailable => "ERR_ADMISSION_CLOCK",
            AoipError::Unsupported => "ERR_UNSUPPORTED",
            AoipError::StreamLost => "RT_E_DEVICE_LOST",
        }
    }

    /// 是否可通过等待/重试恢复（PTP 未锁、端点未就绪）——仅提示，状态机仍由调用方驱动。
    pub const fn retryable(self) -> bool {
        matches!(self, AoipError::PtpUnavailable | AoipError::StreamLost)
    }
}

impl From<AlsaError> for AoipError {
    fn from(error: AlsaError) -> Self {
        match error {
            AlsaError::Rate => AoipError::Rate,
            AlsaError::BlockSize => AoipError::BlockSize,
            AlsaError::Channels => AoipError::Channels,
            AlsaError::StrideAlign | AlsaError::BadLayout => AoipError::BadLayout,
            AlsaError::DeviceParams => AoipError::DeviceParams,
            AlsaError::DeviceBusy | AlsaError::OwnerTransitionTimeout => AoipError::DeviceBusy,
            AlsaError::SwitchBusy => AoipError::SwitchBusy,
            AlsaError::CardNotFound | AlsaError::CardAmbiguous => AoipError::DeviceParams,
            AlsaError::Unsupported | AlsaError::BackendUnavailable | AlsaError::HwOpenDenied => {
                AoipError::Unsupported
            }
            AlsaError::Xrun | AlsaError::DeviceLost | AlsaError::RecoveryExhausted => {
                AoipError::StreamLost
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 协议 / 传输 / PTP 来源（A14/A15/K42/K43/K44）
// ---------------------------------------------------------------------------

/// AoIP 协议族（已有端点，不重写协议）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AoipProtocol {
    /// AES67/RAVENNA：已有 aes67-daemon + MergingRavennaALSA.ko；48k、线上 L24。
    Aes67,
    /// Inferno/Dante：已有 inferno ALSA 插件 / inferno2pipe；96k、S32_LE。
    Inferno,
}

impl AoipProtocol {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            AoipProtocol::Aes67 => "aes67",
            AoipProtocol::Inferno => "inferno",
        }
    }

    /// 协议 profile 采样率（AES67 48k / Inferno 96k）。
    pub const fn profile_rate_hz(self) -> u32 {
        match self {
            AoipProtocol::Aes67 => AES67_RATE_HZ,
            AoipProtocol::Inferno => INFERNO_RATE_HZ,
        }
    }

    /// 设备侧容器格式：两条既有链路都以 S32_LE 交付（camilladsp 模板 cap_fmt="S32_LE"）。
    pub const fn device_format(self) -> u16 {
        sample_format::S32_LE_INTERLEAVED
    }

    /// 该协议唯一允许的 PTP 来源（A15/K44：每 mode 唯一 PTP 来源，不得双闭环）。
    pub const fn expected_ptp_source(self) -> PtpSource {
        match self {
            AoipProtocol::Aes67 => PtpSource::Aes67DaemonBuiltinPtpV2,
            AoipProtocol::Inferno => PtpSource::StatimePtpV1VirtualClock,
        }
    }
}

/// 端点传输方式（AES67/Inferno 均为“已有端点”，新 host 只接 ALSA 或 pipe）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AoipTransport {
    /// 已有 ALSA PCM（AES67：RAVENNA LKM hw:<card>,0；Inferno：inferno 插件）。
    Alsa,
    /// 已有 pipe/raw 端点（inferno2pipe 输出；native i32 交错 = LE S32_LE）。
    Pipe,
}

impl AoipTransport {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            AoipTransport::Alsa => "alsa",
            AoipTransport::Pipe => "pipe",
        }
    }
}

/// PTP 时钟来源（compat A15/K44；PTP 锁 ≠ USB/MOTU 同钟）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PtpSource {
    /// aes67-daemon 内建 PTPv2 slave（B1 默认；B2/B3 才用 ptp4l/phc2sys，互斥）。
    Aes67DaemonBuiltinPtpV2,
    /// statime PTPv1 follower + virtual system clock → usrvclock → Inferno。
    StatimePtpV1VirtualClock,
}

impl PtpSource {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            PtpSource::Aes67DaemonBuiltinPtpV2 => "aes67-daemon-ptpv2-slave",
            PtpSource::StatimePtpV1VirtualClock => "statime-ptpv1-virtual-clock",
        }
    }
}

// ---------------------------------------------------------------------------
// H16 逻辑 12 / 容器 16：角色、flow 配置、容器映射（DEC §1、compat A13/K46）
// ---------------------------------------------------------------------------

/// H16 7.1.4 的 12 个逻辑角色（顺序 = 逻辑顺序，不等于容器顺序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum H16Role {
    /// Front Left（容器 slot 0 / TX1）。
    FrontLeft,
    /// Front Right（slot 1 / TX2）。
    FrontRight,
    /// Center（slot 2 / TX3）。
    Center,
    /// LFE（slot 3 / TX4）。
    Lfe,
    /// Surround Left（slot 4 / TX5 = Ls）。
    SurroundLeft,
    /// Surround Right（slot 5 / TX6 = Rs）。
    SurroundRight,
    /// Back Left（slot 6 / TX7 = Lrs）。
    BackLeft,
    /// Back Right（slot 7 / TX8 = Rrs）。
    BackRight,
    /// Top Front Left（slot 10 / TX11 = Ltf）。
    TopFrontLeft,
    /// Top Front Right（slot 11 / TX12 = Rtf）。
    TopFrontRight,
    /// Top Rear Left（slot 14 / TX15 = Ltr）。
    TopRearLeft,
    /// Top Rear Right（slot 15 / TX16 = Rtr）。
    TopRearRight,
}

impl H16Role {
    /// 全部 12 个逻辑角色（逻辑顺序）。
    pub const ALL: [H16Role; H16_ROLE_COUNT] = [
        H16Role::FrontLeft,
        H16Role::FrontRight,
        H16Role::Center,
        H16Role::Lfe,
        H16Role::SurroundLeft,
        H16Role::SurroundRight,
        H16Role::BackLeft,
        H16Role::BackRight,
        H16Role::TopFrontLeft,
        H16Role::TopFrontRight,
        H16Role::TopRearLeft,
        H16Role::TopRearRight,
    ];

    /// 逻辑角色序号（0..11，用于位图/数组下标）。
    pub const fn index(self) -> usize {
        match self {
            H16Role::FrontLeft => 0,
            H16Role::FrontRight => 1,
            H16Role::Center => 2,
            H16Role::Lfe => 3,
            H16Role::SurroundLeft => 4,
            H16Role::SurroundRight => 5,
            H16Role::BackLeft => 6,
            H16Role::BackRight => 7,
            H16Role::TopFrontLeft => 8,
            H16Role::TopFrontRight => 9,
            H16Role::TopRearLeft => 10,
            H16Role::TopRearRight => 11,
        }
    }

    /// H16 16ch 容器 slot（0-based；官方 TX 通道号 − 1）。
    pub const fn container_slot(self) -> u8 {
        H16_LOGICAL_SLOTS[self.index()]
    }

    /// 机器名（证据/日志用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            H16Role::FrontLeft => "FL",
            H16Role::FrontRight => "FR",
            H16Role::Center => "C",
            H16Role::Lfe => "LFE",
            H16Role::SurroundLeft => "SL",
            H16Role::SurroundRight => "SR",
            H16Role::BackLeft => "BL",
            H16Role::BackRight => "BR",
            H16Role::TopFrontLeft => "TFL",
            H16Role::TopFrontRight => "TFR",
            H16Role::TopRearLeft => "TRL",
            H16Role::TopRearRight => "TRR",
        }
    }

    /// 由容器 slot 反查逻辑角色（非逻辑 slot 返回 None；禁止把 9/10/13/14 当逻辑角色）。
    pub const fn from_container_slot(slot: u8) -> Option<H16Role> {
        match slot {
            0 => Some(H16Role::FrontLeft),
            1 => Some(H16Role::FrontRight),
            2 => Some(H16Role::Center),
            3 => Some(H16Role::Lfe),
            4 => Some(H16Role::SurroundLeft),
            5 => Some(H16Role::SurroundRight),
            6 => Some(H16Role::BackLeft),
            7 => Some(H16Role::BackRight),
            10 => Some(H16Role::TopFrontLeft),
            11 => Some(H16Role::TopFrontRight),
            14 => Some(H16Role::TopRearLeft),
            15 => Some(H16Role::TopRearRight),
            _ => None,
        }
    }

    /// 该 slot 是否在 7.1.4 下有逻辑角色。
    pub const fn slot_is_logical(slot: u8) -> bool {
        H16Role::from_container_slot(slot).is_some()
    }
}

/// 已配置的 H16 flow 集合：每条 flow 的 H16 TX 容器 slot（0-based）连续列表。
///
/// 这是**显式**的 flow→容器映射：SDP 只给每 flow 通道数（C8），真实 TX 通道构成
/// 必须来自 AudioGridController/Dante Controller 的静态配置。未列出的 TX 通道
/// 视为未订阅（显式排除），不会静默进入容器。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H16FlowConfig {
    flows: [[u8; H16_MAX_FLOW_CHANNELS]; H16_MAX_FLOWS],
    lens: [u8; H16_MAX_FLOWS],
    count: u8,
}

impl H16FlowConfig {
    /// 7.1.4 12 路两条 flow：TX1–8 + TX11/12/15/16 → 接收端 8+4 = 12ch 容器。
    pub const fn logical12() -> Self {
        let mut flows = [[0u8; H16_MAX_FLOW_CHANNELS]; H16_MAX_FLOWS];
        flows[0] = [0, 1, 2, 3, 4, 5, 6, 7];
        flows[1] = [10, 11, 14, 15, 0, 0, 0, 0];
        Self {
            flows,
            lens: [8, 4, 0, 0],
            count: 2,
        }
    }

    /// 16ch 两条 flow：TX1–8 + TX9–16 → 接收端 16ch 容器（9/10/13/14 在 7.1.4 下静音）。
    pub const fn container16() -> Self {
        let mut flows = [[0u8; H16_MAX_FLOW_CHANNELS]; H16_MAX_FLOWS];
        flows[0] = [0, 1, 2, 3, 4, 5, 6, 7];
        flows[1] = [8, 9, 10, 11, 12, 13, 14, 15];
        Self {
            flows,
            lens: [8, 8, 0, 0],
            count: 2,
        }
    }

    /// 由显式 slot 列表构造（每条 ≤8 通道，slot < 16，禁止重复）。
    pub fn try_from_slots(flows: &[&[u8]]) -> Result<Self, AoipError> {
        if flows.is_empty() || flows.len() > H16_MAX_FLOWS {
            return Err(AoipError::DeviceParams);
        }
        let mut config = Self {
            flows: [[0u8; H16_MAX_FLOW_CHANNELS]; H16_MAX_FLOWS],
            lens: [0u8; H16_MAX_FLOWS],
            count: flows.len() as u8,
        };
        for (flow_index, slots) in flows.iter().enumerate() {
            if slots.is_empty() || slots.len() > H16_MAX_FLOW_CHANNELS {
                return Err(AoipError::DeviceParams);
            }
            for (slot_index, &slot) in slots.iter().enumerate() {
                if slot as u16 >= H16_CONTAINER_CHANNELS {
                    return Err(AoipError::BadLayout);
                }
                for previous in &config.flows[..flow_index] {
                    if previous[..].contains(&slot) {
                        return Err(AoipError::DeviceParams);
                    }
                }
                if config.flows[flow_index][..slot_index].contains(&slot) {
                    return Err(AoipError::DeviceParams);
                }
                config.flows[flow_index][slot_index] = slot;
            }
            config.lens[flow_index] = slots.len() as u8;
        }
        Ok(config)
    }

    /// flow 数。
    pub const fn flow_count(&self) -> usize {
        self.count as usize
    }

    /// 第 index 条 flow 的通道数。
    pub const fn flow_len(&self, index: usize) -> Option<u8> {
        if index < self.count as usize {
            Some(self.lens[index])
        } else {
            None
        }
    }

    /// 第 index 条 flow 的 slot 切片（0-based H16 TX 容器 slot）。
    pub fn flow_slots(&self, index: usize) -> Option<&[u8]> {
        if index < self.count as usize {
            let len = self.lens[index] as usize;
            Some(&self.flows[index][..len])
        } else {
            None
        }
    }

    /// 所有 flow 的通道总数（= 接收端容器通道数）。
    pub fn total_channels(&self) -> u16 {
        let mut total = 0u16;
        for index in 0..self.count as usize {
            total = total.saturating_add(self.lens[index] as u16);
        }
        total
    }

    /// 校验内部一致性（构造后恒成立；测试/外部反序列化后调用）。
    pub fn validate(&self) -> Result<(), AoipError> {
        let count = self.count as usize;
        if count == 0 || count > H16_MAX_FLOWS {
            return Err(AoipError::DeviceParams);
        }
        let mut seen = 0u32;
        let mut total = 0u16;
        for flow_index in 0..count {
            let len = self.lens[flow_index] as usize;
            if len == 0 || len > H16_MAX_FLOW_CHANNELS {
                return Err(AoipError::DeviceParams);
            }
            for slot_index in 0..len {
                let slot = self.flows[flow_index][slot_index];
                if slot as u16 >= H16_CONTAINER_CHANNELS {
                    return Err(AoipError::BadLayout);
                }
                let bit = 1u32 << slot;
                if seen & bit != 0 {
                    return Err(AoipError::DeviceParams);
                }
                seen |= bit;
            }
            total = total.saturating_add(len as u16);
        }
        if total == 0 || total > H16_CONTAINER_CHANNELS {
            return Err(AoipError::Channels);
        }
        Ok(())
    }
}

/// 接收端 H16 容器映射：capture 通道 index → H16 容器 slot → 逻辑角色（或非逻辑）。
///
/// 由 H16FlowConfig 按 flow 顺序拼接得到；本类型只描述**容器**（哪些通道有内容、
/// 哪些 slot 无逻辑角色），不判断节目布局/内容（那属 calibration/content 域）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H16Container {
    slots: [u8; H16_CONTAINER_LEN],
    roles: [i8; H16_CONTAINER_LEN],
    capture_channels: u16,
    flow_count: u8,
    logical_present: u16,
    non_logical_mask: u16,
}

impl H16Container {
    /// 7.1.4 12 路两条 flow（8+4）→ 12ch 容器（全部有逻辑角色）。
    pub fn logical12() -> Self {
        Self::from_flow_config(&H16FlowConfig::logical12())
            .expect("H16 logical12 flow config is valid")
    }

    /// 16ch 两条 flow（8+8）→ 16ch 容器（capture 8/9/12/13 非逻辑）。
    pub fn container16() -> Self {
        Self::from_flow_config(&H16FlowConfig::container16())
            .expect("H16 container16 flow config is valid")
    }

    /// 由显式 flow 配置构造容器映射（拒绝重复 slot/非法 slot）。
    pub fn from_flow_config(config: &H16FlowConfig) -> Result<Self, AoipError> {
        config.validate()?;
        let mut container = Self {
            slots: [0u8; H16_CONTAINER_LEN],
            roles: [-1i8; H16_CONTAINER_LEN],
            capture_channels: 0,
            flow_count: config.count,
            logical_present: 0,
            non_logical_mask: 0,
        };
        let mut capture = 0usize;
        for flow_index in 0..config.count as usize {
            let len = config.lens[flow_index] as usize;
            for slot_index in 0..len {
                let slot = config.flows[flow_index][slot_index];
                container.slots[capture] = slot;
                match H16Role::from_container_slot(slot) {
                    Some(role) => {
                        container.roles[capture] = role.index() as i8;
                        container.logical_present |= 1u16 << role.index();
                    }
                    None => {
                        container.roles[capture] = -1;
                        container.non_logical_mask |= 1u16 << capture;
                    }
                }
                capture += 1;
            }
        }
        container.capture_channels = capture as u16;
        Ok(container)
    }

    /// 接收端容器通道数（12 或 16；由 flow 组成决定）。
    pub const fn capture_channels(&self) -> u16 {
        self.capture_channels
    }

    /// flow 数。
    pub const fn flow_count(&self) -> u8 {
        self.flow_count
    }

    /// capture index → H16 容器 slot（0-based）。
    pub fn container_slot(&self, capture_index: u16) -> Option<u8> {
        if (capture_index as usize) < self.capture_channels as usize {
            Some(self.slots[capture_index as usize])
        } else {
            None
        }
    }

    /// capture index → 逻辑角色（非逻辑通道返回 None）。
    pub fn role_of(&self, capture_index: u16) -> Option<H16Role> {
        if (capture_index as usize) >= self.capture_channels as usize {
            return None;
        }
        let role = self.roles[capture_index as usize];
        if role < 0 {
            None
        } else {
            H16Role::ALL.get(role as usize).copied()
        }
    }

    /// 逻辑角色 → capture index（该角色未被订阅时返回 None）。
    pub fn capture_index_of(&self, role: H16Role) -> Option<u16> {
        for capture in 0..self.capture_channels {
            if self.roles[capture as usize] == role.index() as i8 {
                return Some(capture);
            }
        }
        None
    }

    /// 已订阅（在容器中出现）的逻辑角色位图（bit = H16Role::index()）。
    pub const fn logical_present_mask(&self) -> u16 {
        self.logical_present
    }

    /// 未订阅的逻辑角色位图（bit = H16Role::index()）。
    pub const fn uncovered_roles_mask(&self) -> u16 {
        (!self.logical_present) & ((1u16 << H16_ROLE_COUNT) - 1)
    }

    /// 非逻辑容器通道的 capture-index 位图（H16 16ch 下为 bit8/9/12/13）。
    pub const fn non_logical_mask(&self) -> u16 {
        self.non_logical_mask
    }

    /// 非逻辑容器通道数。
    pub fn non_logical_count(&self) -> u32 {
        self.non_logical_mask.count_ones()
    }

    /// 结构上限校验（端点规格构造路径用）。
    pub fn validate_limits(&self) -> Result<(), AoipError> {
        let channels = self.capture_channels;
        if channels == 0 || channels > H16_CONTAINER_CHANNELS {
            return Err(AoipError::Channels);
        }
        if self.flow_count == 0 || self.flow_count as usize > H16_MAX_FLOWS {
            return Err(AoipError::DeviceParams);
        }
        Ok(())
    }
}

/// 角色选择位图（bit = H16Role::index()；只允许 bit0..11）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RoleSelection(u16);

impl RoleSelection {
    /// 空选择。
    pub const NONE: RoleSelection = RoleSelection(0);
    /// 全 12 逻辑角色。
    pub const ALL: RoleSelection = RoleSelection((1u16 << H16_ROLE_COUNT) - 1);

    /// 由位图构造（未知位由 plan_role_selection 拒收）。
    pub const fn from_mask(mask: u16) -> Self {
        RoleSelection(mask)
    }

    /// 原始位图。
    pub const fn mask(self) -> u16 {
        self.0
    }

    /// 是否含非法位（> bit11）。
    pub const fn has_unknown_bits(self) -> bool {
        self.0 & !((1u16 << H16_ROLE_COUNT) - 1) != 0
    }

    /// 是否包含某角色。
    pub const fn contains(self, role: H16Role) -> bool {
        self.0 & (1u16 << role.index()) != 0
    }

    /// 加入某角色。
    pub const fn with(self, role: H16Role) -> Self {
        RoleSelection(self.0 | (1u16 << role.index()))
    }

    /// 移除某角色。
    pub const fn without(self, role: H16Role) -> Self {
        RoleSelection(self.0 & !(1u16 << role.index()))
    }

    /// 选择的角色数。
    pub fn count(self) -> u32 {
        self.0.count_ones()
    }
}

/// 非逻辑容器通道（H16 16ch 的 8/9/12/13）处置策略（必须显式声明，禁止默认静默）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NonLogicalPolicy {
    /// 显式忽略：接收端保留这些容器通道，但不映射到任何角色/输出（记录计数）。
    ExplicitlyIgnore,
    /// 拒收：容器含非逻辑通道时直接报 ERR_ROLES（要求改用 12ch flow 组成）。
    Reject,
}

/// 角色选择结果（显式映射表；一个 capture 通道最多映射到一个角色，无 fan-in/下混）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RolePlan {
    /// 角色序号 → capture index，-1 = 未选择。
    pub capture_for_role: [i8; H16_ROLE_COUNT],
    /// 已选角色位图。
    pub selected: u16,
    /// 显式 ignore 的已收逻辑角色位图。
    pub ignored: u16,
    /// 非逻辑容器通道数（policy = ExplicitlyIgnore 时的显式忽略计数）。
    pub non_logical_ignored: u32,
    /// 采用的非逻辑通道策略。
    pub non_logical_policy: NonLogicalPolicy,
}

impl RolePlan {
    /// 某角色对应的 capture index。
    pub fn capture_index(&self, role: H16Role) -> Option<u16> {
        let value = self.capture_for_role[role.index()];
        if value < 0 {
            None
        } else {
            Some(value as u16)
        }
    }
}

/// 生成显式角色映射（禁止静默下混/静默丢弃；目标角色未订阅则拒收）。
///
/// 规则（DEC §1“目标角色以外通道必须显式选择/拒绝”）：
/// 1. selected/ignored 不得含未知位或重叠；
/// 2. 已订阅（容器内存在）的逻辑角色必须出现在 selected 或 ignored 之一；
/// 3. selected 中的角色必须在容器内已订阅（缺角色 → ERR_ROLES，不静默零填）；
/// 4. 容器含非逻辑通道（16ch 的 8/9/12/13）时，ExplicitlyIgnore 显式忽略并计数，
///    Reject 直接 ERR_ROLES；
/// 5. 一个 capture 通道最多映射一个角色（无下混/无 fan-in）。
pub fn plan_role_selection(
    container: &H16Container,
    selected: RoleSelection,
    ignored: RoleSelection,
    non_logical_policy: NonLogicalPolicy,
) -> Result<RolePlan, AoipError> {
    if selected.has_unknown_bits() || ignored.has_unknown_bits() {
        return Err(AoipError::Roles);
    }
    if selected.mask() & ignored.mask() != 0 {
        return Err(AoipError::Roles);
    }
    let present = container.logical_present_mask();
    let covered = selected.mask() | ignored.mask();
    if covered & present != present {
        // 已收逻辑通道里有既未选择也未显式 ignore 的 → 不允许静默丢弃。
        return Err(AoipError::Roles);
    }
    if selected.mask() & !present != 0 {
        // 选中的目标角色在容器里没有被订阅 → 不允许静默零填。
        return Err(AoipError::Roles);
    }
    if non_logical_policy == NonLogicalPolicy::Reject && container.non_logical_mask() != 0 {
        return Err(AoipError::Roles);
    }
    let mut plan = RolePlan {
        capture_for_role: [-1i8; H16_ROLE_COUNT],
        selected: selected.mask(),
        ignored: ignored.mask(),
        non_logical_ignored: 0,
        non_logical_policy,
    };
    for role in H16Role::ALL {
        if selected.contains(role) {
            let capture = container.capture_index_of(role).ok_or(AoipError::Roles)?;
            plan.capture_for_role[role.index()] = capture as i8;
        }
    }
    if non_logical_policy == NonLogicalPolicy::ExplicitlyIgnore {
        plan.non_logical_ignored = container.non_logical_count();
    }
    Ok(plan)
}

// ---------------------------------------------------------------------------
// AES67 SDP（只读解析；不生成/不发送 RTSP/SAP/RTP）
// ---------------------------------------------------------------------------

/// AES67 线上媒体编码（AES67 媒体面：L24 必须；48k 允许 L16）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aes67Encoding {
    /// 24-bit linear packed（H16 官方 SDP L24/48000/8）。
    L24,
    /// 16-bit linear（AES67 48k 允许，本栈仅作兼容解析）。
    L16,
}

impl Aes67Encoding {
    /// SDP 编码名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Aes67Encoding::L24 => "L24",
            Aes67Encoding::L16 => "L16",
        }
    }
}

/// SDP a=ts-refclk 的 PTPv2 参考钟（RFC 7273 子集）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtpRefClock {
    /// PTP domain（AES67 默认 0；必须与 daemon/端点配置一致）。
    pub domain: u8,
    /// 是否携带合法 EUI-64 grandmaster id（xx-xx-... 8 组）。
    pub grandmaster_present: bool,
}

/// 单条 AES67 flow 的 SDP 摘要（H16 每个 flow 一个 SDP；字段全部为解析结果）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67FlowSdp<'a> {
    /// s= 会话名（H16 形如 NuPrime-a227-5240:16；只记录不解释布局）。
    pub session_name: Option<&'a str>,
    /// c= 组播地址（不含 /TTL）。
    pub connection_addr: &'a str,
    /// c= 的可选 TTL/num（越界即拒收）。
    pub connection_ttl: Option<u8>,
    /// m=audio RTP 端口。
    pub port: u16,
    /// 动态 payload type（96..=127）。
    pub payload_type: u8,
    /// rtpmap 编码（L24/L16）。
    pub encoding: Aes67Encoding,
    /// rtpmap 时钟率。
    pub sample_rate_hz: u32,
    /// 本 flow 通道数（AES67 媒体面 ≤8；H16 官方示例为 8）。
    pub channels: u16,
    /// a=ptime:（µs；缺省 None）。
    pub ptime_us: Option<u32>,
    /// a=ts-refclk:ptp=IEEE1588-2008:...（AES67 媒体面必需）。
    pub ts_refclk: Option<PtpRefClock>,
    /// a=mediaclk:direct=0（参考钟直接映射；只记录）。
    pub mediaclk_direct: bool,
}

impl<'a> Aes67FlowSdp<'a> {
    /// 校验单条 flow 是否为可接受的 AES67 媒体面描述。
    pub fn validate(&self) -> Result<(), AoipError> {
        if self.sample_rate_hz != AES67_RATE_HZ {
            return Err(AoipError::Rate);
        }
        if self.channels == 0 || self.channels > AES67_MAX_CHANNELS_PER_FLOW {
            return Err(AoipError::Channels);
        }
        if self.port == 0 {
            return Err(AoipError::DeviceParams);
        }
        if self.payload_type < 96 {
            return Err(AoipError::Unsupported);
        }
        match self.ptime_us {
            Some(ptime) if ptime == 0 || ptime > AES67_PTIME_MAX_US => {
                return Err(AoipError::Unsupported)
            }
            _ => {}
        }
        if self.ts_refclk.is_none() {
            // AES67 媒体面需要可追溯 PTP 时间基准；缺 ts-refclk 不接受（不猜时钟）。
            return Err(AoipError::Unsupported);
        }
        let address = parse_ipv4(self.connection_addr).ok_or(AoipError::DeviceParams)?;
        if !is_multicast_ipv4(address) {
            return Err(AoipError::Unsupported);
        }
        Ok(())
    }
}

/// 一组 AES67 flow SDP（同一接收端的全部 flow；最多 H16_MAX_FLOWS 条）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67FlowSet<'a> {
    flows: [Option<Aes67FlowSdp<'a>>; H16_MAX_FLOWS],
    count: u8,
}

impl<'a> Aes67FlowSet<'a> {
    /// 由已解析 flow 构造并逐条校验（1..=4 条）。
    pub fn new(flows: &[Aes67FlowSdp<'a>]) -> Result<Self, AoipError> {
        if flows.is_empty() || flows.len() > H16_MAX_FLOWS {
            return Err(AoipError::DeviceParams);
        }
        let mut set = Self {
            flows: [None; H16_MAX_FLOWS],
            count: flows.len() as u8,
        };
        for (index, flow) in flows.iter().enumerate() {
            flow.validate()?;
            set.flows[index] = Some(*flow);
        }
        let mut total = 0u16;
        for flow in flows {
            total = total.saturating_add(flow.channels);
        }
        if total == 0 || total > H16_CONTAINER_CHANNELS {
            return Err(AoipError::Channels);
        }
        Ok(set)
    }

    /// flow 数。
    pub const fn flow_count(&self) -> usize {
        self.count as usize
    }

    /// 第 index 条 flow。
    pub fn flow(&self, index: usize) -> Option<Aes67FlowSdp<'a>> {
        if index < self.count as usize {
            self.flows[index]
        } else {
            None
        }
    }

    /// 通道总数。
    pub fn total_channels(&self) -> u16 {
        let mut total = 0u16;
        for index in 0..self.count as usize {
            if let Some(flow) = self.flows[index] {
                total = total.saturating_add(flow.channels);
            }
        }
        total
    }
}

/// 单条 flow 的传输标识（Copy，供计划/证据使用；地址为 4 字节 IPv4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67FlowMedia {
    /// 组播地址。
    pub destination: [u8; 4],
    /// 可选 TTL/num。
    pub ttl: Option<u8>,
    /// RTP 端口。
    pub port: u16,
    /// 动态 payload type。
    pub payload_type: u8,
    /// 本 flow 通道数。
    pub channels: u16,
}

impl Aes67FlowMedia {
    /// 空值（数组占位）。
    pub const EMPTY: Aes67FlowMedia = Aes67FlowMedia {
        destination: [0; 4],
        ttl: None,
        port: 0,
        payload_type: 0,
        channels: 0,
    };
}

/// AES67 SDP ↔ 显式 flow 配置交叉校验后的接收计划。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67CapturePlan {
    /// 接收端容器映射（12/16ch）。
    pub container: H16Container,
    /// flow 数。
    pub flow_count: u8,
    /// 接收端通道总数。
    pub total_channels: u16,
    /// 所有 flow 一致的媒体编码。
    pub encoding: Aes67Encoding,
    /// 媒体采样率（必须 48k）。
    pub sample_rate_hz: u32,
    /// SDP/PTPv2 domain（须与配置一致）。
    pub ptp_domain: u8,
    /// 每条 flow 的传输标识。
    pub media: [Aes67FlowMedia; H16_MAX_FLOWS],
    /// 有效 media 条数。
    pub media_len: u8,
    /// 所有 flow 的最大 ptime（µs；无 ptime 为 0）。
    pub ptime_us_max: u32,
}

impl Aes67CapturePlan {
    /// 按 RAVENNA 稳定 card id 解析已有 ALSA 端点（复用 T17 /proc/asound/cards 解析）。
    pub fn resolve_alsa_device(
        &self,
        cards_text: &str,
        card_id: &str,
    ) -> Result<ResolvedDevice, AoipError> {
        let selector = alsa::DeviceSelector::by_id(card_id, 0);
        ResolvedDevice::from_cards(cards_text, &selector).map_err(AoipError::from)
    }

    /// 生成 AES67 ALSA capture 端点规格（不打开硬件；T26 在硬件闸门后使用）。
    pub fn alsa_endpoint(
        &self,
        cards_text: &str,
        card_id: &str,
    ) -> Result<AoipEndpointSpec<'static>, AoipError> {
        let device = self.resolve_alsa_device(cards_text, card_id)?;
        let spec = AoipEndpointSpec::aes67_alsa(device, self.container);
        spec.validate()?;
        Ok(spec)
    }

    /// 生成 AES67 pipe 端点规格（raw S32_LE 交错；测试/回退路径，非生产协议路径）。
    pub fn pipe_endpoint<'a>(&self, path: &'a str) -> Result<AoipEndpointSpec<'a>, AoipError> {
        let spec = AoipEndpointSpec::aes67_pipe(path, self.container);
        spec.validate()?;
        Ok(spec)
    }
}

/// 解析一条 AES67 flow 的 SDP 文本（RFC 4566 子集；只读）。
///
/// 只支持单 m=audio 会话（H16 每条 flow 一个 SDP 包）；多 m=/video/IPv6/非 RTP/AVP
/// 一律 ERR_UNSUPPORTED；畸形行 ERR_DEVICE_PARAMS。
pub fn parse_aes67_sdp(text: &str) -> Result<Aes67FlowSdp<'_>, AoipError> {
    let mut version_seen = false;
    let mut session_name: Option<&str> = None;
    let mut session_conn: Option<(&str, Option<u8>)> = None;
    let mut media_conn: Option<(&str, Option<u8>)> = None;
    let mut media_seen = false;
    let mut port = 0u16;
    let mut payload_type = 0u8;
    let mut rtpmap: Option<(u8, Aes67Encoding, u32, u16)> = None;
    let mut ptime_us: Option<u32> = None;
    let mut ts_refclk: Option<PtpRefClock> = None;
    let mut mediaclk_direct = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("v=") {
            if version_seen || value != "0" {
                return Err(AoipError::DeviceParams);
            }
            version_seen = true;
        } else if let Some(value) = line.strip_prefix("s=") {
            if session_name.is_none() {
                session_name = Some(value.trim());
            }
        } else if let Some(value) = line.strip_prefix("c=") {
            let parsed = parse_connection(value)?;
            if media_seen {
                if media_conn.is_some() {
                    return Err(AoipError::DeviceParams);
                }
                media_conn = Some(parsed);
            } else {
                if session_conn.is_some() {
                    return Err(AoipError::DeviceParams);
                }
                session_conn = Some(parsed);
            }
        } else if let Some(value) = line.strip_prefix("m=") {
            if media_seen {
                // 多会话（多 m=）不是单 flow SDP；由调用方逐 flow 解析。
                return Err(AoipError::Unsupported);
            }
            let (parsed_port, parsed_payload) = parse_media(value)?;
            port = parsed_port;
            payload_type = parsed_payload;
            media_seen = true;
        } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
            let parsed = parse_rtpmap(value)?;
            if parsed.0 == payload_type {
                if rtpmap.is_some() {
                    return Err(AoipError::DeviceParams);
                }
                rtpmap = Some(parsed);
            }
        } else if let Some(value) = line.strip_prefix("a=ptime:") {
            if ptime_us.is_some() {
                return Err(AoipError::DeviceParams);
            }
            ptime_us = Some(parse_ptime_us(value.trim()).ok_or(AoipError::DeviceParams)?);
        } else if let Some(value) = line.strip_prefix("a=ts-refclk:") {
            if ts_refclk.is_some() {
                return Err(AoipError::DeviceParams);
            }
            ts_refclk = Some(parse_ts_refclk(value.trim())?);
        } else if let Some(value) = line.strip_prefix("a=mediaclk:") {
            if value.trim().starts_with("direct=0") {
                mediaclk_direct = true;
            }
        }
    }

    if !version_seen || !media_seen {
        return Err(AoipError::DeviceParams);
    }
    let (_, encoding, sample_rate_hz, channels) = rtpmap.ok_or(AoipError::DeviceParams)?;
    let (connection_addr, connection_ttl) =
        media_conn.or(session_conn).ok_or(AoipError::DeviceParams)?;

    let flow = Aes67FlowSdp {
        session_name,
        connection_addr,
        connection_ttl,
        port,
        payload_type,
        encoding,
        sample_rate_hz,
        channels,
        ptime_us,
        ts_refclk,
        mediaclk_direct,
    };
    flow.validate()?;
    Ok(flow)
}

/// SDP ↔ 显式 flow 配置交叉校验，得到 AES67 接收计划。
///
/// 校验项：flow 数/每 flow 通道数与配置一致；总通道数 = 容器通道数；48k；编码一致；
/// PTPv2 ts-refclk domain 全部等于配置值；组播且 (addr,port) 唯一；ptime ≤1ms。
pub fn validate_aes67_capture(
    flows: &Aes67FlowSet<'_>,
    config: &H16FlowConfig,
    ptp_domain: u8,
) -> Result<Aes67CapturePlan, AoipError> {
    let container = H16Container::from_flow_config(config)?;
    if flows.flow_count() != config.flow_count() {
        return Err(AoipError::DeviceParams);
    }
    let mut encoding: Option<Aes67Encoding> = None;
    let mut media = [Aes67FlowMedia::EMPTY; H16_MAX_FLOWS];
    let mut ptime_us_max = 0u32;
    let mut total_channels = 0u16;
    for index in 0..flows.flow_count() {
        let flow = flows.flow(index).ok_or(AoipError::DeviceParams)?;
        let expected_channels = config.flow_len(index).ok_or(AoipError::DeviceParams)? as u16;
        if flow.channels != expected_channels {
            return Err(AoipError::DeviceParams);
        }
        if let Some(previous) = encoding {
            if previous != flow.encoding {
                return Err(AoipError::Unsupported);
            }
        } else {
            encoding = Some(flow.encoding);
        }
        let refclk = flow.ts_refclk.ok_or(AoipError::Unsupported)?;
        if refclk.domain != ptp_domain {
            // SDP 声明的 PTP 域与端点/daemon 配置不一致 → 时间基准不可信。
            return Err(AoipError::Unsupported);
        }
        let destination = parse_ipv4(flow.connection_addr).ok_or(AoipError::DeviceParams)?;
        for previous in &media[..index] {
            if previous.destination == destination && previous.port == flow.port {
                return Err(AoipError::DeviceParams);
            }
        }
        media[index] = Aes67FlowMedia {
            destination,
            ttl: flow.connection_ttl,
            port: flow.port,
            payload_type: flow.payload_type,
            channels: flow.channels,
        };
        ptime_us_max = ptime_us_max.max(flow.ptime_us.unwrap_or(0));
        total_channels = total_channels.saturating_add(flow.channels);
    }
    if total_channels != container.capture_channels() {
        return Err(AoipError::Channels);
    }
    Ok(Aes67CapturePlan {
        container,
        flow_count: flows.flow_count() as u8,
        total_channels,
        encoding: encoding.ok_or(AoipError::DeviceParams)?,
        sample_rate_hz: AES67_RATE_HZ,
        ptp_domain,
        media,
        media_len: flows.flow_count() as u8,
        ptime_us_max,
    })
}

/// 已有 AES67 服务是否就绪（daemon 由 T42/T44 启停；本层只消费状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aes67ServiceState {
    /// aes67-daemon 已就绪（HTTP/RTSP/RTP 起来、状态文件可写；板端当前被缺库阻塞）。
    pub daemon_ready: bool,
    /// daemon 实际采样率（daemon.conf sample_rate）。
    pub sample_rate_hz: u32,
    /// daemon PTP domain（daemon.conf ptp_domain）。
    pub ptp_domain: u8,
}

impl Aes67ServiceState {
    /// 构造。
    pub const fn new(daemon_ready: bool, sample_rate_hz: u32, ptp_domain: u8) -> Self {
        Self {
            daemon_ready,
            sample_rate_hz,
            ptp_domain,
        }
    }
}

/// 校验 AES67 服务就绪与 SDP 计划一致（未就绪 → RT_E_DEVICE_LOST 语义的端点不可用）。
pub fn validate_aes67_service(
    service: &Aes67ServiceState,
    plan: &Aes67CapturePlan,
) -> Result<(), AoipError> {
    if !service.daemon_ready {
        return Err(AoipError::StreamLost);
    }
    if service.sample_rate_hz != plan.sample_rate_hz {
        return Err(AoipError::Rate);
    }
    if service.ptp_domain != plan.ptp_domain {
        return Err(AoipError::Unsupported);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Inferno/Dante 端点环境（已有 ALSA 插件 / inferno2pipe；不解析协议）
// ---------------------------------------------------------------------------

/// Inferno ALSA 插件 / inferno2pipe 所需环境事实（khaen-mode 渲染 camilladsp.env）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InfernoEnv<'a> {
    /// device id（16 hex；auto 表示由 MAC 派生 + 0000）。
    pub device_id: &'a str,
    /// PTPv1 虚拟钟导出路径（statime usrvclock）。
    pub clock_path: &'a str,
    /// 绑定网卡（如 end0）。
    pub bind_iface: &'a str,
    /// Inferno 采样率（96k；inferno2pipe 需 SAMPLE_RATE=96000）。
    pub sample_rate_hz: u32,
    /// RX 通道数（必须等于容器通道数）。
    pub rx_channels: u16,
}

impl<'a> InfernoEnv<'a> {
    /// 构造。
    pub const fn new(
        device_id: &'a str,
        clock_path: &'a str,
        bind_iface: &'a str,
        sample_rate_hz: u32,
        rx_channels: u16,
    ) -> Self {
        Self {
            device_id,
            clock_path,
            bind_iface,
            sample_rate_hz,
            rx_channels,
        }
    }

    /// 校验 Inferno 环境与容器一致（缺时钟/绑定/ID 一律拒收，不打开默认 PCM）。
    pub fn validate(&self, container: &H16Container) -> Result<(), AoipError> {
        if self.sample_rate_hz != INFERNO_RATE_HZ {
            return Err(AoipError::Rate);
        }
        if self.rx_channels != container.capture_channels() {
            return Err(AoipError::Channels);
        }
        if self.clock_path.is_empty() || !self.clock_path.starts_with('/') {
            return Err(AoipError::DeviceParams);
        }
        if self.bind_iface.is_empty() {
            return Err(AoipError::DeviceParams);
        }
        if !inferno_device_id_valid(self.device_id) {
            return Err(AoipError::DeviceParams);
        }
        Ok(())
    }
}

/// device id 是否可接受（auto 或 16 个 hex 字符）。
pub fn inferno_device_id_valid(device_id: &str) -> bool {
    if device_id == "auto" {
        return true;
    }
    if device_id.len() != INFERNO_DEVICE_ID_HEX_LEN {
        return false;
    }
    device_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// 端点规格（ALSA / pipe）与格式/速率校验（compat A17/K45）
// ---------------------------------------------------------------------------

/// 已有 ALSA 端点地址（AES67：稳定 card 解析出的 hw:<card>,<device>；Inferno：命名 PCM）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlsaDevice<'a> {
    /// hw:<card>,<device>（由 /proc/asound/cards 稳定 id 解析，禁止写死 card 号）。
    Hw(ResolvedDevice),
    /// 命名 ALSA PCM（Inferno 插件 inferno）。
    Named(&'a str),
}

/// AoIP capture 端点规格（不打开硬件；T26 在 T17 硬件闸门后按此打开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AoipEndpointSpec<'a> {
    /// 协议族。
    pub protocol: AoipProtocol,
    /// 传输方式。
    pub transport: AoipTransport,
    /// ALSA 端点地址（transport = Alsa 时必须 Some）。
    pub alsa_device: Option<AlsaDevice<'a>>,
    /// pipe 路径（transport = Pipe 时必须 Some）。
    pub pipe_path: Option<&'a str>,
    /// 接收端容器映射（capture 通道数 = 容器通道数）。
    pub container: H16Container,
    /// 采样率（AES67 48k / Inferno 96k）。
    pub sample_rate_hz: u32,
    /// 设备侧容器格式（S32_LE；SDP 线上 L24 由 daemon/插件转换）。
    pub device_format: u16,
    /// period（只允许 64/128）。
    pub period_frames: u32,
    /// buffer（period 整数倍，≥ period）。
    pub buffer_frames: u32,
    /// 失流停滞门限（period 数）。
    pub max_stall_periods: u32,
    /// 期望的 PTP 来源（必须与协议匹配；A15/K44 唯一来源）。
    pub ptp_source: PtpSource,
}

impl<'a> AoipEndpointSpec<'a> {
    /// AES67 ALSA 端点（RAVENNA LKM hw:<card>,0）。
    pub fn aes67_alsa(device: ResolvedDevice, container: H16Container) -> Self {
        Self {
            protocol: AoipProtocol::Aes67,
            transport: AoipTransport::Alsa,
            alsa_device: Some(AlsaDevice::Hw(device)),
            pipe_path: None,
            container,
            sample_rate_hz: AES67_RATE_HZ,
            device_format: AoipProtocol::Aes67.device_format(),
            period_frames: DEFAULT_PERIOD_FRAMES,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            max_stall_periods: DEFAULT_MAX_STALL_PERIODS,
            ptp_source: AoipProtocol::Aes67.expected_ptp_source(),
        }
    }

    /// AES67 pipe 端点（raw S32_LE 交错；测试/回退，非生产 daemon+RTP 路径）。
    pub fn aes67_pipe(path: &'a str, container: H16Container) -> Self {
        Self {
            protocol: AoipProtocol::Aes67,
            transport: AoipTransport::Pipe,
            alsa_device: None,
            pipe_path: Some(path),
            container,
            sample_rate_hz: AES67_RATE_HZ,
            device_format: AoipProtocol::Aes67.device_format(),
            period_frames: DEFAULT_PERIOD_FRAMES,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            max_stall_periods: DEFAULT_MAX_STALL_PERIODS,
            ptp_source: AoipProtocol::Aes67.expected_ptp_source(),
        }
    }

    /// Inferno/Dante ALSA 端点（已有 inferno 插件）。
    pub fn inferno_alsa(container: H16Container) -> Self {
        Self {
            protocol: AoipProtocol::Inferno,
            transport: AoipTransport::Alsa,
            alsa_device: Some(AlsaDevice::Named(INFERNO_ALSA_DEVICE)),
            pipe_path: None,
            container,
            sample_rate_hz: INFERNO_RATE_HZ,
            device_format: AoipProtocol::Inferno.device_format(),
            period_frames: DEFAULT_PERIOD_FRAMES,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            max_stall_periods: DEFAULT_MAX_STALL_PERIODS,
            ptp_source: AoipProtocol::Inferno.expected_ptp_source(),
        }
    }

    /// Inferno pipe 端点（已有 inferno2pipe 输出；native i32 交错 = LE S32_LE）。
    pub fn inferno_pipe(path: &'a str, container: H16Container) -> Self {
        Self {
            protocol: AoipProtocol::Inferno,
            transport: AoipTransport::Pipe,
            alsa_device: None,
            pipe_path: Some(path),
            container,
            sample_rate_hz: INFERNO_RATE_HZ,
            device_format: AoipProtocol::Inferno.device_format(),
            period_frames: DEFAULT_PERIOD_FRAMES,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            max_stall_periods: DEFAULT_MAX_STALL_PERIODS,
            ptp_source: AoipProtocol::Inferno.expected_ptp_source(),
        }
    }

    /// 覆写 period/buffer（如 T26 由 actual 参数回填；本方法不改协议 profile）。
    pub fn with_period_buffer(mut self, period_frames: u32, buffer_frames: u32) -> Self {
        self.period_frames = period_frames;
        self.buffer_frames = buffer_frames;
        self
    }

    /// 协议 → 唯一 PTP 来源是否匹配（A15/K44）。
    pub fn ptp_source_matches(&self) -> bool {
        self.ptp_source == self.protocol.expected_ptp_source()
    }

    /// 单帧字节数（容器通道 × 设备样本字节）。
    pub fn frame_bytes(&self) -> Result<u32, AoipError> {
        let bytes = alsa::sample_bytes(self.device_format).ok_or(AoipError::Unsupported)?;
        let channels = self.container.capture_channels() as u32;
        channels.checked_mul(bytes).ok_or(AoipError::BadLayout)
    }

    /// 校验端点规格（不打开设备；任何不匹配 fail closed）。
    pub fn validate(&self) -> Result<(), AoipError> {
        if self.sample_rate_hz != self.protocol.profile_rate_hz() {
            return Err(AoipError::Rate);
        }
        if !is_runtime_rate(self.sample_rate_hz) {
            return Err(AoipError::Rate);
        }
        if self.device_format != self.protocol.device_format() {
            return Err(AoipError::Unsupported);
        }
        if !is_runtime_period(self.period_frames) {
            return Err(AoipError::BlockSize);
        }
        if self.buffer_frames < self.period_frames || self.buffer_frames % self.period_frames != 0 {
            return Err(AoipError::DeviceParams);
        }
        if self.max_stall_periods == 0 || self.max_stall_periods > 1_000 {
            return Err(AoipError::DeviceParams);
        }
        if !self.ptp_source_matches() {
            return Err(AoipError::Unsupported);
        }
        self.container.validate_limits()?;
        match self.transport {
            AoipTransport::Alsa => {
                if self.pipe_path.is_some() {
                    return Err(AoipError::DeviceParams);
                }
                match (self.protocol, self.alsa_device) {
                    (AoipProtocol::Aes67, Some(AlsaDevice::Hw(device))) => {
                        if device.device_index != 0 {
                            // khaen-mode 固定 hw:<RAVENNA>,0；其它 device 不经既有链路。
                            return Err(AoipError::DeviceParams);
                        }
                    }
                    (AoipProtocol::Aes67, _) => return Err(AoipError::DeviceParams),
                    (AoipProtocol::Inferno, Some(AlsaDevice::Named(name))) => {
                        if name != INFERNO_ALSA_DEVICE {
                            return Err(AoipError::DeviceParams);
                        }
                    }
                    (AoipProtocol::Inferno, _) => return Err(AoipError::Unsupported),
                }
            }
            AoipTransport::Pipe => {
                if self.alsa_device.is_some() {
                    return Err(AoipError::DeviceParams);
                }
                let path = self.pipe_path.ok_or(AoipError::DeviceParams)?;
                if path.is_empty() || path.len() > 4_096 {
                    return Err(AoipError::DeviceParams);
                }
                if cfg!(target_endian = "big") {
                    // inferno2pipe 写 native i32；BE target 上不是 S32_LE，不能当 LE 解析。
                    return Err(AoipError::Unsupported);
                }
                let frame_bytes = self.frame_bytes()?;
                let chunk = (self.period_frames as usize)
                    .checked_mul(frame_bytes as usize)
                    .ok_or(AoipError::BadLayout)?;
                if chunk == 0 || chunk > MAX_PIPE_CHUNK_BYTES {
                    return Err(AoipError::BadLayout);
                }
            }
        }
        Ok(())
    }

    /// 校验一次 pipe 读取：必须整帧对齐（无半帧/字节错位），返回帧数。
    pub fn validate_pipe_chunk(&self, bytes: usize) -> Result<u32, AoipError> {
        if self.transport != AoipTransport::Pipe {
            return Err(AoipError::Unsupported);
        }
        let frame_bytes = self.frame_bytes()? as usize;
        if frame_bytes == 0 {
            return Err(AoipError::BadLayout);
        }
        if bytes > MAX_PIPE_CHUNK_BYTES {
            return Err(AoipError::BadLayout);
        }
        if bytes % frame_bytes != 0 {
            // 半帧/字节错位 = 端点流损坏；整块静音并请求恢复，不得当有效数据。
            return Err(AoipError::DeviceParams);
        }
        Ok((bytes / frame_bytes) as u32)
    }

    /// 已声明的期望 PTP 来源（供 T19/T26 断言唯一 servo owner 前使用）。
    pub const fn expected_ptp_source(&self) -> PtpSource {
        self.protocol.expected_ptp_source()
    }
}

/// PTP 锁状态是否足以开始/维持 AoIP 采集（A15；Holdover 不放行——媒体钟会漂）。
pub const fn ptp_ready(lock: LockState) -> bool {
    matches!(lock, LockState::Locked)
}

/// 启动前预检：端点规格 + PTP 来源/锁（不打开硬件）。
pub fn preflight(spec: &AoipEndpointSpec<'_>, lock: LockState) -> Result<(), AoipError> {
    spec.validate()?;
    if !ptp_ready(lock) {
        return Err(AoipError::PtpUnavailable);
    }
    Ok(())
}

/// Inferno ALSA 端点：环境校验 + 规格校验（不打开插件）。
pub fn inferno_alsa_endpoint(
    env: &InfernoEnv<'_>,
    container: H16Container,
) -> Result<AoipEndpointSpec<'static>, AoipError> {
    env.validate(&container)?;
    let spec = AoipEndpointSpec::inferno_alsa(container);
    spec.validate()?;
    Ok(spec)
}

/// Inferno pipe 端点：环境校验 + 规格校验（不打开管道）。
pub fn inferno_pipe_endpoint<'a>(
    env: &InfernoEnv<'_>,
    container: H16Container,
    path: &'a str,
) -> Result<AoipEndpointSpec<'a>, AoipError> {
    env.validate(&container)?;
    let spec = AoipEndpointSpec::inferno_pipe(path, container);
    spec.validate()?;
    Ok(spec)
}

// ---------------------------------------------------------------------------
// 失流模型（fake；不打开设备，只看数据到达时刻/帧数）
// ---------------------------------------------------------------------------

/// 流监视器状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorState {
    /// 未启动。
    Idle,
    /// 已启动、等待首包。
    WaitingFirstData,
    /// 有数据流动。
    Flowing,
    /// 停滞超过停滞门限（整块静音；同 epoch 可恢复）。
    Stalled,
    /// 持续失流超过 2×门限（需新 epoch 才可恢复）。
    Lost,
}

impl MonitorState {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            MonitorState::Idle => "idle",
            MonitorState::WaitingFirstData => "waiting_first_data",
            MonitorState::Flowing => "flowing",
            MonitorState::Stalled => "stalled",
            MonitorState::Lost => "lost",
        }
    }
}

/// 流监视器事件（驱动 host 侧整块静音/新 epoch；不产生半块混音）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorEvent {
    /// 无状态变化。
    None,
    /// 首包到达，进入流动。
    Started,
    /// 停滞：调用方必须整块输出预分配静音；同 epoch 可恢复。
    Stalled {
        /// 已静音的 period 数（由实际间隔估算，饱和）。
        silent_periods: u32,
    },
    /// 持续失流：调用方必须静音并请求新 epoch。
    Lost {
        /// 已静音的 period 数。
        silent_periods: u32,
    },
    /// 停滞期后数据恢复（同 epoch）。
    Resumed {
        /// 本次停滞静音过的 period 数。
        silent_periods: u32,
    },
}

/// 失流监视器：按实际 period 的 T_block 门限（首包超时 = 停滞门限）处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamMonitor {
    period_frames: u32,
    sample_rate_hz: u32,
    max_stall_periods: u32,
    state: MonitorState,
    started_ns: u64,
    last_data_ns: Option<u64>,
    silent_periods: u32,
    frames_total: u64,
    data_events: u64,
    stall_events: u32,
    loss_events: u32,
    resume_events: u32,
}

impl StreamMonitor {
    /// 新建（period 只允许 64/128、rate 只允许 48k/96k、停滞门限 1..=1000）。
    pub fn new(
        period_frames: u32,
        sample_rate_hz: u32,
        max_stall_periods: u32,
    ) -> Result<Self, AoipError> {
        if !is_runtime_period(period_frames) {
            return Err(AoipError::BlockSize);
        }
        if !is_runtime_rate(sample_rate_hz) {
            return Err(AoipError::Rate);
        }
        if max_stall_periods == 0 || max_stall_periods > 1_000 {
            return Err(AoipError::DeviceParams);
        }
        Ok(Self {
            period_frames,
            sample_rate_hz,
            max_stall_periods,
            state: MonitorState::Idle,
            started_ns: 0,
            last_data_ns: None,
            silent_periods: 0,
            frames_total: 0,
            data_events: 0,
            stall_events: 0,
            loss_events: 0,
            resume_events: 0,
        })
    }

    /// 由端点规格构造。
    pub fn from_spec(spec: &AoipEndpointSpec<'_>) -> Result<Self, AoipError> {
        Self::new(
            spec.period_frames,
            spec.sample_rate_hz,
            spec.max_stall_periods,
        )
    }

    /// 启动监视（等待首包）。
    pub fn start(&mut self, now_ns: u64) {
        self.state = MonitorState::WaitingFirstData;
        self.started_ns = now_ns;
        self.last_data_ns = None;
        self.silent_periods = 0;
    }

    /// 复位（重新 epoch 后调用）。
    pub fn reset(&mut self) {
        self.state = MonitorState::Idle;
        self.started_ns = 0;
        self.last_data_ns = None;
        self.silent_periods = 0;
    }

    /// 当前状态。
    pub const fn state(&self) -> MonitorState {
        self.state
    }

    /// period 帧数。
    pub const fn period_frames(&self) -> u32 {
        self.period_frames
    }

    /// 采样率。
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 停滞门限（period 数）。
    pub const fn max_stall_periods(&self) -> u32 {
        self.max_stall_periods
    }

    /// 一个 block 的时长（ns；ceil，不放宽门限）。
    pub const fn t_block_ns(&self) -> u64 {
        t_block_ns_ceil(self.sample_rate_hz, self.period_frames as u16)
    }

    /// 停滞门限（ns）。
    pub fn stall_threshold_ns(&self) -> u64 {
        self.t_block_ns()
            .saturating_mul(self.max_stall_periods as u64)
    }

    /// 持续失流门限（ns）= 2×停滞门限。
    pub fn lost_threshold_ns(&self) -> u64 {
        self.stall_threshold_ns()
            .saturating_mul(STREAM_LOST_MULTIPLIER as u64)
    }

    /// 累计帧数。
    pub const fn frames_total(&self) -> u64 {
        self.frames_total
    }

    /// 累计数据回调数。
    pub const fn data_events(&self) -> u64 {
        self.data_events
    }

    /// 停滞次数。
    pub const fn stall_events(&self) -> u32 {
        self.stall_events
    }

    /// 失流次数。
    pub const fn loss_events(&self) -> u32 {
        self.loss_events
    }

    /// 恢复次数。
    pub const fn resume_events(&self) -> u32 {
        self.resume_events
    }

    /// 数据到达：frames > 0；Lost 状态下必须先 reopen 新 epoch。
    pub fn on_data(&mut self, now_ns: u64, frames: u32) -> Result<MonitorEvent, AoipError> {
        if frames == 0 {
            return Err(AoipError::DeviceParams);
        }
        match self.state {
            MonitorState::Idle => Err(AoipError::SwitchBusy),
            MonitorState::Lost => Err(AoipError::EpochMismatch),
            MonitorState::WaitingFirstData => {
                self.last_data_ns = Some(now_ns);
                self.silent_periods = 0;
                self.frames_total = self.frames_total.saturating_add(frames as u64);
                self.data_events = self.data_events.saturating_add(1);
                self.state = MonitorState::Flowing;
                Ok(MonitorEvent::Started)
            }
            MonitorState::Flowing => {
                self.last_data_ns = Some(now_ns);
                self.frames_total = self.frames_total.saturating_add(frames as u64);
                self.data_events = self.data_events.saturating_add(1);
                Ok(MonitorEvent::None)
            }
            MonitorState::Stalled => {
                let silent = self.silent_periods;
                self.last_data_ns = Some(now_ns);
                self.silent_periods = 0;
                self.frames_total = self.frames_total.saturating_add(frames as u64);
                self.data_events = self.data_events.saturating_add(1);
                self.resume_events = self.resume_events.saturating_add(1);
                self.state = MonitorState::Flowing;
                Ok(MonitorEvent::Resumed {
                    silent_periods: silent,
                })
            }
        }
    }

    /// 按时间推进：首包超时/停滞/持续失流（idempotent，只在迁移时返回事件）。
    pub fn poll(&mut self, now_ns: u64) -> MonitorEvent {
        let threshold = self.stall_threshold_ns();
        let lost_threshold = self.lost_threshold_ns();
        match self.state {
            MonitorState::Idle | MonitorState::Lost => MonitorEvent::None,
            MonitorState::WaitingFirstData => {
                let elapsed = now_ns.saturating_sub(self.started_ns);
                let silent = (elapsed / self.t_block_ns()) as u32;
                if elapsed > lost_threshold {
                    self.silent_periods = silent;
                    self.loss_events = self.loss_events.saturating_add(1);
                    self.state = MonitorState::Lost;
                    MonitorEvent::Lost {
                        silent_periods: silent,
                    }
                } else if elapsed > threshold {
                    self.silent_periods = silent;
                    self.stall_events = self.stall_events.saturating_add(1);
                    self.state = MonitorState::Stalled;
                    MonitorEvent::Stalled {
                        silent_periods: silent,
                    }
                } else {
                    MonitorEvent::None
                }
            }
            MonitorState::Flowing | MonitorState::Stalled => {
                // 停滞也可能发生在“首包未到”阶段（last_data_ns = None），此时以
                // 启动时刻为参考继续升级到 Lost，不能永久停在 Stalled。
                let reference = self.last_data_ns.unwrap_or(self.started_ns);
                let elapsed = now_ns.saturating_sub(reference);
                let silent = (elapsed / self.t_block_ns()) as u32;
                if elapsed > lost_threshold {
                    self.silent_periods = silent;
                    self.loss_events = self.loss_events.saturating_add(1);
                    self.state = MonitorState::Lost;
                    MonitorEvent::Lost {
                        silent_periods: silent,
                    }
                } else if self.state == MonitorState::Flowing && elapsed > threshold {
                    self.silent_periods = silent;
                    self.stall_events = self.stall_events.saturating_add(1);
                    self.state = MonitorState::Stalled;
                    MonitorEvent::Stalled {
                        silent_periods: silent,
                    }
                } else {
                    MonitorEvent::None
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 端点状态机（失流 → 整块静音 / 新 epoch；fake 驱动，不打开设备）
// ---------------------------------------------------------------------------

/// 端点状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AoipState {
    /// 未启动（可 start）。
    Closed,
    /// 已请求启动但 PTP 未锁（等待 T19 servo 锁定）。
    WaitingPtp,
    /// 流动中（含等待首包；见 StreamMonitor）。
    Streaming,
    /// 停滞静音（同 epoch 可在数据恢复后继续）。
    Muted,
    /// 持续失流：必须新 epoch 并经 reopen 恢复。
    Lost,
}

impl AoipState {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            AoipState::Closed => "closed",
            AoipState::WaitingPtp => "waiting_ptp",
            AoipState::Streaming => "streaming",
            AoipState::Muted => "muted",
            AoipState::Lost => "lost",
        }
    }
}

/// 端点事件（host 据此整块静音/新 epoch；无半块半状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// 无状态变化。
    None,
    /// 开始流动（PTP 已锁 + 监视器启动）。
    Started,
    /// 停滞：立即整块静音；同 epoch 可在 Resumed 后继续。
    Stalled {
        /// 已静音 period 数。
        silent_periods: u32,
    },
    /// 持续失流：整块静音 + 必须新 epoch（reopen）。
    Lost {
        /// 已静音 period 数。
        silent_periods: u32,
        /// 建议的新 epoch 原因（T19 枚举；T43 不新增枚举值）。
        discontinuity: DiscontinuityReason,
    },
    /// 停滞期后数据恢复。
    Resumed {
        /// 本次停滞静音过的 period 数。
        silent_periods: u32,
    },
    /// 运行中 PTP 失锁：立即失流并请求新 epoch（PtpTimeout）。
    PtpLost {
        /// 建议的新 epoch 原因。
        discontinuity: DiscontinuityReason,
    },
    /// 已用新 epoch 恢复。
    Reopened {
        /// 新 epoch id。
        epoch_id: u64,
    },
}

/// 端点运行计数（非 RT 可读；单调累加，不静默清零）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AoipCounters {
    /// 数据回调次数。
    pub data_events: u64,
    /// 累计帧数。
    pub frames: u64,
    /// 停滞事件数。
    pub stall_events: u32,
    /// 失流事件数。
    pub loss_events: u32,
    /// 整块静音事件数。
    pub mute_events: u32,
    /// 累计静音 period 数（按事件估算）。
    pub muted_periods: u32,
    /// 停滞恢复次数（同 epoch）。
    pub resume_events: u32,
    /// PTP 失锁次数。
    pub ptp_unlock_events: u32,
    /// 新 epoch 重开次数。
    pub epoch_reopens: u32,
    /// 被拒绝的非法调用次数（状态机/epoch/参数）。
    pub rejected_calls: u32,
}

/// AoIP capture 端点（单一 reader；失流 fake 状态机；不持有/打开真实设备）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AoipEndpoint<'a> {
    spec: AoipEndpointSpec<'a>,
    state: AoipState,
    epoch_id: u64,
    monitor: StreamMonitor,
    counters: AoipCounters,
    needs_new_epoch: bool,
}

impl<'a> AoipEndpoint<'a> {
    /// 新建端点（只校验规格；不打开设备、不需要 PTP 已锁）。
    pub fn new(spec: AoipEndpointSpec<'a>) -> Result<Self, AoipError> {
        spec.validate()?;
        let monitor = StreamMonitor::from_spec(&spec)?;
        Ok(Self {
            spec,
            state: AoipState::Closed,
            epoch_id: 0,
            monitor,
            counters: AoipCounters::default(),
            needs_new_epoch: false,
        })
    }

    /// 端点规格。
    pub const fn spec(&self) -> &AoipEndpointSpec<'a> {
        &self.spec
    }

    /// 当前状态。
    pub const fn state(&self) -> AoipState {
        self.state
    }

    /// 当前 epoch（0 = 未启动）。
    pub const fn epoch_id(&self) -> u64 {
        self.epoch_id
    }

    /// 运行计数。
    pub const fn counters(&self) -> &AoipCounters {
        &self.counters
    }

    /// 当前是否必须整块静音（未开始/等待 PTP/停滞/失流）。
    pub const fn mute_required(&self) -> bool {
        matches!(
            self.state,
            AoipState::Closed | AoipState::WaitingPtp | AoipState::Muted | AoipState::Lost
        )
    }

    /// 是否必须先新 epoch 才能恢复。
    pub const fn needs_new_epoch(&self) -> bool {
        self.needs_new_epoch
    }

    /// 已声明的唯一 PTP 来源（A15/K44）。
    pub const fn ptp_source(&self) -> PtpSource {
        self.spec.ptp_source
    }

    /// 开始采集：epoch_id > 0；PTP 未锁 → WaitingPtp 且返回可重试错误。
    pub fn start(
        &mut self,
        epoch_id: u64,
        lock: LockState,
        now_ns: u64,
    ) -> Result<StreamEvent, AoipError> {
        if self.state != AoipState::Closed {
            self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
            return Err(AoipError::DeviceBusy);
        }
        if epoch_id == 0 {
            self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
            return Err(AoipError::BadLayout);
        }
        self.epoch_id = epoch_id;
        self.needs_new_epoch = false;
        if !ptp_ready(lock) {
            self.state = AoipState::WaitingPtp;
            return Err(AoipError::PtpUnavailable);
        }
        self.monitor.start(now_ns);
        self.state = AoipState::Streaming;
        Ok(StreamEvent::Started)
    }

    /// PTP 锁状态变化：等待期锁上 → 开始流动；运行期失锁/Holdover → 失流（PtpTimeout）。
    pub fn on_ptp(&mut self, lock: LockState, now_ns: u64) -> Result<StreamEvent, AoipError> {
        match self.state {
            AoipState::WaitingPtp => {
                if ptp_ready(lock) {
                    self.monitor.start(now_ns);
                    self.state = AoipState::Streaming;
                    Ok(StreamEvent::Started)
                } else {
                    Ok(StreamEvent::None)
                }
            }
            AoipState::Streaming | AoipState::Muted => {
                if ptp_ready(lock) {
                    Ok(StreamEvent::None)
                } else {
                    self.counters.ptp_unlock_events =
                        self.counters.ptp_unlock_events.saturating_add(1);
                    self.counters.loss_events = self.counters.loss_events.saturating_add(1);
                    self.state = AoipState::Lost;
                    self.needs_new_epoch = true;
                    Ok(StreamEvent::PtpLost {
                        discontinuity: DiscontinuityReason::PtpTimeout,
                    })
                }
            }
            AoipState::Closed => {
                self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
                Err(AoipError::SwitchBusy)
            }
            AoipState::Lost => Ok(StreamEvent::None),
        }
    }

    /// 数据到达（帧数 > 0；Lost 状态必须先 reopen）。
    pub fn on_data(&mut self, now_ns: u64, frames: u32) -> Result<StreamEvent, AoipError> {
        match self.state {
            AoipState::Closed | AoipState::WaitingPtp => {
                self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
                return Err(AoipError::SwitchBusy);
            }
            AoipState::Lost => {
                self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
                return Err(AoipError::EpochMismatch);
            }
            AoipState::Streaming | AoipState::Muted => {}
        }
        match self.monitor.on_data(now_ns, frames)? {
            MonitorEvent::Started => {
                self.counters.data_events = self.counters.data_events.saturating_add(1);
                self.counters.frames = self.counters.frames.saturating_add(frames as u64);
                self.state = AoipState::Streaming;
                Ok(StreamEvent::Started)
            }
            MonitorEvent::None => {
                self.counters.data_events = self.counters.data_events.saturating_add(1);
                self.counters.frames = self.counters.frames.saturating_add(frames as u64);
                Ok(StreamEvent::None)
            }
            MonitorEvent::Resumed { silent_periods } => {
                self.counters.data_events = self.counters.data_events.saturating_add(1);
                self.counters.frames = self.counters.frames.saturating_add(frames as u64);
                self.counters.resume_events = self.counters.resume_events.saturating_add(1);
                self.state = AoipState::Streaming;
                Ok(StreamEvent::Resumed { silent_periods })
            }
            MonitorEvent::Stalled { .. } | MonitorEvent::Lost { .. } => {
                // 监视器的停滞/失流只由 poll 产生；on_data 不会返回这两类。
                Ok(StreamEvent::None)
            }
        }
    }

    /// 推进时间：停滞 → 整块静音；持续失流 → Lost + 新 epoch 要求。
    pub fn poll(&mut self, now_ns: u64) -> StreamEvent {
        match self.monitor.poll(now_ns) {
            MonitorEvent::Stalled { silent_periods } => {
                if self.state == AoipState::Streaming {
                    self.state = AoipState::Muted;
                    self.counters.stall_events = self.counters.stall_events.saturating_add(1);
                    self.counters.mute_events = self.counters.mute_events.saturating_add(1);
                    self.counters.muted_periods =
                        self.counters.muted_periods.saturating_add(silent_periods);
                    StreamEvent::Stalled { silent_periods }
                } else {
                    StreamEvent::None
                }
            }
            MonitorEvent::Lost { silent_periods } => {
                if self.state == AoipState::Streaming || self.state == AoipState::Muted {
                    self.state = AoipState::Lost;
                    self.needs_new_epoch = true;
                    self.counters.loss_events = self.counters.loss_events.saturating_add(1);
                    self.counters.mute_events = self.counters.mute_events.saturating_add(1);
                    self.counters.muted_periods =
                        self.counters.muted_periods.saturating_add(silent_periods);
                    StreamEvent::Lost {
                        silent_periods,
                        discontinuity: DiscontinuityReason::Explicit,
                    }
                } else {
                    StreamEvent::None
                }
            }
            MonitorEvent::None | MonitorEvent::Started | MonitorEvent::Resumed { .. } => {
                StreamEvent::None
            }
        }
    }

    /// 端点 EOF / 管道关闭：视为持续失流（需新 epoch）；重复 EOF 不重复计数。
    pub fn on_source_eof(&mut self, _now_ns: u64) -> StreamEvent {
        match self.state {
            AoipState::Streaming | AoipState::Muted | AoipState::WaitingPtp => {
                self.state = AoipState::Lost;
                self.needs_new_epoch = true;
                self.counters.loss_events = self.counters.loss_events.saturating_add(1);
                StreamEvent::Lost {
                    silent_periods: 0,
                    discontinuity: DiscontinuityReason::Explicit,
                }
            }
            _ => StreamEvent::None,
        }
    }

    /// 用新 epoch 恢复（要求状态 Lost、epoch_id > 当前、PTP 已锁）。
    pub fn reopen(
        &mut self,
        epoch_id: u64,
        lock: LockState,
        now_ns: u64,
    ) -> Result<StreamEvent, AoipError> {
        if self.state != AoipState::Lost {
            self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
            return Err(AoipError::SwitchBusy);
        }
        if epoch_id <= self.epoch_id {
            self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
            return Err(AoipError::EpochMismatch);
        }
        if !ptp_ready(lock) {
            self.counters.rejected_calls = self.counters.rejected_calls.saturating_add(1);
            return Err(AoipError::PtpUnavailable);
        }
        self.epoch_id = epoch_id;
        self.needs_new_epoch = false;
        self.monitor.reset();
        self.monitor.start(now_ns);
        self.state = AoipState::Streaming;
        self.counters.epoch_reopens = self.counters.epoch_reopens.saturating_add(1);
        Ok(StreamEvent::Reopened { epoch_id })
    }

    /// 停止采集（回到 Closed；计数保留；epoch 清零）。
    pub fn stop(&mut self) {
        self.state = AoipState::Closed;
        self.epoch_id = 0;
        self.needs_new_epoch = false;
        self.monitor.reset();
    }
}

// ---------------------------------------------------------------------------
// 内部解析工具（无分配；畸形输入返回 Err）
// ---------------------------------------------------------------------------

fn parse_u32_dec(text: &str) -> Option<u32> {
    if text.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add((byte - b'0') as u32)?;
    }
    Some(value)
}

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut count = 0usize;
    for part in text.split('.') {
        if count >= 4 {
            return None;
        }
        let value = parse_u32_dec(part)?;
        if value > 255 {
            return None;
        }
        octets[count] = value as u8;
        count += 1;
    }
    if count == 4 {
        Some(octets)
    } else {
        None
    }
}

fn is_multicast_ipv4(address: [u8; 4]) -> bool {
    // 224.0.0.0/4
    (address[0] & 0xf0) == 0xe0
}

fn parse_ptime_us(text: &str) -> Option<u32> {
    let (whole_text, fraction_text) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    let whole = parse_u32_dec(whole_text)?;
    let mut micros = whole.checked_mul(1_000)?;
    if let Some(fraction) = fraction_text {
        if fraction.is_empty() || fraction.len() > 3 {
            return None;
        }
        let value = parse_u32_dec(fraction)?;
        let scale = match fraction.len() {
            1 => 100,
            2 => 10,
            _ => 1,
        };
        micros = micros.checked_add(value.checked_mul(scale)?)?;
    }
    Some(micros)
}

fn parse_connection(value: &str) -> Result<(&str, Option<u8>), AoipError> {
    let value = value.trim();
    let rest = value
        .strip_prefix("IN IP4 ")
        .ok_or(AoipError::Unsupported)?;
    let (address, ttl) = match rest.split_once('/') {
        Some((address, ttl_text)) => {
            let parsed = parse_u32_dec(ttl_text).ok_or(AoipError::DeviceParams)?;
            if parsed == 0 || parsed > 255 {
                return Err(AoipError::DeviceParams);
            }
            (address, Some(parsed as u8))
        }
        None => (rest, None),
    };
    if parse_ipv4(address).is_none() {
        return Err(AoipError::DeviceParams);
    }
    Ok((address, ttl))
}

fn parse_media(value: &str) -> Result<(u16, u8), AoipError> {
    let mut fields = value.split_whitespace();
    let media = fields.next().ok_or(AoipError::DeviceParams)?;
    if media != "audio" {
        return Err(AoipError::Unsupported);
    }
    let port_field = fields.next().ok_or(AoipError::DeviceParams)?;
    let port_text = port_field.split('/').next().unwrap_or(port_field);
    let port = parse_u32_dec(port_text).ok_or(AoipError::DeviceParams)?;
    if port == 0 || port > 65_535 {
        return Err(AoipError::DeviceParams);
    }
    let protocol = fields.next().ok_or(AoipError::DeviceParams)?;
    if protocol != "RTP/AVP" {
        return Err(AoipError::Unsupported);
    }
    let format = fields.next().ok_or(AoipError::DeviceParams)?;
    if fields.next().is_some() {
        // 多 payload/多 format 的会话不由本模型接受（单 flow 单格式）。
        return Err(AoipError::Unsupported);
    }
    let payload = parse_u32_dec(format).ok_or(AoipError::DeviceParams)?;
    if payload > 127 {
        return Err(AoipError::DeviceParams);
    }
    Ok((port as u16, payload as u8))
}

fn parse_rtpmap(value: &str) -> Result<(u8, Aes67Encoding, u32, u16), AoipError> {
    let (payload_text, rest) = value.split_once(' ').ok_or(AoipError::DeviceParams)?;
    let payload = parse_u32_dec(payload_text).ok_or(AoipError::DeviceParams)?;
    if payload > 127 {
        return Err(AoipError::DeviceParams);
    }
    let mut fields = rest.split('/');
    let encoding_text = fields.next().ok_or(AoipError::DeviceParams)?;
    let encoding = match encoding_text {
        "L24" => Aes67Encoding::L24,
        "L16" => Aes67Encoding::L16,
        _ => return Err(AoipError::Unsupported),
    };
    let rate_text = fields.next().ok_or(AoipError::DeviceParams)?;
    let sample_rate_hz = parse_u32_dec(rate_text).ok_or(AoipError::DeviceParams)?;
    let channels = match fields.next() {
        Some(text) => {
            let parsed = parse_u32_dec(text).ok_or(AoipError::DeviceParams)?;
            if parsed > 65_535 {
                return Err(AoipError::DeviceParams);
            }
            // 0 通道是“语法可解析但语义非法”，由 validate() 映射为 ERR_CHANNELS。
            parsed as u16
        }
        None => 1,
    };
    if fields.next().is_some() {
        return Err(AoipError::DeviceParams);
    }
    Ok((payload as u8, encoding, sample_rate_hz, channels))
}

fn parse_ts_refclk(value: &str) -> Result<PtpRefClock, AoipError> {
    let rest = value.strip_prefix("ptp=").ok_or(AoipError::Unsupported)?;
    let (standard, rest) = rest.split_once(':').ok_or(AoipError::DeviceParams)?;
    if standard != "IEEE1588-2008" && standard != "IEEE1588-2019" {
        return Err(AoipError::Unsupported);
    }
    let (grandmaster, domain_text) = rest.split_once(':').ok_or(AoipError::DeviceParams)?;
    let domain = parse_u32_dec(domain_text).ok_or(AoipError::DeviceParams)?;
    if domain > 127 {
        return Err(AoipError::DeviceParams);
    }
    if !grandmaster_is_eui64(grandmaster) {
        return Err(AoipError::DeviceParams);
    }
    Ok(PtpRefClock {
        domain: domain as u8,
        grandmaster_present: true,
    })
}

fn grandmaster_is_eui64(text: &str) -> bool {
    let mut groups = 0usize;
    for part in text.split('-') {
        if part.len() != 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        groups += 1;
    }
    groups == 8
}

// ---------------------------------------------------------------------------
// 编译期断言：容量/位图/错误码映射（不依赖运行期）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(H16_LOGICAL_SLOTS.len() == H16_LOGICAL_CHANNELS as usize);
    assert!(H16_LOGICAL_CHANNELS == 12);
    assert!(H16_CONTAINER_CHANNELS == 16);
    assert!(AES67_MAX_CHANNELS_PER_FLOW > 0);
    assert!(DEFAULT_MAX_STALL_PERIODS > 0);
    assert!(STREAM_LOST_MULTIPLIER >= 1);
    assert!(H16_MAX_FLOW_CHANNELS <= AES67_MAX_CHANNELS_PER_FLOW as usize);
    assert!(BLOCK_SIZES[0] as u32 == 64 && BLOCK_SIZES[1] as u32 == 128);
    assert!(SAMPLE_RATES_HZ[0] == AES67_RATE_HZ);
    assert!(SAMPLE_RATES_HZ[1] == INFERNO_RATE_HZ);
};

#[cfg(test)]
mod aoip_fake_tests {
    include!("../tests/T43/aoip_fake.rs");
}
