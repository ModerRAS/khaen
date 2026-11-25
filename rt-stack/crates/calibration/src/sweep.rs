//! [NONRT] 新 host 逐输出扫频（T61） ——WBS T61 写域。
//!
//! 职责：校准 daemon 通过**新 host** 经 MOTU DAC 逐输出播放对数扫频（log sine sweep），
//! 生成可直接交给 playback 图/ALSA 写线程的 planar f32 block，并输出**公共 render frame/slot
//! 时间轴**（session/epoch/graph、每个 slot 的扫频起点与每遍起点 render frame），供 T62
//! UMIK 采集与 T49 公共时基重建消费。本模块是控制面/离线可测实现，**不是 RT 模块**：
//! 允许分配/格式化，不触碰声卡；真实 DAC 由 T17 backend + T26 集成。
//!
//! 契约来源：
//! - WBS T61 行（逐输出扫频、公共 render frame/slot、电平/ramp/保护/clip 停止/cancel、fake backend 逐段验证）。
//! - DSP-HOST-DECISION §2.1/§2.2（daemon 经新 host 逐输出扫频；UMIK 独立 USB ADC；公共 render frame/slot；
//!   可闻扫频安全电平/保护）。
//! - calibration-spec/ACCEPTANCE §0.2（同一次采集内全部通道依次播放、通道间静音、重复间隔）、
//!   §0.4（T1 电学环回固定 **−12 dBFS**、声学 T2/T4 默认 **−30 dBFS**、记录 kMainTrim）、
//!   §0.5（削波门：峰值 ≤ **−1 dBFS**，任何通道无数字削波）、§3（T1 partial 逐通道映射）。
//! - calibration-spec/CONTRACT §2.2（measurement.sweep：start_hz/end_hz/seconds/level_db/fade_ms、
//!   pre_seconds/post_seconds/repeats）。
//! - core-v1 §6.1/§6.2（block 身份/字段/flags：VALID/DRAIN_TAIL/EPOCH_FIRST）、§3.2/§3.3（48k/96k、
//!   B∈{64,128}、SPSC 4 槽）、§5.4（校准 owner 独占；本模块不打开设备）。
//! - T48 modal-design/src/ingest.rs 的 session/stimulus/diag 字段与 T49 timebase 的
//!   render_frame_index/repeat_render_frames 语义：扫频起点 = host render frame（>0），
//!   slot 间隔期望 = repeats × (sweep_seconds + repeat_gap_seconds) × fs。
//!
//! 设计要点（可离线 fake backend 逐段验证）：
//! 1. **逐输出**：一次会话内按 outputs 顺序逐输出、逐 repeat 播放；同一时刻只有目标 DAC 通道
//!    有信号，其它通道严格为零（保护发射/串扰测量语义）。
//! 2. **公共 render frame/slot**：block 序连续 block_seq/frame_index = block_seq × B，
//!    ring slot 循环 0..3；timeline 记录每个 slot 的扫频起点 render_frame_index 与
//!    repeat_render_frames（纯 render frame 域，不含 UMIK capture 域）。
//! 3. **电平/trim**：驱动电平 −12 dBFS（T1 环回）/ −30 dBFS（声学）为默认；kMainTrim 必须
//!    显式记录，TrimPolicy::RecordOnly 只记录（数字域电平即驱动电平），
//!    TrimPolicy::Compensate 用 level − trim 补偿模拟/硬件 trim，但**不得突破 −1 dBFS 峰值门**。
//! 4. **ramp**：扫频起止用 raised-cosine 淡入/淡出，首末样本严格为 0；取消/clip 停止时生成
//!    递减淡出块（DRAIN_TAIL），避免爆音。
//! 5. **保护/clip 停止**：生成峰值在设计阶段即校验 ≤ peak_ceiling_dbfs（默认 −1 dBFS）；
//!    playback/format 层回报 clipped_samples > 0 时（abort_on_clip=true 默认）立即停止并
//!    报告 ClipStopped，绝不继续输出。
//! 6. **cancel**：每个 block 边界检查 job::CancelToken；扫频中取消先淡出再停，报告
//!    Cancelled（映射 T60 StageFailure::cancelled）。
//!
//! 未实现/不越界：不打开 ALSA、不写 /dev/snd、不启动外部进程、不做 UMIK 采集（T62）、
//! 不做拟合/求解/应用（T63–T66）、不解析 layout YAML（T57 提供 outputs）。
//!
//! 写域：WBS T61 = 本文件 + tests/T61 + evidence/T61；本模块不注册公共 API（lib.rs 已由 T06 注册）。

use std::fmt;
use std::fmt::Write as _;

use tspi_dsp_core::abi::{TspiBlockDescV1, ABI_VERSION};
use tspi_dsp_core::rt::{
    BLOCK_SIZES, MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, SAMPLE_RATES_HZ,
};

use crate::job::{CancelToken, StageFailure};

// ---------------------------------------------------------------------------
// 常量（WBS T61 / ACCEPTANCE / core-v1）
// ---------------------------------------------------------------------------

/// 扫频 session/timeline schema（本文件产物；T48/T49 消费的 stimulus/diag 子集）。
pub const SWEEP_SCHEMA: &str = "tspi-sweep-timeline/v1";

/// T1 电学环回固定驱动电平（ACCEPTANCE §0.4.1，不得再用 −30 dBFS）。
pub const T1_LOOPBACK_LEVEL_DBFS: f64 = -12.0;
/// 声学 T2/T4 默认驱动电平（ACCEPTANCE §0.4.1；SNR <25 dB 时提高到 −12 dBFS 重测）。
pub const ACOUSTIC_LEVEL_DBFS: f64 = -30.0;
/// 测量峰值门：扫频/节目素材峰值 ≤ −1 dBFS（ACCEPTANCE §0.5.1/§5 削波门）。
pub const MEASUREMENT_PEAK_CEILING_DBFS: f64 = -1.0;
/// 允许的驱动电平范围（JobRequest.drive_level_dbfs 同口径 −60..=0）。
pub const MIN_DRIVE_LEVEL_DBFS: f64 = -60.0;
/// 允许的驱动电平上限（数字域满刻度；实际保护另受峰值门约束）。
pub const MAX_DRIVE_LEVEL_DBFS: f64 = 0.0;
/// 默认 fade（ms）；CONTRACT §2.2 示例 10 ms。
pub const DEFAULT_FADE_MS: f64 = 10.0;
/// 环回会话的默认遍间静音（秒）；ACCEPTANCE §0.2.4 参考 0.2 s 槽间静音。
pub const LOOPBACK_REPEAT_GAP_SECONDS: f64 = 0.2;
/// 声学会话的默认遍间静音（秒）；ACCEPTANCE §0.2.1 通道间 ≥0.5 s 静音。
pub const ACOUSTIC_REPEAT_GAP_SECONDS: f64 = 0.5;
/// 前静音最短（秒）：保证 T48 diag 的 render_frame_index > 0。
pub const MIN_PRE_SECONDS: f64 = 0.1;
/// 后静音最短（秒）。
pub const MIN_POST_SECONDS: f64 = 0.1;
/// 单遍扫频时长范围（秒）。
pub const MIN_SWEEP_SECONDS: f64 = 0.05;
/// 单遍扫频时长上限（秒；避免误配置长时间全功率发声）。
pub const MAX_SWEEP_SECONDS: f64 = 120.0;
/// 每 slot 重复次数上限（T48 StimulusInfo [1..=8] 的工程上界；JobRequest 允许 10）。
pub const MAX_REPEATS: u32 = 10;
/// 会话总时长上限（秒；保护/证据可控）。
pub const MAX_TOTAL_SECONDS: f64 = 3600.0;
/// playback ring 槽数（= core-v1 §3.3 SPSC_PLAYBACK_SLOTS）。
pub const PLAYBACK_RING_SLOTS: u64 = 4;

/// core-v1 §6.2 flags：VALID。
pub const BLOCK_FLAG_VALID: u32 = 1 << 0;
/// core-v1 §6.2 flags：DRAIN_TAIL（停流尾块；唯一允许 valid_frames < B 的块）。
pub const BLOCK_FLAG_DRAIN_TAIL: u32 = 1 << 1;
/// core-v1 §6.2 flags：EPOCH_FIRST（新 epoch 第一块）。
pub const BLOCK_FLAG_EPOCH_FIRST: u32 = 1 << 2;

/// 占位 graph_id（64 hex 非全零；仅 smoke/离线默认构造使用；生产必须传实际 plan graph hash）。
pub const PLACEHOLDER_GRAPH_ID: &str =
    "0000000000000000000000000000000000000000000000000000000000000001";

// ---------------------------------------------------------------------------
// 基础类型
// ---------------------------------------------------------------------------

/// 会话种类：决定默认电平、最小遍间静音与时长口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepSessionKind {
    /// T1 电学环回自检：−12 dBFS、gap ≥0.2 s。
    Loopback,
    /// T2/T4 声学：−30 dBFS（SNR 不足时可提高至 −12 dBFS）、gap ≥0.5 s。
    Acoustic,
}

impl SweepSessionKind {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            SweepSessionKind::Loopback => "loopback",
            SweepSessionKind::Acoustic => "acoustic",
        }
    }

    /// 默认驱动电平（dBFS）。
    pub const fn default_level_dbfs(self) -> f64 {
        match self {
            SweepSessionKind::Loopback => T1_LOOPBACK_LEVEL_DBFS,
            SweepSessionKind::Acoustic => ACOUSTIC_LEVEL_DBFS,
        }
    }

    /// 最小遍间静音（秒）。
    pub const fn min_repeat_gap_seconds(self) -> f64 {
        match self {
            SweepSessionKind::Loopback => LOOPBACK_REPEAT_GAP_SECONDS,
            SweepSessionKind::Acoustic => ACOUSTIC_REPEAT_GAP_SECONDS,
        }
    }
}

/// kMainTrim 口径：只记录，或在数字域补偿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimPolicy {
    /// 数字域电平 = drive_level_dbfs；trim 只记录（ACCEPTANCE §0.4.2 默认，推荐 trim=0）。
    RecordOnly,
    /// 数字域电平 = drive_level_dbfs − k_main_trim_db；仍受峰值门约束，超门拒绝。
    Compensate,
}

impl TrimPolicy {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            TrimPolicy::RecordOnly => "record_only",
            TrimPolicy::Compensate => "compensate",
        }
    }
}

/// 激励类型（T61 只实现对数扫频；T48 允许的其它类型不由本模块生成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepKind {
    /// 对数正弦扫频（log sine sweep）。
    LogSweep,
}

impl SweepKind {
    /// 机器名（= T48 stimulus.kind）。
    pub const fn as_str(self) -> &'static str {
        match self {
            SweepKind::LogSweep => "log_sweep",
        }
    }
}

/// 一个被测输出（layout 解析结果由 T57 提供；本模块不猜 role）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOutput {
    /// layout 容器槽位（0-based；T48 diag 的 slot）。
    pub slot: u32,
    /// 角色 id（^[A-Z][A-Z0-9]*$；T48 diag 的 speaker）。
    pub speaker: String,
    /// 播放设备通道号（0-based；= T48 diag 的 channel / out_index）。
    pub channel: u32,
}

impl SweepOutput {
    /// 构造。
    pub fn new(slot: u32, speaker: impl Into<String>, channel: u32) -> Self {
        SweepOutput {
            slot,
            speaker: speaker.into(),
            channel,
        }
    }
}

/// 扫频参数（对应 CONTRACT §2.2 measurement.sweep + T48 StimulusInfo 子集）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepStimulus {
    /// 激励类型。
    pub kind: SweepKind,
    /// 起始频率（Hz）。
    pub start_hz: f64,
    /// 结束频率（Hz，≤ min(20000, fs/2−1)）。
    pub end_hz: f64,
    /// 单遍名义时长（秒；实际按 block 对齐，见 SweepActualStimulus）。
    pub seconds: f64,
    /// 数字驱动电平（dBFS，−60..=0；实际生效见 SweepLevelInfo）。
    pub level_dbfs: f64,
    /// fade 时长（ms）。
    pub fade_ms: f64,
    /// 每 slot 重复次数（1..=10）。
    pub repeats: u32,
    /// 遍间静音（秒；实际按 block 对齐）。
    pub repeat_gap_seconds: f64,
}

impl SweepStimulus {
    /// 完整构造。
    pub fn log_sweep(
        start_hz: f64,
        end_hz: f64,
        seconds: f64,
        level_dbfs: f64,
        fade_ms: f64,
        repeats: u32,
        repeat_gap_seconds: f64,
    ) -> Self {
        SweepStimulus {
            kind: SweepKind::LogSweep,
            start_hz,
            end_hz,
            seconds,
            level_dbfs,
            fade_ms,
            repeats,
            repeat_gap_seconds,
        }
    }

    /// T1 环回默认：20 Hz–20 kHz / 5 s / −12 dBFS / 10 ms fade / 1 遍 / gap 0.2 s。
    pub fn loopback_default() -> Self {
        Self::log_sweep(
            20.0,
            20_000.0,
            5.0,
            T1_LOOPBACK_LEVEL_DBFS,
            DEFAULT_FADE_MS,
            1,
            LOOPBACK_REPEAT_GAP_SECONDS,
        )
    }

    /// T2/T4 声学默认：20 Hz–20 kHz / 5 s / −30 dBFS / 10 ms fade / 3 遍 / gap 0.5 s。
    pub fn acoustic_default() -> Self {
        Self::log_sweep(
            20.0,
            20_000.0,
            5.0,
            ACOUSTIC_LEVEL_DBFS,
            DEFAULT_FADE_MS,
            3,
            ACOUSTIC_REPEAT_GAP_SECONDS,
        )
    }
}

/// 输出保护（生成阶段硬校验 + playback clip 回报停止）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepProtection {
    /// 峰值门（dBFS；默认 −1.0；必须 <0 且 ≥−20）。
    pub peak_ceiling_dbfs: f64,
    /// playback/format 层回报 clipped >0 时是否立即停止（默认 true）。
    pub abort_on_clip: bool,
}

impl SweepProtection {
    /// 构造。
    pub const fn new(peak_ceiling_dbfs: f64, abort_on_clip: bool) -> Self {
        SweepProtection {
            peak_ceiling_dbfs,
            abort_on_clip,
        }
    }
}

impl Default for SweepProtection {
    fn default() -> Self {
        Self::new(MEASUREMENT_PEAK_CEILING_DBFS, true)
    }
}

/// 扫频会话配置（一次 T1/T2/T4 测量会话对应一个 config）。
#[derive(Debug, Clone, PartialEq)]
pub struct SweepConfig {
    /// session 唯一 id（进入 timeline/T48 diag）。
    pub session_id: String,
    /// host epoch id（>0；T48 diag 公共 render frame 基准）。
    pub epoch_id: u64,
    /// host graph id（64 lowercase hex、非全零；测量图版本）。
    pub graph_id: String,
    /// layout id（例如 4.1.2；实际 role→DAC 解析归 T57）。
    pub layout: String,
    /// 会话种类。
    pub session_kind: SweepSessionKind,
    /// 采样率（48k/96k）。
    pub sample_rate: u32,
    /// block 帧数（64/128；必须 = ALSA actual period）。
    pub block_frames: u16,
    /// 播放设备通道数（1..=22；输出 buffer 全通道写零/单通道信号）。
    pub playback_channels: u16,
    /// block 描述里的输入通道数（playback-only 可为 0；T26 接入时按实际 capture 改写）。
    pub n_in: u16,
    /// 逐输出（顺序 = 播放顺序）。
    pub outputs: Vec<SweepOutput>,
    /// 扫频参数。
    pub stimulus: SweepStimulus,
    /// MOTU/CueMix 主输出 trim（dB；必须记录，最好为 0）。
    pub k_main_trim_db: f64,
    /// trim 口径。
    pub trim_policy: TrimPolicy,
    /// 扫频前静音（秒；≥0.1）。
    pub pre_seconds: f64,
    /// 扫频后静音（秒；≥0.1）。
    pub post_seconds: f64,
    /// 输出保护。
    pub protection: SweepProtection,
    /// epoch 起点单调时刻（ns；离线/测试可为 0）。
    pub epoch_start_mono_ns: u64,
    /// playback deadline 预算（ns；0 = 默认一个 block 时长）。
    pub block_deadline_ns: u64,
    /// 测量图 plan 版本（>0；写入 block desc）。
    pub plan_version: u32,
}

impl SweepConfig {
    /// T1 环回会话构造（−12 dBFS、gap 0.2 s、pre/post 1 s、trim 记录为 0）。
    pub fn loopback(session_id: impl Into<String>, outputs: Vec<SweepOutput>) -> Self {
        Self::new(
            SweepSessionKind::Loopback,
            session_id,
            outputs,
            SweepStimulus::loopback_default(),
        )
    }

    /// 声学会话构造（−30 dBFS、gap 0.5 s、pre/post 1 s、trim 记录为 0）。
    pub fn acoustic(session_id: impl Into<String>, outputs: Vec<SweepOutput>) -> Self {
        Self::new(
            SweepSessionKind::Acoustic,
            session_id,
            outputs,
            SweepStimulus::acoustic_default(),
        )
    }

    fn new(
        kind: SweepSessionKind,
        session_id: impl Into<String>,
        outputs: Vec<SweepOutput>,
        stimulus: SweepStimulus,
    ) -> Self {
        let playback_channels = outputs
            .iter()
            .map(|output| output.channel)
            .max()
            .unwrap_or(0)
            .saturating_add(1) as u16;
        SweepConfig {
            session_id: session_id.into(),
            epoch_id: 1,
            graph_id: PLACEHOLDER_GRAPH_ID.to_string(),
            layout: "4.1.2".to_string(),
            session_kind: kind,
            sample_rate: 48_000,
            block_frames: 128,
            playback_channels: playback_channels.max(2),
            n_in: 1,
            outputs,
            stimulus,
            k_main_trim_db: 0.0,
            trim_policy: TrimPolicy::RecordOnly,
            pre_seconds: 1.0,
            post_seconds: 1.0,
            protection: SweepProtection::default(),
            epoch_start_mono_ns: 0,
            block_deadline_ns: 0,
            plan_version: 1,
        }
    }

    /// 只做校验（不运行）；错误与 compile_timeline 完全一致。
    pub fn validate(&self) -> Result<(), SweepError> {
        compile_timeline(self).map(|_| ())
    }
}

/// 实际（block 对齐后）激励参数；T48 session.stimulus / T49 期望间隔必须用这些 actual 值
/// （否则 repeat 间隔期望会因 block 取整产生 >2 sample 的残差）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepActualStimulus {
    /// 实际单遍时长（= sweep_frames / fs）。
    pub seconds: f64,
    /// 实际遍间静音（= gap_frames / fs）。
    pub repeat_gap_seconds: f64,
    /// 实际单遍帧数（block 倍数）。
    pub sweep_frames: u64,
    /// 实际遍间静音帧数（block 倍数）。
    pub gap_frames: u64,
    /// fade 帧数（未 block 对齐；只影响扫频两端）。
    pub fade_frames: u64,
}

/// 生效电平/trim 口径（报告/证据必须携带）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepLevelInfo {
    /// 请求的驱动电平（dBFS）。
    pub drive_level_dbfs: f64,
    /// 记录的 kMainTrim（dB）。
    pub k_main_trim_db: f64,
    /// trim 口径。
    pub trim_policy: TrimPolicy,
    /// 数字域实际生效电平（dBFS）。
    pub effective_level_dbfs: f64,
    /// 峰值门（dBFS）。
    pub peak_ceiling_dbfs: f64,
    /// 线性峰值幅度（= 10^(effective/20)）。
    pub peak_linear: f64,
}

/// 段类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepSegmentKind {
    /// 前静音。
    PreSilence,
    /// 扫频。
    Sweep,
    /// 遍间静音（每遍扫频之后）。
    RepeatGap,
    /// 后静音。
    PostSilence,
}

impl SweepSegmentKind {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            SweepSegmentKind::PreSilence => "pre_silence",
            SweepSegmentKind::Sweep => "sweep",
            SweepSegmentKind::RepeatGap => "repeat_gap",
            SweepSegmentKind::PostSilence => "post_silence",
        }
    }

    /// 是否为本会话的有效激励段。
    pub const fn is_sweep(self) -> bool {
        matches!(self, SweepSegmentKind::Sweep)
    }
}

/// 一个时间轴段（block 对齐；frames 必为 block 的整数倍）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepSegment {
    /// 段序号（0-based，按时间顺序）。
    pub index: u32,
    /// 段类型。
    pub kind: SweepSegmentKind,
    /// 输出下标（SweepTimeline.outputs 的下标；静音段为 None）。
    pub output_index: Option<u32>,
    /// repeat 序号（0-based；静音段为 None）。
    pub repeat: Option<u32>,
    /// 起始 render frame（= start_block_seq × B）。
    pub start_frame: u64,
    /// 段帧数（block 倍数）。
    pub frames: u64,
    /// 起始 block 序号。
    pub start_block_seq: u64,
    /// 段 block 数。
    pub blocks: u64,
    /// 扫频信号帧数（仅 Sweep 段 = frames）。
    pub sweep_frames: u64,
    /// fade 帧数（仅 Sweep 段；淡入/淡出各 fade_frames）。
    pub fade_frames: u64,
}

impl SweepSegment {
    /// 结束帧（exclusive）。
    pub const fn end_frame(&self) -> u64 {
        self.start_frame + self.frames
    }
}

/// 一个 slot 的公共 render frame 时间轴（T48/T49 消费）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepSlotTimeline {
    /// layout 容器槽位。
    pub slot: u32,
    /// 角色 id。
    pub speaker: String,
    /// 播放设备通道号。
    pub channel: u32,
    /// 本次 slot 首遍扫频起点 render frame（>0）。
    pub render_frame_index: u64,
    /// 每遍扫频起点 render frame（长度 = repeats；严格递增）。
    pub repeat_render_frames: Vec<u64>,
}

/// 公共 render frame/slot 时间轴（本模块核心交付物之一）。
#[derive(Debug, Clone, PartialEq)]
pub struct SweepTimeline {
    /// session id。
    pub session_id: String,
    /// 会话种类。
    pub session_kind: SweepSessionKind,
    /// layout id。
    pub layout: String,
    /// host epoch id（>0）。
    pub epoch_id: u64,
    /// host graph id（64 hex 非全零）。
    pub graph_id: String,
    /// 测量图 plan 版本。
    pub plan_version: u32,
    /// 采样率。
    pub sample_rate: u32,
    /// block 帧数。
    pub block_frames: u16,
    /// 播放通道数。
    pub playback_channels: u16,
    /// block desc 输入通道数。
    pub n_in: u16,
    /// 请求的激励参数。
    pub stimulus: SweepStimulus,
    /// 实际（block 对齐后）激励参数（T48 session 必须记录这些值）。
    pub actual_stimulus: SweepActualStimulus,
    /// 电平/trim 口径。
    pub level: SweepLevelInfo,
    /// 保护配置。
    pub protection: SweepProtection,
    /// 前静音帧数。
    pub pre_frames: u64,
    /// 后静音帧数。
    pub post_frames: u64,
    /// 总帧数（block 倍数）。
    pub total_frames: u64,
    /// 总 block 数。
    pub total_blocks: u64,
    /// epoch 起点单调时刻（ns）。
    pub epoch_start_mono_ns: u64,
    /// playback deadline 预算（ns）。
    pub block_deadline_ns: u64,
    /// 逐输出配置（顺序 = 播放顺序）。
    pub outputs: Vec<SweepOutput>,
    /// 逐 slot 公共 render frame 时间轴。
    pub slots: Vec<SweepSlotTimeline>,
    /// 全部时间轴段（block 对齐）。
    pub segments: Vec<SweepSegment>,
}

impl SweepTimeline {
    /// 首段扫频起点 render frame（用于确认 T48 diag render_frame_index > 0）。
    pub fn first_sweep_render_frame(&self) -> Option<u64> {
        self.slots.first().map(|slot| slot.render_frame_index)
    }

    /// 逐 slot 期望间隔帧数（T49：repeats × (actual sweep + actual gap) × fs）。
    pub fn expected_slot_interval_frames(&self) -> u64 {
        self.stimulus.repeats as u64
            * (self.actual_stimulus.sweep_frames + self.actual_stimulus.gap_frames)
    }

    /// 输出通道号（按输出下标）。
    pub fn output_channel(&self, output_index: usize) -> Option<u32> {
        self.outputs.get(output_index).map(|output| output.channel)
    }

    /// 时间轴 JSON（确定性；无第三方依赖）。T62/报告可直接写 artifact。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(4096);
        out.push('{');
        out.push_str("\"schema\":\"");
        out.push_str(SWEEP_SCHEMA);
        out.push_str("\",\"session_id\":");
        push_json_str(&mut out, &self.session_id);
        out.push_str(",\"session_kind\":");
        push_json_str(&mut out, self.session_kind.as_str());
        out.push_str(",\"layout\":");
        push_json_str(&mut out, &self.layout);
        let _ = write!(out, ",\"epoch_id\":{},\"graph_id\":", self.epoch_id);
        push_json_str(&mut out, &self.graph_id);
        let _ = write!(
            out,
            ",\"plan_version\":{},\"sample_rate\":{},\"block_frames\":{},\"playback_channels\":{},\"n_in\":{}",
            self.plan_version,
            self.sample_rate,
            self.block_frames,
            self.playback_channels,
            self.n_in
        );
        out.push_str(",\"stimulus\":{\"kind\":");
        push_json_str(&mut out, self.stimulus.kind.as_str());
        let _ = write!(
            out,
            ",\"start_hz\":{},\"end_hz\":{},\"seconds\":{},\"level_dbfs\":{},\"fade_ms\":{},\"repeats\":{},\"repeat_gap_seconds\":{}}}",
            json_number(self.stimulus.start_hz),
            json_number(self.stimulus.end_hz),
            json_number(self.stimulus.seconds),
            json_number(self.stimulus.level_dbfs),
            json_number(self.stimulus.fade_ms),
            self.stimulus.repeats,
            json_number(self.stimulus.repeat_gap_seconds)
        );
        let _ = write!(
            out,
            ",\"actual_stimulus\":{{\"seconds\":{},\"repeat_gap_seconds\":{},\"sweep_frames\":{},\"gap_frames\":{},\"fade_frames\":{}}}",
            json_number(self.actual_stimulus.seconds),
            json_number(self.actual_stimulus.repeat_gap_seconds),
            self.actual_stimulus.sweep_frames,
            self.actual_stimulus.gap_frames,
            self.actual_stimulus.fade_frames
        );
        let _ = write!(
            out,
            ",\"level\":{{\"drive_level_dbfs\":{},\"k_main_trim_db\":{},\"trim_policy\":{},\"effective_level_dbfs\":{},\"peak_ceiling_dbfs\":{},\"peak_linear\":{}}}",
            json_number(self.level.drive_level_dbfs),
            json_number(self.level.k_main_trim_db),
            quoted(self.level.trim_policy.as_str()),
            json_number(self.level.effective_level_dbfs),
            json_number(self.level.peak_ceiling_dbfs),
            json_number(self.level.peak_linear)
        );
        let _ = write!(
            out,
            ",\"protection\":{{\"peak_ceiling_dbfs\":{},\"abort_on_clip\":{}}}",
            json_number(self.protection.peak_ceiling_dbfs),
            self.protection.abort_on_clip
        );
        let _ = write!(
            out,
            ",\"timeline\":{{\"render_start_frame\":0,\"pre_frames\":{},\"post_frames\":{},\"sweep_frames\":{},\"gap_frames\":{},\"total_frames\":{},\"total_blocks\":{},\"ring_slots\":{},\"epoch_start_mono_ns\":{},\"block_deadline_ns\":{}}}",
            self.pre_frames,
            self.post_frames,
            self.actual_stimulus.sweep_frames,
            self.actual_stimulus.gap_frames,
            self.total_frames,
            self.total_blocks,
            PLAYBACK_RING_SLOTS,
            self.epoch_start_mono_ns,
            self.block_deadline_ns
        );
        out.push_str(",\"outputs\":[");
        for (index, output) in self.outputs.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "{{\"slot\":{},\"speaker\":", output.slot);
            push_json_str(&mut out, &output.speaker);
            let _ = write!(out, ",\"channel\":{}}}", output.channel);
        }
        out.push_str("],\"slots\":[");
        for (index, slot) in self.slots.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "{{\"slot\":{},\"speaker\":", slot.slot);
            push_json_str(&mut out, &slot.speaker);
            let _ = write!(
                out,
                ",\"channel\":{},\"render_frame_index\":{},\"repeat_render_frames\":[",
                slot.channel, slot.render_frame_index
            );
            for (repeat, frame) in slot.repeat_render_frames.iter().enumerate() {
                if repeat > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{frame}");
            }
            out.push_str("]}");
        }
        out.push_str("],\"segments\":[");
        for (index, segment) in self.segments.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"index\":{},\"kind\":{},\"output_index\":{},\"repeat\":{},\"start_frame\":{},\"frames\":{},\"start_block_seq\":{},\"blocks\":{},\"sweep_frames\":{},\"fade_frames\":{}}}",
                segment.index,
                quoted(segment.kind.as_str()),
                json_option_u32(segment.output_index),
                json_option_u32(segment.repeat),
                segment.start_frame,
                segment.frames,
                segment.start_block_seq,
                segment.blocks,
                segment.sweep_frames,
                segment.fade_frames
            );
        }
        out.push_str("]}");
        out
    }

    fn empty() -> Self {
        SweepTimeline {
            session_id: String::new(),
            session_kind: SweepSessionKind::Loopback,
            layout: String::new(),
            epoch_id: 0,
            graph_id: String::new(),
            plan_version: 0,
            sample_rate: 0,
            block_frames: 0,
            playback_channels: 0,
            n_in: 0,
            stimulus: SweepStimulus::loopback_default(),
            actual_stimulus: SweepActualStimulus {
                seconds: 0.0,
                repeat_gap_seconds: 0.0,
                sweep_frames: 0,
                gap_frames: 0,
                fade_frames: 0,
            },
            level: SweepLevelInfo {
                drive_level_dbfs: 0.0,
                k_main_trim_db: 0.0,
                trim_policy: TrimPolicy::RecordOnly,
                effective_level_dbfs: 0.0,
                peak_ceiling_dbfs: MEASUREMENT_PEAK_CEILING_DBFS,
                peak_linear: 0.0,
            },
            protection: SweepProtection::default(),
            pre_frames: 0,
            post_frames: 0,
            total_frames: 0,
            total_blocks: 0,
            epoch_start_mono_ns: 0,
            block_deadline_ns: 0,
            outputs: Vec::new(),
            slots: Vec::new(),
            segments: Vec::new(),
        }
    }
}

/// 编译配置 -> 公共 render frame/slot 时间轴（纯函数；T62/T66 可独立调用）。
pub fn compile_timeline(config: &SweepConfig) -> Result<SweepTimeline, SweepError> {
    validate_config(config)?;

    let fs = config.sample_rate as f64;
    let block = config.block_frames as u64;
    let sweep_frames = round_frames(config.stimulus.seconds, fs, block);
    let gap_frames = round_frames(config.stimulus.repeat_gap_seconds, fs, block);
    let pre_frames = round_frames(config.pre_seconds, fs, block);
    let post_frames = round_frames(config.post_seconds, fs, block);
    let fade_frames = (config.stimulus.fade_ms * fs / 1000.0).round() as u64;
    if fade_frames < 2 {
        return Err(invalid(
            "stimulus.fade_ms",
            "fade must be at least 2 samples after rounding",
        ));
    }
    if fade_frames.saturating_mul(2) > sweep_frames {
        return Err(invalid(
            "stimulus.fade_ms",
            "fade in+out exceeds the sweep duration",
        ));
    }

    let ceiling_linear = 10f64.powf(config.protection.peak_ceiling_dbfs / 20.0);
    let effective_level_dbfs = match config.trim_policy {
        TrimPolicy::RecordOnly => config.stimulus.level_dbfs,
        TrimPolicy::Compensate => config.stimulus.level_dbfs - config.k_main_trim_db,
    };
    let peak_linear = 10f64.powf(effective_level_dbfs / 20.0);
    if peak_linear > ceiling_linear {
        return Err(invalid(
            "level",
            format!(
                "effective level {effective_level_dbfs:.3} dBFS exceeds peak ceiling {:.3} dBFS",
                config.protection.peak_ceiling_dbfs
            ),
        ));
    }
    let block_deadline_ns = if config.block_deadline_ns == 0 {
        block_deadline_ns_of(config.block_frames, config.sample_rate)
    } else {
        config.block_deadline_ns
    };

    let mut timeline = SweepTimeline::empty();
    timeline.session_id = config.session_id.clone();
    timeline.session_kind = config.session_kind;
    timeline.layout = config.layout.clone();
    timeline.epoch_id = config.epoch_id;
    timeline.graph_id = config.graph_id.clone();
    timeline.plan_version = config.plan_version;
    timeline.sample_rate = config.sample_rate;
    timeline.block_frames = config.block_frames;
    timeline.playback_channels = config.playback_channels;
    timeline.n_in = config.n_in;
    timeline.stimulus = config.stimulus;
    timeline.actual_stimulus = SweepActualStimulus {
        seconds: sweep_frames as f64 / fs,
        repeat_gap_seconds: gap_frames as f64 / fs,
        sweep_frames,
        gap_frames,
        fade_frames,
    };
    timeline.level = SweepLevelInfo {
        drive_level_dbfs: config.stimulus.level_dbfs,
        k_main_trim_db: config.k_main_trim_db,
        trim_policy: config.trim_policy,
        effective_level_dbfs,
        peak_ceiling_dbfs: config.protection.peak_ceiling_dbfs,
        peak_linear,
    };
    timeline.protection = config.protection;
    timeline.pre_frames = pre_frames;
    timeline.post_frames = post_frames;
    timeline.epoch_start_mono_ns = config.epoch_start_mono_ns;
    timeline.block_deadline_ns = block_deadline_ns;
    timeline.outputs = config.outputs.clone();

    let mut frame = 0u64;
    let mut slot_timelines: Vec<SweepSlotTimeline> = Vec::with_capacity(config.outputs.len());
    let mut segments: Vec<SweepSegment> = Vec::new();
    fn push_segment(
        segments: &mut Vec<SweepSegment>,
        frame: &mut u64,
        block: u64,
        kind: SweepSegmentKind,
        output_index: Option<u32>,
        repeat: Option<u32>,
        frames: u64,
        sweep_frames: u64,
        fade_frames: u64,
    ) {
        let segment = SweepSegment {
            index: segments.len() as u32,
            kind,
            output_index,
            repeat,
            start_frame: *frame,
            frames,
            start_block_seq: *frame / block,
            blocks: frames / block,
            sweep_frames,
            fade_frames,
        };
        *frame += frames;
        segments.push(segment);
    }

    push_segment(
        &mut segments,
        &mut frame,
        block,
        SweepSegmentKind::PreSilence,
        None,
        None,
        pre_frames,
        0,
        0,
    );
    for (output_index, output) in config.outputs.iter().enumerate() {
        let mut repeat_frames = Vec::with_capacity(config.stimulus.repeats as usize);
        for repeat in 0..config.stimulus.repeats {
            repeat_frames.push(frame);
            push_segment(
                &mut segments,
                &mut frame,
                block,
                SweepSegmentKind::Sweep,
                Some(output_index as u32),
                Some(repeat),
                sweep_frames,
                sweep_frames,
                fade_frames,
            );
            push_segment(
                &mut segments,
                &mut frame,
                block,
                SweepSegmentKind::RepeatGap,
                Some(output_index as u32),
                Some(repeat),
                gap_frames,
                0,
                0,
            );
        }
        let render_frame_index = repeat_frames.first().copied().unwrap_or(0);
        if render_frame_index == 0 {
            return Err(invalid(
                "pre_seconds",
                "first sweep must start after a non-empty pre-silence (render_frame_index > 0 for T48 diag)",
            ));
        }
        slot_timelines.push(SweepSlotTimeline {
            slot: output.slot,
            speaker: output.speaker.clone(),
            channel: output.channel,
            render_frame_index,
            repeat_render_frames: repeat_frames,
        });
    }
    push_segment(
        &mut segments,
        &mut frame,
        block,
        SweepSegmentKind::PostSilence,
        None,
        None,
        post_frames,
        0,
        0,
    );

    let total_frames = frame;
    if total_frames == 0 || total_frames > (MAX_TOTAL_SECONDS * fs) as u64 + block {
        return Err(invalid(
            "total",
            "session duration exceeds the configured cap",
        ));
    }
    timeline.total_frames = total_frames;
    timeline.total_blocks = total_frames / block;
    timeline.slots = slot_timelines;
    timeline.segments = segments;
    Ok(timeline)
}

fn validate_config(config: &SweepConfig) -> Result<(), SweepError> {
    validate_text("session_id", &config.session_id, 64)?;
    validate_text("layout", &config.layout, 64)?;
    if config.epoch_id == 0 {
        return Err(invalid("epoch_id", "epoch_id must be > 0"));
    }
    validate_graph_id(&config.graph_id)?;
    if config.plan_version == 0 {
        return Err(invalid("plan_version", "plan_version must be > 0"));
    }
    if !SAMPLE_RATES_HZ.contains(&config.sample_rate) {
        return Err(invalid(
            "sample_rate",
            format!(
                "unsupported sample_rate {} (expected 48000 or 96000)",
                config.sample_rate
            ),
        ));
    }
    if !BLOCK_SIZES.contains(&config.block_frames) {
        return Err(invalid(
            "block_frames",
            format!(
                "unsupported block_frames {} (expected 64 or 128)",
                config.block_frames
            ),
        ));
    }
    if config.playback_channels == 0 || config.playback_channels as usize > MAX_OUTPUT_CHANNELS {
        return Err(invalid(
            "playback_channels",
            format!(
                "playback_channels {} outside 1..={MAX_OUTPUT_CHANNELS}",
                config.playback_channels
            ),
        ));
    }
    if config.n_in as usize > MAX_INPUT_CHANNELS {
        return Err(invalid(
            "n_in",
            format!("n_in {} exceeds {MAX_INPUT_CHANNELS}", config.n_in),
        ));
    }
    if config.outputs.is_empty() {
        return Err(invalid("outputs", "at least one output is required"));
    }
    if config.outputs.len() > MAX_OUTPUT_ROLES {
        return Err(invalid(
            "outputs",
            format!(
                "{} outputs exceed MAX_OUTPUT_ROLES={MAX_OUTPUT_ROLES}",
                config.outputs.len()
            ),
        ));
    }
    let mut slots: Vec<u32> = Vec::with_capacity(config.outputs.len());
    let mut channels: Vec<u32> = Vec::with_capacity(config.outputs.len());
    for (index, output) in config.outputs.iter().enumerate() {
        if output.slot as usize >= MAX_OUTPUT_ROLES {
            return Err(invalid(
                format!("outputs[{index}].slot"),
                format!("slot {} outside 0..{MAX_OUTPUT_ROLES}", output.slot),
            ));
        }
        if !is_speaker_name(&output.speaker) {
            return Err(invalid(
                format!("outputs[{index}].speaker"),
                "speaker must match ^[A-Z][A-Z0-9]*$ (<=16 chars)",
            ));
        }
        if output.channel as usize >= MAX_OUTPUT_CHANNELS {
            return Err(invalid(
                format!("outputs[{index}].channel"),
                format!(
                    "channel {} outside 0..{MAX_OUTPUT_CHANNELS}",
                    output.channel
                ),
            ));
        }
        if output.channel >= config.playback_channels as u32 {
            return Err(invalid(
                format!("outputs[{index}].channel"),
                format!(
                    "channel {} must be < playback_channels {}",
                    output.channel, config.playback_channels
                ),
            ));
        }
        if slots.contains(&output.slot) {
            return Err(invalid(
                format!("outputs[{index}].slot"),
                format!("duplicate slot {}", output.slot),
            ));
        }
        if channels.contains(&output.channel) {
            return Err(invalid(
                format!("outputs[{index}].channel"),
                format!("duplicate channel {}", output.channel),
            ));
        }
        slots.push(output.slot);
        channels.push(output.channel);
    }
    validate_stimulus(config)?;
    if !config.k_main_trim_db.is_finite() || !(-60.0..=60.0).contains(&config.k_main_trim_db) {
        return Err(invalid(
            "k_main_trim_db",
            "k_main_trim_db must be finite and within -60..=60",
        ));
    }
    if !config.pre_seconds.is_finite() || !(MIN_PRE_SECONDS..=10.0).contains(&config.pre_seconds) {
        return Err(invalid(
            "pre_seconds",
            format!("pre_seconds must be within {MIN_PRE_SECONDS}..=10"),
        ));
    }
    if !config.post_seconds.is_finite() || !(MIN_POST_SECONDS..=10.0).contains(&config.post_seconds)
    {
        return Err(invalid(
            "post_seconds",
            format!("post_seconds must be within {MIN_POST_SECONDS}..=10"),
        ));
    }
    let protection = config.protection;
    if !protection.peak_ceiling_dbfs.is_finite()
        || !(-20.0..0.0).contains(&protection.peak_ceiling_dbfs)
    {
        return Err(invalid(
            "protection.peak_ceiling_dbfs",
            "peak ceiling must be finite and within -20..0 dBFS",
        ));
    }
    Ok(())
}

fn validate_stimulus(config: &SweepConfig) -> Result<(), SweepError> {
    let stimulus = &config.stimulus;
    if stimulus.kind != SweepKind::LogSweep {
        return Err(invalid("stimulus.kind", "only log_sweep is implemented"));
    }
    let fs = config.sample_rate as f64;
    if !stimulus.start_hz.is_finite() || stimulus.start_hz < 20.0 {
        return Err(invalid(
            "stimulus.start_hz",
            "start_hz must be finite and >= 20",
        ));
    }
    let max_hz = 20_000.0f64.min(fs / 2.0 - 1.0);
    if !stimulus.end_hz.is_finite() || stimulus.end_hz <= stimulus.start_hz {
        return Err(invalid(
            "stimulus.end_hz",
            "end_hz must be finite and > start_hz",
        ));
    }
    if stimulus.end_hz > max_hz {
        return Err(invalid(
            "stimulus.end_hz",
            format!("end_hz must be <= {max_hz} (fs/2-1 upper bound)"),
        ));
    }
    if !stimulus.seconds.is_finite()
        || !(MIN_SWEEP_SECONDS..=MAX_SWEEP_SECONDS).contains(&stimulus.seconds)
    {
        return Err(invalid(
            "stimulus.seconds",
            format!("seconds must be within {MIN_SWEEP_SECONDS}..={MAX_SWEEP_SECONDS}"),
        ));
    }
    if !stimulus.level_dbfs.is_finite()
        || !(MIN_DRIVE_LEVEL_DBFS..=MAX_DRIVE_LEVEL_DBFS).contains(&stimulus.level_dbfs)
    {
        return Err(invalid(
            "stimulus.level_dbfs",
            format!("level_dbfs must be within {MIN_DRIVE_LEVEL_DBFS}..={MAX_DRIVE_LEVEL_DBFS}"),
        ));
    }
    if !stimulus.fade_ms.is_finite() || !(0.01..=500.0).contains(&stimulus.fade_ms) {
        return Err(invalid(
            "stimulus.fade_ms",
            "fade_ms must be finite and within 0.01..=500",
        ));
    }
    if stimulus.repeats == 0 || stimulus.repeats > MAX_REPEATS {
        return Err(invalid(
            "stimulus.repeats",
            format!("repeats must be within 1..={MAX_REPEATS}"),
        ));
    }
    if !stimulus.repeat_gap_seconds.is_finite()
        || stimulus.repeat_gap_seconds > 10.0
        || stimulus.repeat_gap_seconds < config.session_kind.min_repeat_gap_seconds()
    {
        return Err(invalid(
            "stimulus.repeat_gap_seconds",
            format!(
                "repeat_gap_seconds must be within {}..=10 for {} sessions",
                config.session_kind.min_repeat_gap_seconds(),
                config.session_kind.as_str()
            ),
        ));
    }
    Ok(())
}

fn round_frames(seconds: f64, fs: f64, block: u64) -> u64 {
    let raw = seconds * fs;
    let blocks = (raw / block as f64).round().max(1.0) as u64;
    blocks.saturating_mul(block)
}

fn block_deadline_ns_of(block_frames: u16, sample_rate: u32) -> u64 {
    let numerator = block_frames as u128 * 1_000_000_000u128;
    let denominator = sample_rate as u128;
    ((numerator + denominator - 1) / denominator) as u64
}

fn invalid(field: impl AsRef<str>, message: impl Into<String>) -> SweepError {
    SweepError::InvalidConfig(format!("{}: {}", field.as_ref(), message.into()))
}

fn validate_text(field: &str, value: &str, max: usize) -> Result<(), SweepError> {
    if value.trim().is_empty() {
        return Err(invalid(field, "must be non-empty"));
    }
    if value.chars().count() > max {
        return Err(invalid(field, format!("must be <= {max} chars")));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

fn validate_graph_id(graph_id: &str) -> Result<(), SweepError> {
    if graph_id.len() != 64
        || !graph_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(
            "graph_id",
            "graph_id must be 64 lowercase hex chars",
        ));
    }
    if graph_id.bytes().all(|byte| byte == b'0') {
        return Err(invalid("graph_id", "graph_id must not be all zero"));
    }
    Ok(())
}

fn is_speaker_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    name.len() <= 16 && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// block 描述 / 渲染 / 播放
// ---------------------------------------------------------------------------

/// 一个渲染 block：core block 描述 + 段/输出/repeat 归属（sink 用它落 artifact/时轴）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepBlockInfo {
    /// core-v1 §11.3 block 描述。
    pub desc: TspiBlockDescV1,
    /// 段序号（SweepTimeline.segments 下标）。
    pub segment_index: u32,
    /// 段类型。
    pub segment_kind: SweepSegmentKind,
    /// 输出下标（静音段为 None）。
    pub output_index: Option<u32>,
    /// repeat 序号（静音段为 None）。
    pub repeat: Option<u32>,
}

impl SweepBlockInfo {
    /// 本 block 的 render frame 起点。
    pub const fn render_frame_index(&self) -> u64 {
        self.desc.frame_index
    }

    /// 本 block 的 ring slot。
    pub const fn ring_slot(&self) -> u32 {
        self.desc.slot_index
    }

    /// 是否尾块。
    pub const fn is_drain_tail(&self) -> bool {
        self.desc.flags & BLOCK_FLAG_DRAIN_TAIL != 0
    }
}

/// playback sink 回报（格式层 clip/nonfinite 统计；T22/T17 提供）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepSinkAck {
    /// 本 block 被饱和/clip 的样本数。
    pub clipped_samples: u64,
    /// 本 block 输出 nonfinite 样本数（应为 0）。
    pub nonfinite_samples: u64,
}

impl SweepSinkAck {
    /// 是否干净。
    pub const fn is_clean(&self) -> bool {
        self.clipped_samples == 0 && self.nonfinite_samples == 0
    }
}

/// 扫频 playback sink（真实 host 由 T26 接到 SPSC/ALSA；fake 见 [FakeSweepSink]）。
pub trait SweepPlaybackSink {
    /// 写一个 block（planar，channel-major：planar[ch * B + frame]）。
    fn write_block(
        &mut self,
        block: &SweepBlockInfo,
        planar: &[f32],
    ) -> Result<SweepSinkAck, SweepError>;

    /// 正常完成（全部 block 已写）；返回最后统计。
    fn finish(&mut self) -> Result<SweepSinkAck, SweepError> {
        Ok(SweepSinkAck::default())
    }

    /// 取消/保护停止（已写出的 block 不回滚；实现方应停流/静音）。
    fn abort(&mut self, _reason: SweepStopReason) -> Result<(), SweepError> {
        Ok(())
    }
}

/// 停止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepStopReason {
    /// 协作式取消（job cancel）。
    Cancelled,
    /// playback/format 层检测到 clip（保护停止）。
    ClipDetected,
}

impl SweepStopReason {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            SweepStopReason::Cancelled => "cancelled",
            SweepStopReason::ClipDetected => "clip_detected",
        }
    }
}

/// 运行结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepOutcome {
    /// 全部 block 写完成。
    Completed,
    /// 取消（已淡出）。
    Cancelled,
    /// clip 保护停止（已淡出）。
    ClipStopped,
}

impl SweepOutcome {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            SweepOutcome::Completed => "completed",
            SweepOutcome::Cancelled => "cancelled",
            SweepOutcome::ClipStopped => "clip_stopped",
        }
    }

    /// 是否完整跑完。
    pub const fn is_completed(self) -> bool {
        matches!(self, SweepOutcome::Completed)
    }
}

/// 运行报告（证据/状态；cancel/clip 时随错误返回）。
#[derive(Debug, Clone, PartialEq)]
pub struct SweepRunReport {
    /// 结果分类。
    pub outcome: SweepOutcome,
    /// 停止原因（Completed 为 None）。
    pub stop_reason: Option<SweepStopReason>,
    /// 停止发生时的下一个 planned block 序号。
    pub stop_block_seq: Option<u64>,
    /// 已写 block 数（含淡出块）。
    pub blocks_written: u64,
    /// 已写帧数（含淡出块）。
    pub frames_written: u64,
    /// 计划 block 数。
    pub planned_blocks: u64,
    /// 计划帧数。
    pub planned_frames: u64,
    /// 完整写完的段数。
    pub segments_completed: u32,
    /// 总段数。
    pub total_segments: u32,
    /// 生成样本峰值（线性）。
    pub peak: f32,
    /// playback/format 层回报的 clip 样本数。
    pub clipped_samples: u64,
    /// playback/format 层回报的 nonfinite 样本数。
    pub nonfinite_samples: u64,
    /// 取消/clip 停止时写出的淡出 block 数。
    pub cancel_fade_blocks: u64,
}

impl SweepRunReport {
    fn new(timeline: &SweepTimeline) -> Self {
        SweepRunReport {
            outcome: SweepOutcome::Completed,
            stop_reason: None,
            stop_block_seq: None,
            blocks_written: 0,
            frames_written: 0,
            planned_blocks: timeline.total_blocks,
            planned_frames: timeline.total_frames,
            segments_completed: 0,
            total_segments: timeline.segments.len() as u32,
            peak: 0.0,
            clipped_samples: 0,
            nonfinite_samples: 0,
            cancel_fade_blocks: 0,
        }
    }

    /// 峰值 dBFS（0 峰值返回 -inf）。
    pub fn peak_dbfs(&self) -> f64 {
        if self.peak <= 0.0 {
            f64::NEG_INFINITY
        } else {
            20.0 * (self.peak as f64).log10()
        }
    }

    fn record_planar(&mut self, planar: &[f32]) {
        for sample in planar {
            let value = sample.abs();
            if value > self.peak {
                self.peak = value;
            }
        }
    }

    fn record_ack(&mut self, ack: SweepSinkAck) {
        self.clipped_samples = self.clipped_samples.saturating_add(ack.clipped_samples);
        self.nonfinite_samples = self.nonfinite_samples.saturating_add(ack.nonfinite_samples);
    }
}

/// 逐输出扫频播放器（NONRT；持有一条固定时间轴的只读计划）。
#[derive(Debug, Clone)]
pub struct SweepPlayer {
    timeline: SweepTimeline,
    started: bool,
}

impl SweepPlayer {
    /// 校验配置并编译时间轴。
    pub fn new(config: SweepConfig) -> Result<Self, SweepError> {
        let timeline = compile_timeline(&config)?;
        Ok(SweepPlayer {
            timeline,
            started: false,
        })
    }

    /// 公共 render frame/slot 时间轴（只读）。
    pub fn timeline(&self) -> &SweepTimeline {
        &self.timeline
    }

    /// 计划帧数。
    pub fn total_frames(&self) -> u64 {
        self.timeline.total_frames
    }

    /// 计划 block 数。
    pub fn total_blocks(&self) -> u64 {
        self.timeline.total_blocks
    }

    /// 渲染一个**计划内** block 到 planar 缓冲（len = playback_channels × block_frames）。
    pub fn render_block(
        &self,
        block_seq: u64,
        planar: &mut [f32],
    ) -> Result<SweepBlockInfo, SweepError> {
        self.fill_planned_block(block_seq, planar)
    }

    /// 完整播放：逐 block 写入 sink；每 block 边界检查 cancel；clip 按保护策略停止。
    ///
    /// - 正常完成返回 Ok(report)（outcome=Completed）。
    /// - 取消返回 Err(SweepError::Cancelled(report))（扫频中会先写淡出块）。
    /// - clip 保护停止返回 Err(SweepError::ClipDetected(report))（扫频中会先写淡出块）。
    pub fn run(
        &mut self,
        sink: &mut dyn SweepPlaybackSink,
        cancel: &CancelToken,
    ) -> Result<SweepRunReport, SweepError> {
        if self.started {
            return Err(SweepError::InvalidState(
                "SweepPlayer::run may only be called once per compiled timeline".to_string(),
            ));
        }
        self.started = true;

        let channels = self.timeline.playback_channels as usize;
        let block = self.timeline.block_frames as usize;
        let mut planar = vec![0.0f32; channels.saturating_mul(block)];
        let mut report = SweepRunReport::new(&self.timeline);
        let mut block_seq = 0u64;
        while block_seq < self.timeline.total_blocks {
            if cancel.is_requested() {
                return self.stop_with_fade(
                    sink,
                    &mut planar,
                    block_seq,
                    SweepStopReason::Cancelled,
                    report,
                );
            }
            let info = self.fill_planned_block(block_seq, &mut planar)?;
            let ack = sink.write_block(&info, &planar)?;
            report.record_planar(&planar);
            report.record_ack(ack);
            report.blocks_written = report.blocks_written.saturating_add(1);
            report.frames_written = report
                .frames_written
                .saturating_add(info.desc.valid_frames as u64);
            let segment = &self.timeline.segments[info.segment_index as usize];
            if block_seq + 1 == segment.start_block_seq + segment.blocks {
                report.segments_completed = report.segments_completed.saturating_add(1);
            }
            block_seq += 1;
            if !ack.is_clean() && self.timeline.protection.abort_on_clip {
                return self.stop_with_fade(
                    sink,
                    &mut planar,
                    block_seq,
                    SweepStopReason::ClipDetected,
                    report,
                );
            }
        }
        let final_ack = sink.finish()?;
        report.record_ack(final_ack);
        if report.clipped_samples > 0 && self.timeline.protection.abort_on_clip {
            report.outcome = SweepOutcome::ClipStopped;
            report.stop_reason = Some(SweepStopReason::ClipDetected);
            report.stop_block_seq = Some(block_seq);
            return Err(SweepError::ClipDetected(report));
        }
        report.outcome = SweepOutcome::Completed;
        Ok(report)
    }

    fn fill_planned_block(
        &self,
        block_seq: u64,
        planar: &mut [f32],
    ) -> Result<SweepBlockInfo, SweepError> {
        let channels = self.timeline.playback_channels as usize;
        let block = self.timeline.block_frames as usize;
        let expected = channels.saturating_mul(block);
        if planar.len() != expected {
            return Err(SweepError::InvalidBuffer {
                expected,
                actual: planar.len(),
            });
        }
        if block_seq >= self.timeline.total_blocks {
            return Err(SweepError::InvalidBlockSeq {
                block_seq,
                total_blocks: self.timeline.total_blocks,
            });
        }
        let location = self.locate(block_seq).ok_or(SweepError::InvalidBlockSeq {
            block_seq,
            total_blocks: self.timeline.total_blocks,
        })?;
        planar.fill(0.0);
        let mut info = SweepBlockInfo {
            desc: TspiBlockDescV1::ZERO,
            segment_index: location.segment.index,
            segment_kind: location.segment.kind,
            output_index: location.segment.output_index,
            repeat: location.segment.repeat,
        };
        if location.segment.kind.is_sweep() {
            let output_index = location
                .segment
                .output_index
                .ok_or_else(|| SweepError::InvalidState("sweep segment without output".into()))?;
            let channel = self
                .timeline
                .output_channel(output_index as usize)
                .ok_or_else(|| {
                    SweepError::InvalidState(format!("unknown output index {output_index}"))
                })? as usize;
            if channel >= channels {
                return Err(SweepError::InvalidState(format!(
                    "output channel {channel} outside playback buffer"
                )));
            }
            let base = channel * block;
            for frame in 0..block {
                let t = location.frame_offset + frame as u64;
                planar[base + frame] = self.sample_at(location.segment, t)?;
            }
        }
        let mut flags = BLOCK_FLAG_VALID;
        if block_seq == 0 {
            flags |= BLOCK_FLAG_EPOCH_FIRST;
        }
        if block_seq + 1 == self.timeline.total_blocks {
            flags |= BLOCK_FLAG_DRAIN_TAIL;
        }
        info.desc = self.build_desc(block_seq, block as u32, flags);
        Ok(info)
    }

    fn locate(&self, block_seq: u64) -> Option<BlockLocation<'_>> {
        if block_seq >= self.timeline.total_blocks {
            return None;
        }
        let position = self
            .timeline
            .segments
            .partition_point(|segment| segment.start_block_seq <= block_seq);
        if position == 0 {
            return None;
        }
        let segment = &self.timeline.segments[position - 1];
        let local_block = block_seq - segment.start_block_seq;
        if local_block >= segment.blocks {
            return None;
        }
        Some(BlockLocation {
            segment,
            frame_offset: local_block * self.timeline.block_frames as u64,
        })
    }

    fn build_desc(&self, block_seq: u64, valid_frames: u32, flags: u32) -> TspiBlockDescV1 {
        let frame_index = block_seq * self.timeline.block_frames as u64;
        let publish = self
            .timeline
            .epoch_start_mono_ns
            .saturating_add(frames_to_ns(frame_index, self.timeline.sample_rate));
        TspiBlockDescV1 {
            abi_version: ABI_VERSION,
            block_size: self.timeline.block_frames,
            n_in: self.timeline.n_in,
            n_out: self.timeline.playback_channels,
            epoch_id: self.timeline.epoch_id,
            block_seq,
            frame_index,
            plan_version: self.timeline.plan_version,
            flags,
            valid_frames,
            slot_index: (block_seq % PLAYBACK_RING_SLOTS) as u32,
            mono_publish_ns: publish,
            play_deadline_ns: publish.saturating_add(self.timeline.block_deadline_ns),
        }
    }

    fn sample_at(&self, segment: &SweepSegment, t: u64) -> Result<f32, SweepError> {
        if segment.kind != SweepSegmentKind::Sweep {
            return Ok(0.0);
        }
        if t >= segment.sweep_frames {
            return Ok(0.0);
        }
        let fs = self.timeline.sample_rate as f64;
        let frames = segment.sweep_frames as f64;
        let duration = frames / fs;
        let f0 = self.timeline.stimulus.start_hz;
        let f1 = self.timeline.stimulus.end_hz;
        let log_ratio = (f1 / f0).ln();
        let time = t as f64 / fs;
        let phase = 2.0 * std::f64::consts::PI * f0 * duration / log_ratio
            * ((time / duration * log_ratio).exp() - 1.0);
        let envelope = segment_envelope(segment, t);
        let sample = self.timeline.level.peak_linear * envelope * phase.sin();
        if !sample.is_finite() {
            return Err(SweepError::NonfiniteOutput {
                block_seq: t / self.timeline.block_frames.max(1) as u64,
            });
        }
        Ok(sample as f32)
    }

    fn stop_with_fade(
        &self,
        sink: &mut dyn SweepPlaybackSink,
        planar: &mut [f32],
        stop_block: u64,
        reason: SweepStopReason,
        mut report: SweepRunReport,
    ) -> Result<SweepRunReport, SweepError> {
        report.stop_reason = Some(reason);
        report.stop_block_seq = Some(stop_block);
        report.outcome = match reason {
            SweepStopReason::Cancelled => SweepOutcome::Cancelled,
            SweepStopReason::ClipDetected => SweepOutcome::ClipStopped,
        };
        if let Some(location) = self.locate(stop_block) {
            if location.segment.kind.is_sweep() {
                self.write_fade_blocks(sink, planar, stop_block, location, &mut report)?;
            }
        }
        sink.abort(reason)?;
        Err(match reason {
            SweepStopReason::Cancelled => SweepError::Cancelled(report),
            SweepStopReason::ClipDetected => SweepError::ClipDetected(report),
        })
    }

    fn write_fade_blocks(
        &self,
        sink: &mut dyn SweepPlaybackSink,
        planar: &mut [f32],
        start_block: u64,
        location: BlockLocation<'_>,
        report: &mut SweepRunReport,
    ) -> Result<(), SweepError> {
        let segment = location.segment;
        let block = self.timeline.block_frames as usize;
        let fade_frames = segment.fade_frames;
        if fade_frames == 0 {
            return Ok(());
        }
        let frames_left = segment.sweep_frames.saturating_sub(location.frame_offset);
        let fade = fade_frames.min(frames_left);
        if fade == 0 {
            return Ok(());
        }
        let output_index = segment
            .output_index
            .ok_or_else(|| SweepError::InvalidState("fade segment without output".into()))?;
        let channel = self
            .timeline
            .output_channel(output_index as usize)
            .ok_or_else(|| {
                SweepError::InvalidState(format!("unknown output index {output_index}"))
            })?;
        let channel = channel as usize;
        let mut emitted = 0u64;
        while emitted < fade {
            let frames_this = (fade - emitted).min(block as u64) as usize;
            planar.fill(0.0);
            let base = channel * block;
            for frame in 0..frames_this {
                let k = emitted + frame as u64;
                let t = location.frame_offset + k;
                let sample = self.sample_at(segment, t)? as f64 * (1.0 - k as f64 / fade as f64);
                planar[base + frame] = sample as f32;
            }
            let seq = start_block + report.cancel_fade_blocks;
            let is_last = emitted + frames_this as u64 >= fade;
            let mut flags = BLOCK_FLAG_VALID;
            if is_last {
                flags |= BLOCK_FLAG_DRAIN_TAIL;
            }
            let valid_frames = if frames_this == block {
                block as u32
            } else {
                frames_this as u32
            };
            let info = SweepBlockInfo {
                desc: self.build_desc(seq, valid_frames, flags),
                segment_index: segment.index,
                segment_kind: segment.kind,
                output_index: segment.output_index,
                repeat: segment.repeat,
            };
            if planar.iter().any(|sample| !sample.is_finite()) {
                return Err(SweepError::NonfiniteOutput { block_seq: seq });
            }
            report.record_planar(planar);
            let ack = sink.write_block(&info, planar)?;
            report.record_ack(ack);
            report.blocks_written = report.blocks_written.saturating_add(1);
            report.frames_written = report.frames_written.saturating_add(valid_frames as u64);
            report.cancel_fade_blocks = report.cancel_fade_blocks.saturating_add(1);
            emitted += frames_this as u64;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct BlockLocation<'a> {
    segment: &'a SweepSegment,
    frame_offset: u64,
}

fn segment_envelope(segment: &SweepSegment, t: u64) -> f64 {
    let fade = segment.fade_frames;
    if fade == 0 || segment.sweep_frames == 0 {
        return 1.0;
    }
    let frames = segment.sweep_frames;
    if t < fade {
        let denom = fade.saturating_sub(1) as f64;
        if denom <= 0.0 {
            return 1.0;
        }
        return 0.5 * (1.0 - (std::f64::consts::PI * t as f64 / denom).cos());
    }
    if t + fade >= frames {
        let local = t.saturating_sub(frames.saturating_sub(fade));
        let denom = fade.saturating_sub(1) as f64;
        if denom <= 0.0 {
            return 0.0;
        }
        return 0.5 * (1.0 + (std::f64::consts::PI * local as f64 / denom).cos());
    }
    1.0
}

fn frames_to_ns(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let numerator = frames as u128 * 1_000_000_000u128;
    (numerator / sample_rate as u128) as u64
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 扫频错误（配置/缓冲/block 序/sink/取消/clip）。
#[derive(Debug, Clone, PartialEq)]
pub enum SweepError {
    /// 配置非法（字段级消息）。
    InvalidConfig(String),
    /// planar 缓冲长度不符（期望 × 实际）。
    InvalidBuffer {
        /// 期望样本数。
        expected: usize,
        /// 实际样本数。
        actual: usize,
    },
    /// block 序号越界。
    InvalidBlockSeq {
        /// 请求的 block 序号。
        block_seq: u64,
        /// 计划 block 数。
        total_blocks: u64,
    },
    /// 状态非法（例如重复 run）。
    InvalidState(String),
    /// sink 失败。
    Sink {
        /// 稳定错误码。
        code: &'static str,
        /// 细节。
        message: String,
    },
    /// 生成样本出现 nonfinite（不变量破坏）。
    NonfiniteOutput {
        /// 所在 block。
        block_seq: u64,
    },
    /// 取消（含已写出的报告）。
    Cancelled(SweepRunReport),
    /// clip 保护停止（含已写出的报告）。
    ClipDetected(SweepRunReport),
}

impl SweepError {
    /// 取消/clip 时取回运行报告。
    pub fn report(&self) -> Option<&SweepRunReport> {
        match self {
            SweepError::Cancelled(report) | SweepError::ClipDetected(report) => Some(report),
            _ => None,
        }
    }

    /// 是否协作式取消（映射 T60 cancel）。
    pub fn is_cancelled(&self) -> bool {
        matches!(self, SweepError::Cancelled(_))
    }

    /// 是否 clip 保护停止。
    pub fn is_clip_stopped(&self) -> bool {
        matches!(self, SweepError::ClipDetected(_))
    }

    /// 映射为 T60 CalibrationJobBackend 的 stage 失败（取消/失败分派）。
    pub fn stage_failure(&self) -> StageFailure {
        match self {
            SweepError::Cancelled(report) => StageFailure::cancelled(format!(
                "sweep cancelled after {} of {} blocks",
                report.blocks_written, report.planned_blocks
            )),
            SweepError::ClipDetected(report) => StageFailure::failed(
                "E_SWEEP_CLIP",
                format!(
                    "sweep stopped by clip protection after {} blocks (clipped={}, fade_blocks={})",
                    report.blocks_written, report.clipped_samples, report.cancel_fade_blocks
                ),
            ),
            other => StageFailure::failed("E_SWEEP", other.to_string()),
        }
    }
}

impl fmt::Display for SweepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SweepError::InvalidConfig(message) => write!(f, "invalid sweep config: {message}"),
            SweepError::InvalidBuffer { expected, actual } => write!(
                f,
                "invalid planar buffer: expected {expected} samples, got {actual}"
            ),
            SweepError::InvalidBlockSeq {
                block_seq,
                total_blocks,
            } => write!(f, "block_seq {block_seq} outside planned 0..{total_blocks}"),
            SweepError::InvalidState(message) => write!(f, "invalid sweep state: {message}"),
            SweepError::Sink { code, message } => write!(f, "sweep sink {code}: {message}"),
            SweepError::NonfiniteOutput { block_seq } => {
                write!(f, "nonfinite sweep output at block {block_seq}")
            }
            SweepError::Cancelled(report) => write!(
                f,
                "sweep cancelled after {}/{} blocks ({} fade blocks)",
                report.blocks_written, report.planned_blocks, report.cancel_fade_blocks
            ),
            SweepError::ClipDetected(report) => write!(
                f,
                "sweep clip-stop after {}/{} blocks (clipped={})",
                report.blocks_written, report.planned_blocks, report.clipped_samples
            ),
        }
    }
}

impl std::error::Error for SweepError {}

// ---------------------------------------------------------------------------
// fake backend（离线逐段样本验证；也可被 T62/T26 复用）
// ---------------------------------------------------------------------------

/// fake sink 记录的一个 block。
#[derive(Debug, Clone, PartialEq)]
pub struct FakeSweepBlock {
    /// block 身份/段归属。
    pub info: SweepBlockInfo,
    /// 有效帧数（= desc.valid_frames）。
    pub frames: u64,
    /// 本 block 样本峰值（线性）。
    pub peak_abs: f32,
}

/// 离线 fake sink：逐 block 记录 planar 样本（channel-major）并做序号/长度断言；
/// 可注入 clip/nonfinite 回报与 IO 失败，用于“样本逐段验证”。
#[derive(Debug, Clone)]
pub struct FakeSweepSink {
    playback_channels: usize,
    block_frames: usize,
    channels: Vec<Vec<f32>>,
    blocks: Vec<FakeSweepBlock>,
    next_block_seq: u64,
    /// 在第 N 个 block 回报 clipped_samples=1。
    pub inject_clipped_at_block: Option<u64>,
    /// 在第 N 个 block 回报 nonfinite_samples=1。
    pub inject_nonfinite_at_block: Option<u64>,
    /// 在第 N 个 block 返回 IO 错误。
    pub fail_at_block: Option<u64>,
    /// 是否已正常 finish。
    pub finished: bool,
    /// abort 原因（未 abort 为 None）。
    pub aborted: Option<SweepStopReason>,
}

impl FakeSweepSink {
    /// 构造。
    pub fn new(playback_channels: usize, block_frames: usize) -> Self {
        FakeSweepSink {
            playback_channels,
            block_frames,
            channels: vec![Vec::new(); playback_channels],
            blocks: Vec::new(),
            next_block_seq: 0,
            inject_clipped_at_block: None,
            inject_nonfinite_at_block: None,
            fail_at_block: None,
            finished: false,
            aborted: None,
        }
    }

    /// 已记录帧数。
    pub fn frame_count(&self) -> usize {
        self.channels
            .first()
            .map(|channel| channel.len())
            .unwrap_or(0)
    }

    /// 某通道样本切片。
    pub fn channel(&self, channel: usize) -> &[f32] {
        &self.channels[channel]
    }

    /// 全部通道。
    pub fn channels(&self) -> &[Vec<f32>] {
        &self.channels
    }

    /// 记录的 block 列表。
    pub fn blocks(&self) -> &[FakeSweepBlock] {
        &self.blocks
    }

    /// 某 block 在整条时间轴上的起始帧。
    pub fn block_start_frame(&self, block_index: usize) -> Option<u64> {
        self.blocks
            .get(block_index)
            .map(|block| block.info.desc.frame_index)
    }
}

impl SweepPlaybackSink for FakeSweepSink {
    fn write_block(
        &mut self,
        block: &SweepBlockInfo,
        planar: &[f32],
    ) -> Result<SweepSinkAck, SweepError> {
        if block.desc.abi_version != ABI_VERSION {
            return Err(SweepError::Sink {
                code: "E_FAKE_ABI",
                message: format!("abi_version {} != {ABI_VERSION}", block.desc.abi_version),
            });
        }
        if block.desc.block_size as usize != self.block_frames {
            return Err(SweepError::Sink {
                code: "E_FAKE_BLOCK",
                message: format!(
                    "block_size {} != sink block_frames {}",
                    block.desc.block_size, self.block_frames
                ),
            });
        }
        if planar.len() != self.playback_channels.saturating_mul(self.block_frames) {
            return Err(SweepError::Sink {
                code: "E_FAKE_LEN",
                message: format!(
                    "planar len {} != {}x{}",
                    planar.len(),
                    self.playback_channels,
                    self.block_frames
                ),
            });
        }
        if block.desc.block_seq != self.next_block_seq {
            return Err(SweepError::Sink {
                code: "E_FAKE_SEQ",
                message: format!(
                    "block_seq {} != expected {}",
                    block.desc.block_seq, self.next_block_seq
                ),
            });
        }
        if let Some(fail_at) = self.fail_at_block {
            if block.desc.block_seq == fail_at {
                return Err(SweepError::Sink {
                    code: "E_FAKE_IO",
                    message: format!("injected IO failure at block {fail_at}"),
                });
            }
        }
        for channel in 0..self.playback_channels {
            let start = channel * self.block_frames;
            self.channels[channel].extend_from_slice(&planar[start..start + self.block_frames]);
        }
        let mut peak = 0.0f32;
        for sample in planar {
            let value = sample.abs();
            if value > peak {
                peak = value;
            }
        }
        self.blocks.push(FakeSweepBlock {
            info: *block,
            frames: block.desc.valid_frames as u64,
            peak_abs: peak,
        });
        self.next_block_seq += 1;
        let mut ack = SweepSinkAck::default();
        if self.inject_clipped_at_block == Some(block.desc.block_seq) {
            ack.clipped_samples = 1;
        }
        if self.inject_nonfinite_at_block == Some(block.desc.block_seq) {
            ack.nonfinite_samples = 1;
        }
        Ok(ack)
    }

    fn finish(&mut self) -> Result<SweepSinkAck, SweepError> {
        self.finished = true;
        Ok(SweepSinkAck::default())
    }

    fn abort(&mut self, reason: SweepStopReason) -> Result<(), SweepError> {
        self.aborted = Some(reason);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JSON 辅助
// ---------------------------------------------------------------------------

fn push_json_str(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            other if (other as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    push_json_str(&mut out, value);
    out
}

fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

fn json_option_u32(value: Option<u32>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

// ---------------------------------------------------------------------------
// 单元测试（WBS T61：fake backend 样本逐段验证；cargo test -p tspi-calibration sweep）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs(count: usize) -> Vec<SweepOutput> {
        (0..count)
            .map(|index| SweepOutput::new(index as u32, format!("CH{index}"), index as u32))
            .collect()
    }

    fn short_config(output_count: usize, repeats: u32) -> SweepConfig {
        let mut config = SweepConfig::loopback("sweep-test", outputs(output_count));
        config.block_frames = 64;
        config.stimulus = SweepStimulus::log_sweep(100.0, 1_000.0, 0.05, -12.0, 2.0, repeats, 0.2);
        config.pre_seconds = 0.2;
        config.post_seconds = 0.2;
        config
    }

    fn long_config() -> SweepConfig {
        let mut config = short_config(1, 1);
        config.stimulus.seconds = 0.5;
        config
    }

    fn compiled(config: &SweepConfig) -> SweepTimeline {
        compile_timeline(config).expect("timeline compiles")
    }

    fn run_ok(config: SweepConfig) -> (SweepRunReport, FakeSweepSink, SweepTimeline) {
        let timeline = compiled(&config);
        let mut player = SweepPlayer::new(config).expect("player");
        let mut sink = FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        );
        let cancel = CancelToken::new();
        let report = player.run(&mut sink, &cancel).expect("sweep run");
        (report, sink, timeline)
    }

    fn segment_channel<'a>(
        sink: &'a FakeSweepSink,
        segment: &SweepSegment,
        channel: usize,
    ) -> &'a [f32] {
        let start = segment.start_frame as usize;
        let end = segment.end_frame() as usize;
        &sink.channel(channel)[start..end]
    }

    fn assert_invalid(config: &SweepConfig, field: &str) {
        let error = config
            .validate()
            .expect_err("configuration must be rejected");
        let message = error.to_string();
        assert!(
            message.contains(field),
            "expected field {field:?} in error, got {message}"
        );
    }

    fn phase_at(t: u64, frames: u64, fs: f64, f0: f64, f1: f64) -> f64 {
        let duration = frames as f64 / fs;
        let log_ratio = (f1 / f0).ln();
        let time = t as f64 / fs;
        2.0 * std::f64::consts::PI * f0 * duration / log_ratio
            * ((time / duration * log_ratio).exp() - 1.0)
    }

    fn zero_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|window| (window[0] <= 0.0) != (window[1] <= 0.0))
            .count()
    }

    #[test]
    fn sweep_config_validation_rejects_contract_violations() {
        let good = short_config(2, 1);
        assert!(good.validate().is_ok(), "baseline config must compile");

        let mut bad = good.clone();
        bad.sample_rate = 44_100;
        assert_invalid(&bad, "sample_rate");

        let mut bad = good.clone();
        bad.block_frames = 256;
        assert_invalid(&bad, "block_frames");

        let mut bad = good.clone();
        bad.outputs = Vec::new();
        assert_invalid(&bad, "outputs");

        let mut bad = good.clone();
        bad.outputs[1].channel = 0;
        assert_invalid(&bad, "duplicate channel");

        let mut bad = good.clone();
        bad.outputs[1].speaker = "fl".to_string();
        assert_invalid(&bad, "speaker");

        let mut bad = good.clone();
        bad.playback_channels = 1;
        assert_invalid(&bad, "playback_channels");

        let mut bad = good.clone();
        bad.epoch_id = 0;
        assert_invalid(&bad, "epoch_id");

        let mut bad = good.clone();
        bad.graph_id = "abc".to_string();
        assert_invalid(&bad, "graph_id");

        let mut bad = good.clone();
        bad.graph_id = "0".repeat(64);
        assert_invalid(&bad, "graph_id");

        let mut bad = good.clone();
        bad.stimulus.level_dbfs = -61.0;
        assert_invalid(&bad, "level_dbfs");

        let mut bad = good.clone();
        bad.stimulus.start_hz = 10.0;
        assert_invalid(&bad, "start_hz");

        let mut bad = good.clone();
        bad.stimulus.end_hz = 25_000.0;
        assert_invalid(&bad, "end_hz");

        let mut bad = good.clone();
        bad.stimulus.end_hz = 100.0;
        assert_invalid(&bad, "end_hz");

        let mut bad = good.clone();
        bad.stimulus.repeats = 0;
        assert_invalid(&bad, "repeats");

        let mut bad = good.clone();
        bad.stimulus.fade_ms = 500.0;
        assert_invalid(&bad, "fade");

        let mut bad = good.clone();
        bad.session_kind = SweepSessionKind::Acoustic;
        bad.stimulus.repeat_gap_seconds = 0.2;
        assert_invalid(&bad, "repeat_gap_seconds");

        let mut bad = good.clone();
        bad.pre_seconds = 0.0;
        assert_invalid(&bad, "pre_seconds");

        let mut bad = good.clone();
        bad.post_seconds = 0.0;
        assert_invalid(&bad, "post_seconds");

        let mut bad = good.clone();
        bad.k_main_trim_db = 100.0;
        assert_invalid(&bad, "k_main_trim_db");

        let mut bad = good.clone();
        bad.protection.peak_ceiling_dbfs = 3.0;
        assert_invalid(&bad, "peak_ceiling_dbfs");

        let mut bad = good.clone();
        bad.plan_version = 0;
        assert_invalid(&bad, "plan_version");
    }

    #[test]
    fn sweep_level_and_trim_policy_match_acceptance() {
        let t1 = compiled(&short_config(1, 1));
        assert_eq!(t1.level.drive_level_dbfs, T1_LOOPBACK_LEVEL_DBFS);
        assert_eq!(t1.level.effective_level_dbfs, T1_LOOPBACK_LEVEL_DBFS);
        assert!((t1.level.peak_linear - 10f64.powf(-12.0 / 20.0)).abs() < 1.0e-12);
        assert_eq!(
            t1.protection.peak_ceiling_dbfs,
            MEASUREMENT_PEAK_CEILING_DBFS
        );

        let mut acoustic = SweepConfig::acoustic("acoustic", outputs(1));
        acoustic.block_frames = 64;
        acoustic.stimulus.seconds = 0.05;
        acoustic.stimulus.fade_ms = 2.0;
        acoustic.stimulus.repeats = 1;
        acoustic.stimulus.start_hz = 100.0;
        acoustic.stimulus.end_hz = 1_000.0;
        acoustic.pre_seconds = 0.2;
        acoustic.post_seconds = 0.2;
        let timeline = compiled(&acoustic);
        assert_eq!(timeline.level.drive_level_dbfs, ACOUSTIC_LEVEL_DBFS);
        assert_eq!(timeline.level.effective_level_dbfs, ACOUSTIC_LEVEL_DBFS);

        let mut record_only = acoustic.clone();
        record_only.k_main_trim_db = -28.0;
        record_only.trim_policy = TrimPolicy::RecordOnly;
        let timeline = compiled(&record_only);
        assert_eq!(timeline.level.k_main_trim_db, -28.0);
        assert_eq!(timeline.level.effective_level_dbfs, ACOUSTIC_LEVEL_DBFS);

        let mut compensate = acoustic.clone();
        compensate.k_main_trim_db = -28.0;
        compensate.trim_policy = TrimPolicy::Compensate;
        let timeline = compiled(&compensate);
        assert!((timeline.level.effective_level_dbfs - (-2.0)).abs() < 1.0e-12);
        assert!(timeline.level.peak_linear <= 10f64.powf(-1.0 / 20.0));

        let mut over = short_config(1, 1);
        over.k_main_trim_db = -28.0;
        over.trim_policy = TrimPolicy::Compensate;
        assert_invalid(&over, "ceiling");
    }

    #[test]
    fn sweep_endpoints_are_exactly_zero_and_peak_matches_level() {
        let config = short_config(1, 1);
        let (report, sink, timeline) = run_ok(config);
        assert!(report.outcome.is_completed());
        assert!(sink.finished && sink.aborted.is_none());
        let sweep_segment = timeline
            .segments
            .iter()
            .find(|segment| segment.kind == SweepSegmentKind::Sweep)
            .expect("sweep segment");
        let samples = segment_channel(&sink, sweep_segment, 0);
        assert_eq!(samples.len() as u64, sweep_segment.frames);
        assert_eq!(samples[0], 0.0, "first sample must be exactly zero");
        assert_eq!(
            samples[samples.len() - 1],
            0.0,
            "last sample must be exactly zero"
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
        let expected_peak = timeline.level.peak_linear;
        let measured_peak = samples
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs())) as f64;
        assert!(
            measured_peak <= expected_peak * 1.0001,
            "peak {measured_peak} exceeds level {expected_peak}"
        );
        assert!(
            measured_peak >= expected_peak * 0.99,
            "peak {measured_peak} misses expected level {expected_peak}"
        );
        assert!(report.peak as f64 <= expected_peak * 1.0001);
    }

    #[test]
    fn sweep_zero_crossings_match_log_sweep_phase_rate() {
        let mut config = short_config(1, 1);
        config.stimulus = SweepStimulus::log_sweep(100.0, 1_000.0, 0.2, -12.0, 1.0, 1, 0.2);
        let (_report, sink, timeline) = run_ok(config);
        let sweep_segment = timeline
            .segments
            .iter()
            .find(|segment| segment.kind == SweepSegmentKind::Sweep)
            .expect("sweep segment");
        let samples = segment_channel(&sink, sweep_segment, 0);
        let fade = sweep_segment.fade_frames as usize;
        let frames = sweep_segment.sweep_frames;
        let region = &samples[fade..samples.len() - fade];
        let crossings = zero_crossings(region);
        let start = fade as u64;
        let end = frames - fade as u64 - 1;
        let span = phase_at(end, frames, 48_000.0, 100.0, 1_000.0)
            - phase_at(start, frames, 48_000.0, 100.0, 1_000.0);
        let expected = span / std::f64::consts::PI;
        let tolerance = (expected * 0.02).max(2.0);
        assert!(
            ((crossings as f64) - expected).abs() <= tolerance,
            "zero crossings {crossings} vs expected {expected} (span {span})"
        );
    }

    #[test]
    fn sweep_blocks_are_continuous_and_marked() {
        let config = short_config(2, 1);
        let timeline = compiled(&config);
        let (report, sink, _timeline) = run_ok(config);
        assert!(report.outcome.is_completed());
        assert_eq!(sink.blocks().len() as u64, timeline.total_blocks);
        let block = timeline.block_frames as u64;
        for (index, record) in sink.blocks().iter().enumerate() {
            let desc = record.info.desc;
            assert_eq!(desc.abi_version, ABI_VERSION);
            assert_eq!(desc.block_seq, index as u64);
            assert_eq!(desc.frame_index, index as u64 * block);
            assert_eq!(desc.slot_index, (index as u64 % PLAYBACK_RING_SLOTS) as u32);
            assert_eq!(desc.block_size, timeline.block_frames);
            assert_eq!(desc.n_out, timeline.playback_channels);
            assert_eq!(desc.epoch_id, timeline.epoch_id);
            assert_eq!(desc.plan_version, timeline.plan_version);
            assert_eq!(desc.valid_frames, timeline.block_frames as u32);
            assert_ne!(desc.flags & BLOCK_FLAG_VALID, 0);
            if index == 0 {
                assert_ne!(desc.flags & BLOCK_FLAG_EPOCH_FIRST, 0);
            } else {
                assert_eq!(desc.flags & BLOCK_FLAG_EPOCH_FIRST, 0);
            }
            if index + 1 == sink.blocks().len() {
                assert_ne!(desc.flags & BLOCK_FLAG_DRAIN_TAIL, 0);
            } else {
                assert_eq!(desc.flags & BLOCK_FLAG_DRAIN_TAIL, 0);
            }
        }
    }

    #[test]
    fn sweep_silence_segments_are_exactly_zero() {
        let config = short_config(2, 1);
        let (_report, sink, timeline) = run_ok(config);
        let mut seen = 0usize;
        for segment in &timeline.segments {
            if segment.kind == SweepSegmentKind::Sweep {
                continue;
            }
            seen += 1;
            for channel in 0..sink.channels().len() {
                let samples = segment_channel(&sink, segment, channel);
                assert!(
                    samples.iter().all(|sample| *sample == 0.0),
                    "{} segment at {} has non-zero channel {} samples",
                    segment.kind.as_str(),
                    segment.start_frame,
                    channel
                );
            }
        }
        assert!(seen >= 3, "expected pre/gap/post silence segments");
    }

    #[test]
    fn sweep_only_target_output_channel_is_active() {
        let config = short_config(2, 2);
        let (_report, sink, timeline) = run_ok(config);
        let mut sweep_count = 0usize;
        for segment in &timeline.segments {
            if segment.kind != SweepSegmentKind::Sweep {
                continue;
            }
            sweep_count += 1;
            let output_index = segment.output_index.expect("output") as usize;
            let active = timeline.outputs[output_index].channel as usize;
            for channel in 0..sink.channels().len() {
                let samples = segment_channel(&sink, segment, channel);
                if channel == active {
                    let peak = samples
                        .iter()
                        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                    assert!(
                        peak > 0.1 * timeline.level.peak_linear as f32,
                        "active channel {channel} has no signal"
                    );
                } else {
                    assert!(
                        samples.iter().all(|sample| *sample == 0.0),
                        "inactive channel {channel} must stay exactly zero"
                    );
                }
            }
        }
        assert_eq!(sweep_count, 4, "2 outputs x 2 repeats");
    }

    #[test]
    fn sweep_slot_timeline_matches_t48_t49_expectations() {
        let mut config = short_config(2, 2);
        config.stimulus.repeats = 2;
        let timeline = compiled(&config);
        assert_eq!(timeline.slots.len(), 2);
        let sweep = timeline.actual_stimulus.sweep_frames;
        let gap = timeline.actual_stimulus.gap_frames;
        let expected_interval = timeline.stimulus.repeats as u64 * (sweep + gap);
        assert_eq!(timeline.expected_slot_interval_frames(), expected_interval);
        assert_eq!(
            timeline.slots[1].render_frame_index - timeline.slots[0].render_frame_index,
            expected_interval
        );
        for slot in &timeline.slots {
            assert!(
                slot.render_frame_index > 0,
                "T48 diag requires render_frame_index > 0"
            );
            assert_eq!(slot.repeat_render_frames.len(), 2);
            let delta = slot.repeat_render_frames[1] - slot.repeat_render_frames[0];
            assert_eq!(delta, sweep + gap);
            assert!(slot.repeat_render_frames[1] > slot.repeat_render_frames[0]);
        }
        assert_eq!(
            timeline.first_sweep_render_frame(),
            Some(timeline.pre_frames)
        );
        assert_eq!(timeline.pre_frames % timeline.block_frames as u64, 0);
        assert_eq!(timeline.actual_stimulus.seconds, sweep as f64 / 48_000.0);
        assert_eq!(
            timeline.actual_stimulus.repeat_gap_seconds,
            gap as f64 / 48_000.0
        );
        assert_eq!(timeline.segments.len(), 1 + 2 * 2 * 2 + 1);
        let mut frame = 0u64;
        for segment in &timeline.segments {
            assert_eq!(segment.start_frame, frame);
            assert_eq!(
                segment.start_block_seq * timeline.block_frames as u64,
                frame
            );
            frame += segment.frames;
        }
        assert_eq!(frame, timeline.total_frames);
        assert_eq!(timeline.total_blocks * timeline.block_frames as u64, frame);
    }

    #[test]
    fn sweep_timeline_json_is_deterministic_and_contract_shaped() {
        let mut config = short_config(2, 1);
        config.session_id = "sess-\"q\"\\x".to_string();
        let timeline = compiled(&config);
        let again = compiled(&config);
        assert_eq!(timeline, again);
        let json = timeline.to_json();
        assert_eq!(json, again.to_json());
        assert!(json.contains("\"schema\":\"tspi-sweep-timeline/v1\""));
        assert!(json.contains("\"kind\":\"log_sweep\""));
        assert!(json.contains("\"drive_level_dbfs\":-12"));
        assert!(json.contains("\"k_main_trim_db\":0"));
        assert!(json.contains("\"trim_policy\":\"record_only\""));
        assert!(json.contains("\"abort_on_clip\":true"));
        assert!(json.contains("\"render_frame_index\":"));
        assert!(json.contains("\"repeat_render_frames\":["));
        assert!(json.contains("\\\"q\\\""), "quote must be escaped: {json}");
        assert!(json.contains("\\\\x"), "backslash must be escaped: {json}");
    }

    #[test]
    fn sweep_cancel_stops_with_decreasing_fade_and_reports_cancelled() {
        struct CancelAfterSink {
            inner: FakeSweepSink,
            cancel: CancelToken,
            after_blocks: u64,
        }
        impl SweepPlaybackSink for CancelAfterSink {
            fn write_block(
                &mut self,
                block: &SweepBlockInfo,
                planar: &[f32],
            ) -> Result<SweepSinkAck, SweepError> {
                let ack = self.inner.write_block(block, planar)?;
                if self.inner.blocks().len() as u64 >= self.after_blocks {
                    self.cancel.request();
                }
                Ok(ack)
            }
            fn finish(&mut self) -> Result<SweepSinkAck, SweepError> {
                self.inner.finish()
            }
            fn abort(&mut self, reason: SweepStopReason) -> Result<(), SweepError> {
                self.inner.abort(reason)
            }
        }

        let config = long_config();
        let timeline = compiled(&config);
        let mut player = SweepPlayer::new(config).expect("player");
        let cancel = CancelToken::new();
        let mut sink = CancelAfterSink {
            inner: FakeSweepSink::new(
                timeline.playback_channels as usize,
                timeline.block_frames as usize,
            ),
            cancel: cancel.clone(),
            after_blocks: 155,
        };
        let error = player
            .run(&mut sink, &cancel)
            .expect_err("cancel must stop the sweep");
        assert!(error.is_cancelled(), "unexpected error: {error}");
        let report = error.report().expect("report").clone();
        assert_eq!(report.outcome, SweepOutcome::Cancelled);
        assert_eq!(report.stop_reason, Some(SweepStopReason::Cancelled));
        assert_eq!(report.stop_block_seq, Some(155));
        assert_eq!(report.cancel_fade_blocks, 2);
        assert_eq!(report.blocks_written, 155 + report.cancel_fade_blocks);
        assert!(report.frames_written < report.planned_frames);
        assert_eq!(sink.inner.aborted, Some(SweepStopReason::Cancelled));
        assert!(!sink.inner.finished);
        assert_eq!(sink.inner.blocks().len() as u64, report.blocks_written);
        let last = sink.inner.blocks().last().expect("fade block");
        assert_ne!(last.info.desc.flags & BLOCK_FLAG_DRAIN_TAIL, 0);
        assert!(last.info.desc.valid_frames < timeline.block_frames as u32);
        let failure = error.stage_failure();
        assert!(failure.cancelled);
        assert_eq!(failure.code, "CANCELLED");
    }

    #[test]
    fn sweep_clip_stop_fades_and_reports_clip() {
        let config = long_config();
        let timeline = compiled(&config);
        let mut player = SweepPlayer::new(config).expect("player");
        let mut sink = FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        );
        sink.inject_clipped_at_block = Some(155);
        let cancel = CancelToken::new();
        let error = player
            .run(&mut sink, &cancel)
            .expect_err("clip protection must stop the sweep");
        assert!(error.is_clip_stopped(), "unexpected error: {error}");
        let report = error.report().expect("report").clone();
        assert_eq!(report.outcome, SweepOutcome::ClipStopped);
        assert_eq!(report.stop_reason, Some(SweepStopReason::ClipDetected));
        assert_eq!(report.clipped_samples, 1);
        assert_eq!(report.stop_block_seq, Some(156));
        assert_eq!(report.cancel_fade_blocks, 2);
        assert_eq!(report.blocks_written, 156 + report.cancel_fade_blocks);
        assert_eq!(sink.aborted, Some(SweepStopReason::ClipDetected));
        assert!(!sink.finished);
        let last = sink.blocks().last().expect("fade block");
        assert_ne!(last.info.desc.flags & BLOCK_FLAG_DRAIN_TAIL, 0);
        let failure = error.stage_failure();
        assert!(!failure.cancelled);
        assert_eq!(failure.code, "E_SWEEP_CLIP");
    }

    #[test]
    fn sweep_clip_without_abort_continues_to_completion() {
        let mut config = short_config(1, 1);
        config.protection.abort_on_clip = false;
        let timeline = compiled(&config);
        let mut player = SweepPlayer::new(config).expect("player");
        let mut sink = FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        );
        sink.inject_clipped_at_block = Some(3);
        let cancel = CancelToken::new();
        let report = player.run(&mut sink, &cancel).expect("run completes");
        assert_eq!(report.outcome, SweepOutcome::Completed);
        assert_eq!(report.clipped_samples, 1);
        assert_eq!(report.blocks_written, timeline.total_blocks);
        assert!(sink.finished);
        assert!(sink.aborted.is_none());
    }

    #[test]
    fn sweep_sink_failure_propagates_without_finish() {
        let config = long_config();
        let timeline = compiled(&config);
        let mut player = SweepPlayer::new(config).expect("player");
        let mut sink = FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        );
        sink.fail_at_block = Some(2);
        let cancel = CancelToken::new();
        let error = player.run(&mut sink, &cancel).expect_err("sink failure");
        match &error {
            SweepError::Sink { code, .. } => assert_eq!(*code, "E_FAKE_IO"),
            other => panic!("unexpected error: {other}"),
        }
        assert!(!sink.finished);
        assert!(sink.aborted.is_none());
    }

    #[test]
    fn sweep_samples_are_deterministic_across_runs() {
        let config = short_config(2, 1);
        let (report_a, sink_a, timeline) = run_ok(config.clone());
        let (report_b, sink_b, _timeline) = run_ok(config);
        assert_eq!(report_a.peak, report_b.peak);
        for channel in 0..sink_a.channels().len() {
            assert_eq!(sink_a.channel(channel), sink_b.channel(channel));
        }
        assert_eq!(sink_a.frame_count(), timeline.total_frames as usize);
        assert_eq!(sink_b.frame_count(), timeline.total_frames as usize);
    }

    #[test]
    fn sweep_second_run_is_rejected() {
        let config = short_config(1, 1);
        let timeline = compiled(&config);
        let mut player = SweepPlayer::new(config).expect("player");
        let cancel = CancelToken::new();
        let mut first = FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        );
        player.run(&mut first, &cancel).expect("first run");
        let mut second = FakeSweepSink::new(
            timeline.playback_channels as usize,
            timeline.block_frames as usize,
        );
        let error = player.run(&mut second, &cancel).expect_err("second run");
        assert!(matches!(error, SweepError::InvalidState(_)));
    }

    #[test]
    fn sweep_render_block_validates_buffer_and_sequence() {
        let config = short_config(1, 1);
        let timeline = compiled(&config);
        let player = SweepPlayer::new(config).expect("player");
        let mut small = vec![0.0f32; 3];
        let error = player.render_block(0, &mut small).expect_err("buffer");
        assert!(matches!(error, SweepError::InvalidBuffer { .. }));
        let mut buffer =
            vec![0.0f32; timeline.playback_channels as usize * timeline.block_frames as usize];
        let error = player
            .render_block(timeline.total_blocks, &mut buffer)
            .expect_err("block seq");
        assert!(matches!(error, SweepError::InvalidBlockSeq { .. }));
        let info = player.render_block(0, &mut buffer).expect("block 0");
        assert_ne!(info.desc.flags & BLOCK_FLAG_EPOCH_FIRST, 0);
        assert_eq!(info.desc.frame_index, 0);
        assert_eq!(
            timeline.first_sweep_render_frame(),
            Some(timeline.pre_frames)
        );
    }

    #[test]
    fn sweep_96k_block128_timeline_compiles() {
        let mut config = short_config(1, 1);
        config.sample_rate = 96_000;
        config.block_frames = 128;
        config.stimulus = SweepStimulus::log_sweep(20.0, 20_000.0, 0.05, -12.0, 2.0, 1, 0.2);
        let timeline = compiled(&config);
        assert_eq!(timeline.sample_rate, 96_000);
        assert_eq!(timeline.block_frames, 128);
        assert_eq!(
            timeline.block_deadline_ns,
            block_deadline_ns_of(128, 96_000)
        );
        assert_eq!(timeline.pre_frames % 128, 0);
        let mut too_high = config.clone();
        too_high.stimulus.end_hz = 21_000.0;
        assert_invalid(&too_high, "end_hz");
    }

    #[test]
    fn sweep_default_constructors_match_acceptance_defaults() {
        let loopback = SweepConfig::loopback("loop", outputs(2));
        assert_eq!(loopback.session_kind, SweepSessionKind::Loopback);
        assert_eq!(loopback.stimulus.level_dbfs, T1_LOOPBACK_LEVEL_DBFS);
        assert_eq!(
            loopback.stimulus.repeat_gap_seconds,
            LOOPBACK_REPEAT_GAP_SECONDS
        );
        assert_eq!(loopback.stimulus.kind, SweepKind::LogSweep);
        assert_eq!(loopback.k_main_trim_db, 0.0);
        assert_eq!(loopback.trim_policy, TrimPolicy::RecordOnly);
        let timeline = compiled(&loopback);
        assert!(timeline.total_frames > 0);
        assert!(timeline.first_sweep_render_frame().unwrap_or(0) > 0);

        let acoustic = SweepConfig::acoustic("acoustic", outputs(2));
        assert_eq!(acoustic.session_kind, SweepSessionKind::Acoustic);
        assert_eq!(acoustic.stimulus.level_dbfs, ACOUSTIC_LEVEL_DBFS);
        assert_eq!(
            acoustic.stimulus.repeat_gap_seconds,
            ACOUSTIC_REPEAT_GAP_SECONDS
        );
        assert_eq!(acoustic.stimulus.repeats, 3);
        assert_eq!(acoustic.stimulus.fade_ms, DEFAULT_FADE_MS);
        let timeline = compiled(&acoustic);
        assert!(timeline.first_sweep_render_frame().unwrap_or(0) > 0);
    }
}
