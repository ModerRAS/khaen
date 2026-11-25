//! [NONRT] 真实校准 stage backend（T60 复合 backend 的设备测量段） ——T67 前置。
//!
//! 职责：把 T61（逐输出扫频 render）/T62（UMIK 采集 + 时基报告）接到 T60 的
//! CalibrationJobBackend 上，使 JobManager 能真正驱动 MOTU 播放 + UMIK-1（或 MOTU 参考麦
//! ch0 同源环回）采集，并在 preflight/sweep_capture/quality_check/remeasure/holdout 上执行
//! 超时/削波/SNR/xrun/电平/采样率保护。**未接线的 stage（fit/solve/compile/prepared/apply）
//! 一律如实失败**（E_STAGE_NOT_WIRED），不得伪造成功。
//!
//! 关键口径（任务卡 T67 前置）：
//! - 采样率：声学 UMIK 路径只允许 **48000 Hz**（UMIK-1 仅 48k）；MOTU 电学环回路径允许
//!   48000/96000；**192000 一律显式拒绝**（E_STAGE_RATE_192K_FORBIDDEN）。
//! - 电平：默认 **−50 dBFS**（STAGE_DEFAULT_DRIVE_LEVEL_DBFS），可配；> −30 dBFS 视为
//!   “更大音量”，必须显式授权（hot_level_authorized，仅白天授权窗口由
//!   rt-stack/evidence/board-window-runbook/ 约束）；绝对上限 −1 dBFS（峰值门）。
//! - 保护：采集峰值 ≤ −1 dBFS 且无数字削波、xrun/overrun = 0、带限前静音 SNR ≥ 25 dB、
//!   采集时长覆盖时间轴 ≥ 95%、onset 可检出；不满足则本 stage 失败（如实，不降级 PASS）。
//! - 设备：播放 MOTU（默认 hw:4,0 / 22ch / S24_3LE / 128 帧 / 512 buffer）；采集 UMIK-1
//!   （默认 hw:3,0 / 1ch）或 MOTU 电学 reference 输入通道（同源环回，如 ch7/1-based→0-based）。
//!
//! 设备引擎 seam（可注入，离线可测）：
//! - StageAudioEngine：FakeEngine（离线合成，测试/干跑）、ToolsEngine（板端 smoke：
//!   ALSA arecord/aplay 子进程，S24_3LE raw；与 calibrate/BOARD-T1-T5-RUNBOOK.md 同一套
//!   板端既定工具链）。
//! - 生产实时路径（新 host 内 ALSA 写/读线程 + SPSC）属 T17/T26 接线，本模块**不冒充**；
//!   板端 smoke 通过后该 seam 由 T26 注入 in-process engine 即可替换（EngineKind）。
//!
//! 边界（NOT_RUN）：本机（Windows）不开真实声卡/不上板；真实 MOTU/UMIK 板上 smoke 标 NOT_RUN，
//! 待授权板窗（rt-stack/evidence/cursor-fix-stage-backend/README.md 与 board-window-runbook）。
//! 不碰 RT 图、不抢设备 owner（owner 交接由 T60 JobManager + SafeOutputController 负责）、
//! 不启动 Camilla/旧 C 流程。写域：本文件 + tests/T67。

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use khaen_core::abi::{sample_format, ABI_VERSION};
use khaen_core::rt::{BLOCK_SIZES, MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ};
use khaen_modal_design::ingest::SampleFormat;
use khaen_rt_host::format::{self, DitherCfg, FormatConfig, TspiHwParamsV1};

use crate::capture::{
    self, CaptureError, ElectricalReferenceCapture, ElectricalReferenceChannel,
    ElectricalReferenceConfig, OnsetConfig, ReferencePlan, RepeatGrid, UmikCapture,
    UmikCaptureConfig,
};
use crate::ir_extract::{extract_irs, IrExtractConfig};
use crate::job::{
    sha256_hex, ArtifactKind, CalibrationJobBackend, CancelToken, JobRequest, JobStage, StageCtx,
    StageFailure,
};
use crate::noise_est::{self, RobustSnrConfig, RobustSnrEstimate};
use crate::rt60::{
    analyze_ir, analyze_wav_path, csd_json, encode_wav_pcm24, status_json, warning_json,
    Rt60AnalyzedIr, Rt60Config, Rt60Report, Rt60Source,
};
use crate::sweep::{
    SweepConfig, SweepError, SweepOutput, SweepPlayer, SweepProtection, SweepSessionKind,
    SweepStimulus, SweepTimeline, PLACEHOLDER_GRAPH_ID,
};

// ---------------------------------------------------------------------------
// 常量与稳定错误码
// ---------------------------------------------------------------------------

/// 证据/report schema（本模块产物）。
pub const STAGE_BACKEND_SCHEMA: &str = "tspi-cal-stage-backend/v1";
/// backend 版本（artifact version 用）。
pub const STAGE_BACKEND_VERSION: &str = "stage-backend-v1";

/// 默认驱动电平（dBFS）：T67 板窗首次 smoke 的保守默认，−50 dBFS。
pub const STAGE_DEFAULT_DRIVE_LEVEL_DBFS: f64 = -50.0;
/// 允许的最低驱动电平（dBFS；与 T61 JobRequest 的 −60 下限同口径）。
pub const STAGE_MIN_DRIVE_LEVEL_DBFS: f64 = -60.0;
/// 绝对驱动电平上限（dBFS；再高必破 −1 dBFS 峰值门，显式拒绝）。
pub const STAGE_MAX_DRIVE_LEVEL_DBFS: f64 = -1.0;
/// “更大音量”阈值（dBFS）：超过它必须显式 hot_level_authorized（白天授权窗口）。
pub const STAGE_HOT_LEVEL_THRESHOLD_DBFS: f64 = -30.0;

/// 声学 UMIK 采集唯一允许的采样率（UMIK-1 原生 48k；T62 校验同口径）。
pub const STAGE_UMIK_SAMPLE_RATE_HZ: u32 = 48_000;
/// MOTU 电学环回允许的采样率（core-v1 §3.1 runtime 集合）。
pub const STAGE_LOOPBACK_SAMPLE_RATES_HZ: [u32; 2] = [48_000, 96_000];
/// 任务卡硬禁止的采样率（192k 一律拒绝，即使设备支持）。
pub const STAGE_FORBIDDEN_SAMPLE_RATES_HZ: [u32; 1] = [192_000];

/// 默认采集峰值门（dBFS；ACCEPTANCE §0.5：≤ −1 dBFS 且无数字削波）。
pub const STAGE_PEAK_CEILING_DBFS: f64 = -1.0;
/// 默认最低 SNR（dB；ACCEPTANCE T2 带限 200–8k ≥ 25 dB 的保守宽带近似门）。
/// T3 remeasure 用 `--min-snr-db` 显式冻结门值（`meas-fix` 证据里记录实际值）。
pub const STAGE_MIN_SNR_DB: f64 = 25.0;
/// SNR 门实现口径（证据冻结）：稳健宽带估计（T63 带限/相干门仍属未接线，不能冒充）。
pub const SNR_GATE_MODE_ROBUST_BROADBAND: &str = "robust-broadband";
/// apply-host 退出到 remeasure 采集之间的最小 settle（ms；流切换 pop 的物理隔离）。
pub const MIN_APPLY_SETTLE_MS: u64 = 500;
/// apply-host 退出后 settle 的缺省值（ms；1 s 覆盖 MOTU USB 流停止/altset 切换）。
pub const DEFAULT_APPLY_SETTLE_MS: u64 = 1_000;
/// 默认最大允许 xrun/overrun 数。
pub const STAGE_MAX_XRUNS: u64 = 0;

/// 采样率被任务卡硬禁（192k）。
pub const E_RATE_192K_FORBIDDEN: &str = "E_STAGE_RATE_192K_FORBIDDEN";
/// 声学 UMIK 路径只允许 48k。
pub const E_RATE_UMIK_48K_ONLY: &str = "E_STAGE_RATE_UMIK_48K_ONLY";
/// 其余不支持的采样率。
pub const E_RATE_UNSUPPORTED: &str = "E_STAGE_RATE_UNSUPPORTED";
/// 设备不可用/无法打开（如实 NOT_RUN 口径）。
pub const E_DEVICE_UNAVAILABLE: &str = "E_STAGE_DEVICE_UNAVAILABLE";
/// 采集/播放超时。
pub const E_CAPTURE_TIMEOUT: &str = "E_STAGE_CAPTURE_TIMEOUT";
/// xrun/overrun（采集丢块或播放欠载）。
pub const E_CAPTURE_XRUN: &str = "E_STAGE_CAPTURE_XRUN";
/// 采集峰值越过 −1 dBFS 或出现削波。
pub const E_CAPTURE_CLIP: &str = "E_STAGE_CAPTURE_CLIP";
/// 采集不完整（帧数不足以覆盖时间轴）。
pub const E_CAPTURE_INCOMPLETE: &str = "E_STAGE_CAPTURE_INCOMPLETE";
/// onset 不可检出（时基不可信）。
pub const E_CAPTURE_ONSET_INVALID: &str = "E_STAGE_CAPTURE_ONSET_INVALID";
/// 带限前静音 SNR 低于门限。
pub const E_SNR_BELOW_MIN: &str = "E_STAGE_SNR_BELOW_MIN";
/// 驱动电平越界或超峰值门的安全上限。
pub const E_LEVEL_UNSAFE: &str = "E_STAGE_LEVEL_UNSAFE";
/// 更大音量未显式授权（> −30 dBFS）。
pub const E_LEVEL_NOT_AUTHORIZED: &str = "E_STAGE_LEVEL_NOT_AUTHORIZED";
/// 麦克风声明（UMIK）缺失/非法。
pub const E_MIC_DECLARATION: &str = "E_STAGE_MIC_DECLARATION";
/// 配置非法（字段级）。
pub const E_CONFIG_INVALID: &str = "E_STAGE_CONFIG_INVALID";
/// 播放侧失败（aplay 退出/断管；host-plan 下 rt-host 启动/非零退出/超时同码）。
pub const E_PLAYBACK_FAILED: &str = "E_STAGE_PLAYBACK_FAILED";
/// remeasure/holdout 走 plan-arena host 播放时，apply/compile 阶段的 plan 产物缺失。
pub const E_REMEASURE_PLAN_MISSING: &str = "E_STAGE_REMEASURE_PLAN_MISSING";
/// remeasure/holdout 走 plan-arena host 播放时，配置/几何/stereo-only 口径违例。
pub const E_REMEASURE_PLAYBACK_CONFIG: &str = "E_STAGE_REMEASURE_PLAYBACK_CONFIG";
/// 采集侧失败（arecord 退出/读错）。
pub const E_CAPTURE_FAILED: &str = "E_STAGE_CAPTURE_FAILED";
/// T62 采集 finalize/时基失败。
pub const E_CAPTURE_FINALIZE: &str = "E_STAGE_CAPTURE_FINALIZE";
/// T61 扫频配置/时间轴非法。
pub const E_SWEEP_CONFIG: &str = "E_STAGE_SWEEP_CONFIG";
/// stage 尚未接线（如实 NOT_RUN；禁止伪造成功）。
pub const E_STAGE_NOT_WIRED: &str = "E_STAGE_NOT_WIRED";
/// 进程重启后本 stage 需要的测量结果不在内存（如实提示需重跑前序 stage）。
pub const E_STAGE_RESUME_PENDING: &str = "E_STAGE_RESUME_PENDING";
/// 质量门失败（汇总）。
pub const E_QUALITY_FAILED: &str = "E_STAGE_QUALITY_FAILED";
/// 协作式取消（映射 T60 cancelled）。
pub const E_CANCELLED: &str = "CANCELLED";

// ---------------------------------------------------------------------------
// 基础错误/工具
// ---------------------------------------------------------------------------

/// 设备引擎/测量层错误（稳定 code + 人读 message；由 backend 映射为 StageFailure）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageError {
    /// 稳定错误码。
    pub code: String,
    /// 人读说明。
    pub message: String,
}

impl StageError {
    /// 构造。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        StageError {
            code: code.into(),
            message: message.into(),
        }
    }

    /// 设备不可用。
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(E_DEVICE_UNAVAILABLE, message)
    }

    /// 超时。
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(E_CAPTURE_TIMEOUT, message)
    }

    /// 取消。
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(E_CANCELLED, message)
    }

    /// → StageFailure（cancelled 标志按 code 分派）。
    pub fn stage_failure(&self) -> StageFailure {
        if self.code == E_CANCELLED {
            StageFailure::cancelled(self.message.clone())
        } else {
            StageFailure::failed(self.code.clone(), self.message.clone())
        }
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StageError {}

/// unix 秒。
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 墙钟 ns（仅用于 block 时间戳与证据；不用于 RT 时钟）。
fn mono_now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// 线性幅度 → dBFS（0 返回 −inf）。
fn dbfs_of(linear: f64) -> f64 {
    if linear > 0.0 && linear.is_finite() {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// dBFS → 线性幅度。
fn linear_of_dbfs(dbfs: f64) -> f64 {
    10.0f64.powf(dbfs / 20.0)
}

/// JSON 字符串转义（本模块字段均为 ASCII/中文标签；控制字符按 \u 转义）。
fn json_escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 输出 JSON 字符串字面量（含引号）。
fn push_json_text(out: &mut String, value: &str) {
    out.push('"');
    out.push_str(&json_escape_text(value));
    out.push('"');
}

/// 输出 f64（NaN/inf 写 null）。
fn push_json_f64(out: &mut String, value: f64) {
    if value.is_finite() {
        out.push_str(&format!("{value}"));
    } else {
        out.push_str("null");
    }
}

/// 输出 Option<f64>。
fn push_json_opt_f64(out: &mut String, value: Option<f64>) {
    match value {
        Some(v) => push_json_f64(out, v),
        None => out.push_str("null"),
    }
}

/// 64 hex 非全零（graph id 校验）。
fn is_nonzero_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}

fn invalid(field: &str, detail: impl Into<String>) -> StageFailure {
    StageFailure::failed(E_CONFIG_INVALID, format!("{field}: {}", detail.into()))
}

// ---------------------------------------------------------------------------
// 配置（设备/电平/扫频/保护）
// ---------------------------------------------------------------------------

/// 测量模式：声学 UMIK 或 MOTU 电学环回（同源参考麦 ch0）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageMeasurementMode {
    /// 声学：MOTU 播放 + UMIK-1 独立 USB ADC 1ch 采集（T2/T4/T6 口径）。
    AcousticUmik,
    /// 电学环回：MOTU 播放 + MOTU 同一设备输入通道（参考麦 ch0 同源；T1 口径）。
    ElectricalLoopback,
}

impl StageMeasurementMode {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            StageMeasurementMode::AcousticUmik => "acoustic_umik",
            StageMeasurementMode::ElectricalLoopback => "electrical_loopback",
        }
    }

    /// 解析机器名（未知拒绝）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "acoustic_umik" | "acoustic" => Some(StageMeasurementMode::AcousticUmik),
            "electrical_loopback" | "loopback" => Some(StageMeasurementMode::ElectricalLoopback),
            _ => None,
        }
    }
}

/// 设备引擎种类（可注入 seam）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Auto：板端 = ToolsEngine（arecord/aplay）；非 Linux 在 probe 阶段如实 DEVICE_UNAVAILABLE。
    Auto,
    /// 离线合成引擎（测试/干跑；证据必须标 simulated）。
    Fake,
    /// ALSA 工具子进程引擎（板端 smoke 路径）。
    Tools,
}

impl EngineKind {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            EngineKind::Auto => "auto",
            EngineKind::Fake => "fake",
            EngineKind::Tools => "tools",
        }
    }

    /// 解析（未知拒绝）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(EngineKind::Auto),
            "fake" => Some(EngineKind::Fake),
            "tools" => Some(EngineKind::Tools),
            _ => None,
        }
    }
}

/// 设备配置（播放 MOTU + 采集 UMIK/MOTU 通道）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageDeviceConfig {
    /// 播放设备（ALSA 名，例如 hw:4,0）。
    pub playback_device: String,
    /// 播放通道数（MOTU=22）。
    pub playback_channels: u16,
    /// 采集设备（UMIK: hw:3,0；MOTU 环回: hw:4,0）。
    pub capture_device: String,
    /// 采集设备读块通道数（UMIK=1；MOTU 环回=20）。
    pub capture_channels: u16,
    /// MOTU 环回参考通道（0-based）；UMIK 声学必须 None。
    pub capture_channel: Option<u16>,
    /// 采样率（见模式约束）。
    pub sample_rate: u32,
    /// render/采集块帧数（64/128）。
    pub block_frames: u16,
    /// ALSA buffer 帧数（period 整数倍）。
    pub buffer_frames: u32,
    /// 采集先起后等待的 settle（ms；板端实测建议 ≥1000）。
    pub settle_ms: u64,
    /// 设备样本格式（P1 固定 S24_3LE）。
    pub sample_format: u16,
}

impl Default for StageDeviceConfig {
    fn default() -> Self {
        StageDeviceConfig {
            playback_device: "hw:4,0".to_string(),
            playback_channels: 22,
            capture_device: "hw:3,0".to_string(),
            capture_channels: 1,
            capture_channel: None,
            sample_rate: STAGE_UMIK_SAMPLE_RATE_HZ,
            block_frames: 128,
            buffer_frames: 512,
            settle_ms: 1_200,
            sample_format: sample_format::S24_3LE_INTERLEAVED,
        }
    }
}

impl StageDeviceConfig {
    /// 严格校验（未知/越界全部拒绝；不静默降级）。
    pub fn validate(&self) -> Result<(), StageFailure> {
        if self.playback_device.trim().is_empty() || self.playback_device.chars().count() > 128 {
            return Err(invalid(
                "playback_device",
                "must be non-empty and <=128 chars",
            ));
        }
        if self.capture_device.trim().is_empty() || self.capture_device.chars().count() > 128 {
            return Err(invalid(
                "capture_device",
                "must be non-empty and <=128 chars",
            ));
        }
        if !(1..=MAX_OUTPUT_CHANNELS as u16).contains(&self.playback_channels) {
            return Err(invalid(
                "playback_channels",
                format!("must be in 1..={MAX_OUTPUT_CHANNELS}"),
            ));
        }
        if !(1..=MAX_INPUT_CHANNELS as u16).contains(&self.capture_channels) {
            return Err(invalid(
                "capture_channels",
                format!("must be in 1..={MAX_INPUT_CHANNELS}"),
            ));
        }
        if let Some(channel) = self.capture_channel {
            if channel >= self.capture_channels {
                return Err(invalid(
                    "capture_channel",
                    format!(
                        "channel {channel} out of range (capture_channels={})",
                        self.capture_channels
                    ),
                ));
            }
        }
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate)
            && !STAGE_FORBIDDEN_SAMPLE_RATES_HZ.contains(&self.sample_rate)
        {
            return Err(StageFailure::failed(
                E_RATE_UNSUPPORTED,
                format!("sample_rate {} is not a runtime rate", self.sample_rate),
            ));
        }
        if !BLOCK_SIZES.contains(&self.block_frames) {
            return Err(invalid(
                "block_frames",
                format!("must be one of {BLOCK_SIZES:?}"),
            ));
        }
        if self.buffer_frames < self.block_frames as u32
            || self.buffer_frames % self.block_frames as u32 != 0
        {
            return Err(invalid(
                "buffer_frames",
                "must be >= block_frames and an integer multiple of it",
            ));
        }
        if self.sample_format != sample_format::S24_3LE_INTERLEAVED {
            return Err(invalid(
                "sample_format",
                "P1 stage backend supports only S24_3LE (3-byte packed)",
            ));
        }
        if self.settle_ms > 10_000 {
            return Err(invalid("settle_ms", "must be <= 10000"));
        }
        Ok(())
    }

    /// 单帧单通道设备字节数（P1 S24_3LE）。
    pub const fn bytes_per_sample(&self) -> usize {
        3
    }
}

/// 电平配置（默认 −50 dBFS，可配；更大音量需显式授权）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageLevelConfig {
    /// 默认驱动电平（dBFS；CLI 未显式给 --level-db 时使用）。
    pub default_drive_level_dbfs: f64,
    /// 允许的最低电平（dBFS）。
    pub min_drive_level_dbfs: f64,
    /// 绝对允许上限（dBFS；再高必破峰值门）。
    pub max_drive_level_dbfs: f64,
    /// 超过多少 dBFS 视为“更大音量”（必须 hot_level_authorized）。
    pub hot_threshold_dbfs: f64,
    /// 是否显式授权更大音量（仅白天授权窗口；由 runbook 约束）。
    pub hot_level_authorized: bool,
}

impl Default for StageLevelConfig {
    fn default() -> Self {
        StageLevelConfig {
            default_drive_level_dbfs: STAGE_DEFAULT_DRIVE_LEVEL_DBFS,
            min_drive_level_dbfs: STAGE_MIN_DRIVE_LEVEL_DBFS,
            max_drive_level_dbfs: STAGE_MAX_DRIVE_LEVEL_DBFS,
            hot_threshold_dbfs: STAGE_HOT_LEVEL_THRESHOLD_DBFS,
            hot_level_authorized: false,
        }
    }
}

impl StageLevelConfig {
    /// 解析/校验实际驱动电平（声学口径：非有限/越界/未授权一律拒绝）。
    pub fn resolve(&self, requested_dbfs: f64) -> Result<f64, StageFailure> {
        self.resolve_for(requested_dbfs, StageMeasurementMode::AcousticUmik)
    }

    /// 按测量模式解析/校验驱动电平。
    ///
    /// - 声学（可闻扫频）：> hot_threshold_dbfs 必须显式 hot_level_authorized；
    /// - 电学环回（T1，ACCEPTANCE §0.4 固定 −12 dBFS）：不涉及声学放大，不套用可闻授权门，
    ///   但仍受 [min, max]（−60..=−1 dBFS）硬范围约束。
    pub fn resolve_for(
        &self,
        requested_dbfs: f64,
        mode: StageMeasurementMode,
    ) -> Result<f64, StageFailure> {
        if !requested_dbfs.is_finite() {
            return Err(StageFailure::failed(
                E_LEVEL_UNSAFE,
                "drive level must be finite",
            ));
        }
        if !(self.min_drive_level_dbfs..=self.max_drive_level_dbfs).contains(&requested_dbfs) {
            return Err(StageFailure::failed(
                E_LEVEL_UNSAFE,
                format!(
                    "drive level {requested_dbfs} dBFS outside [{}..={}]",
                    self.min_drive_level_dbfs, self.max_drive_level_dbfs
                ),
            ));
        }
        if mode == StageMeasurementMode::AcousticUmik
            && requested_dbfs > self.hot_threshold_dbfs
            && !self.hot_level_authorized
        {
            return Err(StageFailure::failed(
                E_LEVEL_NOT_AUTHORIZED,
                format!(
                    "level {requested_dbfs} dBFS > {} dBFS requires explicit daytime-window \
                     authorization (board-window-runbook)",
                    self.hot_threshold_dbfs
                ),
            ));
        }
        Ok(requested_dbfs)
    }

    /// 默认电平。
    pub fn default_level(&self) -> f64 {
        self.default_drive_level_dbfs
    }
}

/// 扫频参数配置（默认值 = ACCEPTANCE T2/T4 口径；板端 smoke 可缩短）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageSweepConfig {
    /// 起始频率（Hz）。
    pub start_hz: f64,
    /// 结束频率（Hz）。
    pub end_hz: f64,
    /// 单遍时长（秒）。
    pub seconds: f64,
    /// fade（ms）。
    pub fade_ms: f64,
    /// 每 slot 重复次数（1..=10）。
    pub repeats: u32,
    /// 遍间静音（秒；声学 ≥0.5，环回 ≥0.2）。
    pub repeat_gap_seconds: f64,
    /// 前静音（秒；≥0.1，建议 ≥0.75 给设备 settle）。
    pub pre_seconds: f64,
    /// 后静音（秒）。
    pub post_seconds: f64,
    /// MOTU/CueMix 主输出 trim（dB；必须记录，最好 0）。
    pub k_main_trim_db: f64,
}

impl Default for StageSweepConfig {
    fn default() -> Self {
        StageSweepConfig {
            start_hz: 20.0,
            end_hz: 20_000.0,
            seconds: 5.0,
            fade_ms: 10.0,
            repeats: 3,
            repeat_gap_seconds: 0.5,
            pre_seconds: 1.5,
            post_seconds: 1.5,
            k_main_trim_db: 0.0,
        }
    }
}

impl StageSweepConfig {
    /// 板端 smoke 短会话（1 遍 × 1 s；gap 0.5 同时满足声学最小间隔；0.75 s 前静音）。
    pub fn smoke() -> Self {
        StageSweepConfig {
            seconds: 1.0,
            repeats: 1,
            repeat_gap_seconds: 0.5,
            pre_seconds: 0.75,
            post_seconds: 0.5,
            ..StageSweepConfig::default()
        }
    }

    /// T1 电学环回口径（gap 0.2 s；电平由 StageLevelConfig 决定）。
    pub fn loopback() -> Self {
        StageSweepConfig {
            seconds: 5.0,
            repeats: 1,
            repeat_gap_seconds: 0.2,
            pre_seconds: 1.5,
            post_seconds: 1.5,
            k_main_trim_db: 0.0,
            ..StageSweepConfig::default()
        }
    }
}

/// 保护配置（超时/削波/SNR/xrun/完整性门）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageProtectionConfig {
    /// 采集峰值门（dBFS；默认 −1.0）。
    pub peak_ceiling_dbfs: f64,
    /// 最低 SNR（dB；默认 25）。
    pub min_snr_db: f64,
    /// 最大允许 xrun/overrun 数（默认 0）。
    pub max_xruns: u64,
    /// 是否要求全部 slot onset 可检出（默认 true）。
    pub require_all_onsets: bool,
    /// 采集帧数下限比例（相对时间轴总帧数；默认 0.95）。
    pub min_capture_fraction: f64,
    /// 超时额外裕量（ms；默认 5000）。
    pub timeout_slack_ms: u64,
}

impl Default for StageProtectionConfig {
    fn default() -> Self {
        StageProtectionConfig {
            peak_ceiling_dbfs: STAGE_PEAK_CEILING_DBFS,
            min_snr_db: STAGE_MIN_SNR_DB,
            max_xruns: STAGE_MAX_XRUNS,
            require_all_onsets: true,
            min_capture_fraction: 0.95,
            timeout_slack_ms: 5_000,
        }
    }
}

impl StageProtectionConfig {
    /// 严格校验。
    pub fn validate(&self) -> Result<(), StageFailure> {
        if !(-20.0..0.0).contains(&self.peak_ceiling_dbfs) {
            return Err(invalid(
                "protection.peak_ceiling_dbfs",
                "must be in [-20, 0)",
            ));
        }
        if !self.min_snr_db.is_finite() || !(0.0..=120.0).contains(&self.min_snr_db) {
            return Err(invalid("protection.min_snr_db", "must be in 0..=120"));
        }
        if !(0.5..=1.0).contains(&self.min_capture_fraction) {
            return Err(invalid(
                "protection.min_capture_fraction",
                "must be in 0.5..=1.0",
            ));
        }
        Ok(())
    }
}

/// 二次测量（Remeasure / IndependentHoldout）的播放路径。
///
/// - `Aplay`（默认）：保持既有行为——T61 timeline 经 aplay 管道直放；
/// - `HostPlan`：把同一 T61 timeline 先渲染成 2ch S24_3LE 48k WAV，再交给
///   `khaen-rt-host run --config <plan-arena>` 以 `[capture] source="file"` 播放，
///   使测量回放真的经过已 apply 的 plan（stereo 2.0 单点路径）。
///
/// 只对 Remeasure / IndependentHoldout 生效；首次 sweep_capture 必须仍用 aplay
/// （否则测到的是已校正状态，逻辑错误）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemeasurePlayback {
    /// 既有 aplay 直放（bit-identical 行为）。
    Aplay,
    /// khaen-rt-host plan-arena + file 输入（stereo 2.0 单点，2ch plan）。
    HostPlan,
}

impl RemeasurePlayback {
    /// 机器名（CLI/证据）。
    pub fn as_str(self) -> &'static str {
        match self {
            RemeasurePlayback::Aplay => "aplay",
            RemeasurePlayback::HostPlan => "host-plan",
        }
    }

    /// 解析 CLI 值（未知 = None，不静默降级）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "aplay" => Some(RemeasurePlayback::Aplay),
            "host-plan" => Some(RemeasurePlayback::HostPlan),
            _ => None,
        }
    }
}

/// remeasure/holdout 走 plan-arena host 播放的参数（RemeasurePlayback::HostPlan 时生效）。
///
/// 只写文件/起进程，不打开设备：真实 device owner 由 rt-host 进程持有，UMIK 仍由
/// 本 crate 的 arecord 采集（与 acoustic-verify A/B 实验同一路径）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemeasureHostConfig {
    /// khaen-rt-host 可执行文件（默认 env `TSPI_RT_HOST_BIN` 或 PATH 中 `khaen-rt-host`）。
    pub host_bin: PathBuf,
    /// host ALSA backend：`alsa`（真实 MOTU 播放/窗口）|`fake`（仅离线接线诊断，不打开设备，
    /// 因此产不出声音 → 保护门会如实失败，不得当真实测量）。
    pub backend: String,
    /// host 打开 MOTU duplex 时钟所需的 capture 宽度（默认 20，与 apply-alsa/A-B 证据一致；
    /// 与 UMIK 采集宽度无关）。
    pub capture_channels: u16,
    /// 显式 plan.bin（缺省按 apply/compile 产物解析；用于 retest/replay）。
    pub plan_path: Option<PathBuf>,
    /// apply-host 的 plan bundle 目录（CLI 在 `--apply-host` 时传 `<out>/host-apply`）：
    /// 优先于 job artifacts 的 compile/plan.bin（同一 apply 会话写出的 plan 字节 + manifest）。
    pub apply_bundle_dir: Option<PathBuf>,
    /// 播放证据落盘目录（缺省 = job artifacts 下 `remeasure-host-plan/`）。
    pub work_dir: Option<PathBuf>,
}

impl Default for RemeasureHostConfig {
    fn default() -> Self {
        RemeasureHostConfig {
            // 与 host_apply 同语义：env TSPI_RT_HOST_BIN 优先，否则 PATH 中 khaen-rt-host。
            host_bin: crate::host_apply::HostApplyConfig::default().host_bin,
            backend: "alsa".to_string(),
            capture_channels: 20,
            plan_path: None,
            apply_bundle_dir: None,
            work_dir: None,
        }
    }
}

impl RemeasureHostConfig {
    /// 结构校验（不触碰设备/文件）。
    pub fn validate(&self) -> Result<(), StageFailure> {
        if self.host_bin.as_os_str().is_empty() {
            return Err(invalid("remeasure_host.host_bin", "must be non-empty"));
        }
        if self.backend != "alsa" && self.backend != "fake" {
            return Err(invalid(
                "remeasure_host.backend",
                format!(
                    "only alsa|fake allowed (got {:?}); host-plan 的真实测量必须是 alsa",
                    self.backend
                ),
            ));
        }
        if self.capture_channels == 0 {
            return Err(invalid(
                "remeasure_host.capture_channels",
                "must be > 0（host duplex 时钟所需 MOTU capture 宽度）",
            ));
        }
        Ok(())
    }
}

/// 真实 stage backend 配置。
#[derive(Debug, Clone, PartialEq)]
pub struct StageBackendConfig {
    /// 测量模式。
    pub mode: StageMeasurementMode,
    /// 设备。
    pub devices: StageDeviceConfig,
    /// UMIK 声明（声学模式必填；含 cal_file/sha256/orientation）。
    pub mic: Option<crate::capture::UmikDeviceConfig>,
    /// 电平。
    pub level: StageLevelConfig,
    /// 扫频。
    pub sweep: StageSweepConfig,
    /// 保护。
    pub protection: StageProtectionConfig,
    /// 逐输出（布局解析结果；顺序 = 播放顺序）。
    pub outputs: Vec<SweepOutput>,
    /// 引擎种类。
    pub engine: EngineKind,
    /// session label 前缀（证据）。
    pub session_label: String,
    /// host graph id（64 hex 非全零；默认占位）。
    pub graph_id: String,
    /// host epoch id（>0）。
    pub epoch_id: u64,
    /// apply-host 退出 → remeasure 采集之间的 settle（ms；0 = 显式关闭，仅离线/诊断用；
    /// 非零必须 ≥ MIN_APPLY_SETTLE_MS）。apply-host 关闭 MOTU USB 播放流时会产生流切换
    /// pop（g0-g1 snr-cliff.json：t≈6 ms、峰值 −31.57 dBFS），物理上把它隔离在采集窗之外。
    pub apply_settle_ms: u64,
    /// 二次测量（Remeasure / IndependentHoldout）播放路径；默认 aplay 保持既有行为。
    pub remeasure_playback: RemeasurePlayback,
    /// host-plan 播放参数（仅 remeasure_playback = HostPlan 生效）。
    pub remeasure_host: RemeasureHostConfig,
}

impl Default for StageBackendConfig {
    fn default() -> Self {
        StageBackendConfig {
            mode: StageMeasurementMode::AcousticUmik,
            devices: StageDeviceConfig::default(),
            mic: None,
            level: StageLevelConfig::default(),
            sweep: StageSweepConfig::default(),
            protection: StageProtectionConfig::default(),
            outputs: Vec::new(),
            engine: EngineKind::Auto,
            session_label: "stage".to_string(),
            graph_id: PLACEHOLDER_GRAPH_ID.to_string(),
            epoch_id: 1,
            apply_settle_ms: DEFAULT_APPLY_SETTLE_MS,
            remeasure_playback: RemeasurePlayback::Aplay,
            remeasure_host: RemeasureHostConfig::default(),
        }
    }
}

/// 外部（真实）反卷积 IR 输入（RT60/CSD 后处理用；--rt60-ir）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rt60ExternalIr {
    /// 标签（通道/测量点；artifact 文件名会安全化）。
    pub label: String,
    /// WAV 路径（PCM 16/24/32-bit；取第 0 声道）。
    pub path: PathBuf,
}

/// RT60/CSD 后处理配置（默认开启；失败不阻断主流程，写 warning artifact）。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60StageConfig {
    /// 是否开启（--no-rt60 关闭；关闭时写 rt60/status.json 留痕）。
    pub enabled: bool,
    /// 最大分析窗口（ms，自直达声起；默认 900 = sweep 反卷积 tail 0.9 s）。
    pub window_ms: f64,
    /// CSD 窗口（ms；默认 600）。
    pub csd_window_ms: f64,
    /// 反卷积 IR 尾部（ms；默认 900）。
    pub tail_ms: f64,
    /// 倍频带中心频率（Hz）。
    pub bands_hz: Vec<f64>,
    /// 外部真实 IR（非空时优先用这些 IR，不再反卷积 job 采集）。
    pub external_irs: Vec<Rt60ExternalIr>,
}

impl Default for Rt60StageConfig {
    fn default() -> Self {
        Rt60StageConfig {
            enabled: true,
            window_ms: crate::rt60::DEFAULT_WINDOW_MS,
            csd_window_ms: crate::rt60::DEFAULT_CSD_WINDOW_MS,
            tail_ms: crate::ir_extract::DEFAULT_TAIL_S * 1000.0,
            bands_hz: crate::rt60::DEFAULT_BANDS_HZ.to_vec(),
            external_irs: Vec::new(),
        }
    }
}

impl Rt60StageConfig {
    /// 显式关闭（--no-rt60）。
    pub fn disabled() -> Self {
        Rt60StageConfig {
            enabled: false,
            ..Rt60StageConfig::default()
        }
    }

    /// 转成 RT60 分析配置。
    pub fn rt60_config(&self) -> Rt60Config {
        Rt60Config {
            bands_hz: self.bands_hz.clone(),
            window_ms: self.window_ms,
            csd_window_ms: self.csd_window_ms,
        }
    }

    /// 严格校验（配置非法时 CLI/调用方应早失败；job 内失败也只会 warning 不阻断）。
    pub fn validate(&self) -> Result<(), StageFailure> {
        self.rt60_config()
            .validate()
            .map_err(|message| invalid("rt60", message))?;
        if !self.tail_ms.is_finite() || !(50.0..=5000.0).contains(&self.tail_ms) {
            return Err(invalid(
                "rt60.tail_ms",
                "must be finite and in 50..=5000",
            ));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.external_irs.len());
        for ir in &self.external_irs {
            if ir.label.trim().is_empty() || ir.label.chars().count() > 64 {
                return Err(invalid(
                    "rt60.external_ir.label",
                    "must be non-empty and <=64 chars",
                ));
            }
            if seen.contains(&ir.label.as_str()) {
                return Err(invalid(
                    "rt60.external_ir.label",
                    format!("duplicate label {}", ir.label),
                ));
            }
            seen.push(ir.label.as_str());
        }
        Ok(())
    }
}

/// 单个 RT60 artifact（stage 负责写入并登记；kind 用于 ArtifactKind::Other）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rt60ArtifactFile {
    /// artifact kind 标签。
    pub kind: &'static str,
    /// job artifact 相对路径。
    pub path: String,
    /// 文件字节。
    pub bytes: Vec<u8>,
}

/// RT60 artifact version（job manifest 记录）。
pub const RT60_ARTIFACT_VERSION: &str = "rt60-rust-v1";

/// 构建 RT60/CSD artifacts（纯函数，可单测；失败 -> warning artifact，绝不 panic）。
///
/// 数据来源优先级：
/// 1. config.external_irs 非空 -> 外部真实反卷积 IR（离线证据路径）；
/// 2. 否则 -> 反卷积本次 job 的采集（T62 samples + T61 timeline）。
pub fn build_rt60_artifacts(
    config: &Rt60StageConfig,
    measurement: &MeasurementOutput,
    prefix: &str,
    stage: &str,
) -> Vec<Rt60ArtifactFile> {
    let base = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let mut files: Vec<Rt60ArtifactFile> = Vec::new();
    if !config.enabled {
        files.push(Rt60ArtifactFile {
            kind: "rt60_status",
            path: format!("{base}rt60/status.json"),
            bytes: status_json(false, stage, "disabled by configuration (--no-rt60)")
                .into_bytes(),
        });
        return files;
    }
    if measurement.summary.mode != StageMeasurementMode::AcousticUmik {
        files.push(Rt60ArtifactFile {
            kind: "rt60_status",
            path: format!("{base}rt60/status.json"),
            bytes: status_json(
                true,
                stage,
                "not applicable: electrical loopback has no room decay",
            )
            .into_bytes(),
        });
        return files;
    }
    let mut errors: Vec<String> = Vec::new();
    if let Err(error) = config.validate() {
        files.push(Rt60ArtifactFile {
            kind: "rt60_warning",
            path: format!("{base}rt60/warning.json"),
            bytes: warning_json(stage, &error.message, &errors).into_bytes(),
        });
        return files;
    }
    let rt60_config = config.rt60_config();
    let summary = &measurement.summary;
    let simulated = summary.engine == "fake-synth";
    let real_measurement = if simulated {
        "NOT_RUN"
    } else {
        "EXECUTED_ON_HOST"
    };
    let external = !config.external_irs.is_empty();
    let mut entries: Vec<Rt60AnalyzedIr> = Vec::new();
    if external {
        for ir in &config.external_irs {
            match analyze_wav_path(&ir.path, &ir.label, &rt60_config) {
                Ok((analysis, bytes)) => entries.push(Rt60AnalyzedIr {
                    analysis,
                    source_path: Some(ir.path.display().to_string()),
                    ir_sha256: Some(sha256_hex(&bytes)),
                    ir_wav_path: None,
                    repeats_averaged: 1,
                }),
                Err(error) => errors.push(format!("{}: {error}", ir.label)),
            }
        }
    } else {
        match (measurement.timeline.as_ref(), measurement.samples.is_empty()) {
            (Some(timeline), false) => {
                let extract_config = IrExtractConfig {
                    tail_s: config.tail_ms / 1000.0,
                    ..IrExtractConfig::default()
                };
                match extract_irs(
                    timeline,
                    &measurement.samples,
                    summary.seed_render_frame,
                    &extract_config,
                ) {
                    Ok(deconvolved) => {
                        for ir in deconvolved {
                            let wav = encode_wav_pcm24(&ir.ir, ir.sample_rate_hz);
                            let wav_path = format!(
                                "{base}rt60/ir/{}.wav",
                                crate::rt60::safe_artifact_label(&ir.label)
                            );
                            files.push(Rt60ArtifactFile {
                                kind: "rt60_ir",
                                path: wav_path.clone(),
                                bytes: wav.clone(),
                            });
                            match analyze_ir(
                                &ir.ir,
                                ir.sample_rate_hz,
                                &ir.label,
                                &rt60_config,
                            ) {
                                Ok(analysis) => entries.push(Rt60AnalyzedIr {
                                    analysis,
                                    source_path: None,
                                    ir_sha256: Some(sha256_hex(&wav)),
                                    ir_wav_path: Some(wav_path),
                                    repeats_averaged: ir.repeats_averaged,
                                }),
                                Err(error) => errors.push(format!("{}: {error}", ir.label)),
                            }
                        }
                    }
                    Err(error) => errors.push(format!("deconvolve: {error}")),
                }
            }
            _ => errors.push(
                "measurement output has no capture samples/timeline; cannot deconvolve IR"
                    .to_string(),
            ),
        }
    }
    if entries.is_empty() {
        files.push(Rt60ArtifactFile {
            kind: "rt60_warning",
            path: format!("{base}rt60/warning.json"),
            bytes: warning_json(
                stage,
                "RT60 post-processing failed for every IR (main pipeline continues)",
                &errors,
            )
            .into_bytes(),
        });
        return files;
    }
    let source = Rt60Source {
        kind: if external {
            "external_ir"
        } else {
            "deconvolved_capture"
        },
        stage: stage.to_string(),
        engine: summary.engine.clone(),
        simulated,
        real_measurement,
        ir_origin: if external {
            "external deconvolved IR WAVs supplied via --rt60-ir (offline replay; not this job's capture)".to_string()
        } else {
            format!(
                "deconvolved from this job's capture (T61 sweep + T62 capture, tail_ms={})",
                config.tail_ms
            )
        },
        capture_sha256: if external {
            None
        } else {
            Some(sha256_hex(&measurement.raw))
        },
        sweep_start_hz: measurement.timeline.as_ref().map(|t| t.stimulus.start_hz),
        sweep_end_hz: measurement.timeline.as_ref().map(|t| t.stimulus.end_hz),
        sweep_seconds: measurement
            .timeline
            .as_ref()
            .map(|t| t.actual_stimulus.seconds),
    };
    let report = Rt60Report {
        config: rt60_config.clone(),
        source,
        generated_unix: unix_now(),
        irs: entries,
        errors: errors.clone(),
    };
    files.push(Rt60ArtifactFile {
        kind: "rt60_summary",
        path: format!("{base}rt60/summary.json"),
        bytes: report.summary_json().into_bytes(),
    });
    files.push(Rt60ArtifactFile {
        kind: "rt60_csv",
        path: format!("{base}rt60/rt60.csv"),
        bytes: report.rt60_csv().into_bytes(),
    });
    for entry in &report.irs {
        files.push(Rt60ArtifactFile {
            kind: "rt60_csd",
            path: format!(
                "{base}csd/{}.json",
                crate::rt60::safe_artifact_label(&entry.analysis.label)
            ),
            bytes: csd_json(entry, config.csd_window_ms).into_bytes(),
        });
    }
    if !errors.is_empty() {
        files.push(Rt60ArtifactFile {
            kind: "rt60_warning",
            path: format!("{base}rt60/warning.json"),
            bytes: warning_json(
                stage,
                "RT60 post-processing partially failed; successful IRs are recorded",
                &errors,
            )
            .into_bytes(),
        });
    }
    files
}

impl StageBackendConfig {
    /// 构造 UMIK-1 声明（供 CLI/调用方）。
    pub fn umik1_declaration(
        device: impl Into<String>,
        serial: impl Into<String>,
        cal_file: impl Into<String>,
        cal_sha256: impl Into<String>,
        orientation: impl Into<String>,
    ) -> crate::capture::UmikDeviceConfig {
        crate::capture::UmikDeviceConfig::umik1(device, serial, cal_file, cal_sha256, orientation)
    }

    /// apply 后 settle 校验（0 = 显式关闭；否则必须 ≥ MIN_APPLY_SETTLE_MS 且 ≤ 5000）。
    pub fn validate_apply_settle(apply_settle_ms: u64) -> Result<(), StageFailure> {
        if apply_settle_ms != 0 && apply_settle_ms < MIN_APPLY_SETTLE_MS {
            return Err(invalid(
                "apply_settle_ms",
                format!("nonzero apply settle must be >= {MIN_APPLY_SETTLE_MS} ms (stream-close pop isolation)"),
            ));
        }
        if apply_settle_ms > 5_000 {
            return Err(invalid("apply_settle_ms", "must be <= 5000"));
        }
        Ok(())
    }

    /// 严格校验（模式/电平/扫频/输出/设备/保护）。
    pub fn validate(&self) -> Result<(), StageFailure> {
        self.devices.validate()?;
        Self::validate_apply_settle(self.apply_settle_ms)?;
        self.protection.validate()?;
        self.remeasure_host.validate()?;
        if self.remeasure_playback == RemeasurePlayback::HostPlan
            && self.mode != StageMeasurementMode::AcousticUmik
        {
            // host-plan 只用于声学 remeasure：loopback 的采集设备是 MOTU，host 也打开 MOTU
            // duplex → 设备 owner 冲突（NOT_IMPLEMENTED，不硬塞）。
            return Err(StageFailure::failed(
                E_REMEASURE_PLAYBACK_CONFIG,
                "remeasure-playback=host-plan is NOT_IMPLEMENTED for electrical loopback                  (host would own the same MOTU device as the reference capture); only acoustic UMIK",
            ));
        }
        if self.outputs.is_empty() {
            return Err(invalid("outputs", "at least one output is required"));
        }
        let mut seen_channels = Vec::with_capacity(self.outputs.len());
        for output in &self.outputs {
            if output.speaker.trim().is_empty() {
                return Err(invalid("outputs.speaker", "must be non-empty"));
            }
            if output.channel >= self.devices.playback_channels as u32 {
                return Err(invalid(
                    "outputs.channel",
                    format!(
                        "channel {} >= playback_channels {}",
                        output.channel, self.devices.playback_channels
                    ),
                ));
            }
            if seen_channels.contains(&output.channel) {
                return Err(invalid(
                    "outputs.channel",
                    format!("duplicate playback channel {}", output.channel),
                ));
            }
            seen_channels.push(output.channel);
        }
        if !is_nonzero_hex64(&self.graph_id) {
            return Err(invalid(
                "graph_id",
                "must be 64 lowercase hex chars and non-zero",
            ));
        }
        if self.epoch_id == 0 {
            return Err(invalid("epoch_id", "must be > 0"));
        }
        if self.session_label.trim().is_empty() || self.session_label.chars().count() > 64 {
            return Err(invalid("session_label", "must be non-empty and <=64 chars"));
        }
        if !self.sweep.start_hz.is_finite()
            || !self.sweep.end_hz.is_finite()
            || self.sweep.start_hz <= 0.0
            || self.sweep.end_hz <= self.sweep.start_hz
        {
            return Err(invalid(
                "sweep",
                "start_hz/end_hz must be finite and increasing",
            ));
        }
        if !self.sweep.seconds.is_finite() || !(0.05..=120.0).contains(&self.sweep.seconds) {
            return Err(invalid("sweep.seconds", "must be in 0.05..=120"));
        }
        if !(1..=10).contains(&self.sweep.repeats) {
            return Err(invalid("sweep.repeats", "must be in 1..=10"));
        }
        if !self.sweep.pre_seconds.is_finite() || self.sweep.pre_seconds < 0.1 {
            return Err(invalid("sweep.pre_seconds", "must be >= 0.1"));
        }
        if !self.sweep.post_seconds.is_finite() || self.sweep.post_seconds < 0.1 {
            return Err(invalid("sweep.post_seconds", "must be >= 0.1"));
        }
        if !self.sweep.k_main_trim_db.is_finite() {
            return Err(invalid("sweep.k_main_trim_db", "must be finite"));
        }
        match self.mode {
            StageMeasurementMode::AcousticUmik => {
                // 真实设备适配：UMIK-1 在本机（dsp-host card0）暴露 2ch S24_3LE，
                // 必须在 hw 层按 2ch 采集、再显式选 1ch dense 给 T62；1ch 设备仍支持。
                // 只放开到 1..=2，且 >1 时必须显式声明选哪一路（不猜）。
                if !(1..=2).contains(&self.devices.capture_channels) {
                    return Err(invalid(
                        "devices.capture_channels",
                        "acoustic UMIK capture must be 1 or 2 channels (device read width)",
                    ));
                }
                if self.devices.capture_channels == 1 {
                    if self.devices.capture_channel.is_some() {
                        return Err(invalid(
                            "devices.capture_channel",
                            "1ch UMIK capture must not declare capture_channel",
                        ));
                    }
                } else {
                    match self.devices.capture_channel {
                        Some(channel) if channel < self.devices.capture_channels => {}
                        Some(channel) => {
                            return Err(invalid(
                                "devices.capture_channel",
                                format!(
                                    "capture_channel {channel} >= capture_channels {}",
                                    self.devices.capture_channels
                                ),
                            ))
                        }
                        None => {
                            return Err(invalid(
                                "devices.capture_channel",
                                "multi-channel UMIK capture requires the selected capture_channel",
                            ))
                        }
                    }
                }
                let mic = self.mic.as_ref().ok_or_else(|| {
                    StageFailure::failed(
                        E_MIC_DECLARATION,
                        "acoustic mode requires UMIK declaration \
                         (model/serial/cal_file/cal_sha256)",
                    )
                })?;
                mic.validate().map_err(|error| {
                    StageFailure::failed(
                        E_MIC_DECLARATION,
                        format!("UMIK declaration rejected: {error:?}"),
                    )
                })?;
                self.validate_rate(self.devices.sample_rate)?;
            }
            StageMeasurementMode::ElectricalLoopback => {
                if self.mic.is_some() {
                    return Err(invalid(
                        "mic",
                        "electrical loopback must not declare a UMIK (device separation rule)",
                    ));
                }
                if self.devices.capture_channel.is_none() {
                    return Err(invalid(
                        "devices.capture_channel",
                        "electrical loopback requires the MOTU reference input channel",
                    ));
                }
                self.validate_rate(self.devices.sample_rate)?;
            }
        }
        Ok(())
    }

    /// 采样率校验（192k 硬禁；UMIK 48k-only；环回 48k/96k）。
    pub fn validate_rate(&self, sample_rate: u32) -> Result<(), StageFailure> {
        if STAGE_FORBIDDEN_SAMPLE_RATES_HZ.contains(&sample_rate) {
            return Err(StageFailure::failed(
                E_RATE_192K_FORBIDDEN,
                "192000 Hz is forbidden for calibration sweeps/capture \
                 (task hard rule; UMIK-1 is 48k)",
            ));
        }
        match self.mode {
            StageMeasurementMode::AcousticUmik => {
                if sample_rate != STAGE_UMIK_SAMPLE_RATE_HZ {
                    return Err(StageFailure::failed(
                        E_RATE_UMIK_48K_ONLY,
                        format!("acoustic UMIK-1 path supports only 48000 Hz (got {sample_rate})"),
                    ));
                }
                Ok(())
            }
            StageMeasurementMode::ElectricalLoopback => {
                if !STAGE_LOOPBACK_SAMPLE_RATES_HZ.contains(&sample_rate) {
                    return Err(StageFailure::failed(
                        E_RATE_UNSUPPORTED,
                        format!(
                            "electrical loopback supports only {STAGE_LOOPBACK_SAMPLE_RATES_HZ:?} \
                             (got {sample_rate})"
                        ),
                    ));
                }
                Ok(())
            }
        }
    }

    /// request 的采样率必须与配置一致（不一致 = 配置/请求漂移，如实拒绝）。
    pub fn check_request_rate(&self, request_rate: u32) -> Result<(), StageFailure> {
        self.validate_rate(request_rate)?;
        if request_rate != self.devices.sample_rate {
            return Err(invalid(
                "sample_rate",
                format!(
                    "request {request_rate} Hz != configured device rate {} Hz",
                    self.devices.sample_rate
                ),
            ));
        }
        Ok(())
    }

    /// 构造引擎（Auto/Tools → ToolsEngine；Fake → FakeEngine）。
    pub fn build_engine(&self) -> Box<dyn StageAudioEngine> {
        match self.engine {
            EngineKind::Fake => Box::new(FakeEngine::new(FakeEngineConfig::default())),
            EngineKind::Auto | EngineKind::Tools => Box::new(ToolsEngine::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// 设备引擎 seam
// ---------------------------------------------------------------------------

/// host-plan 播放的一次会话规格（remeasure/holdout 专用；stereo 2.0 单点 2ch plan）。
///
/// 由 `StageBackend::secondary_measure` 在 plan 产物解析（apply bundle / compile artifacts）
/// 与几何校验之后构造；`ToolsEngine` 在 arecord 采集开始后启动 host，并把 T61 timeline
/// 先渲染成 `wav_path`（file 输入）。
#[derive(Debug, Clone, PartialEq)]
pub struct HostPlanPlayback {
    /// khaen-rt-host 可执行文件。
    pub host_bin: PathBuf,
    /// rt-host 配置文件路径（会话前写入 `config_text`）。
    pub config_path: PathBuf,
    /// rt-host 配置全文（plan-arena + [capture] source="file"）。
    pub config_text: String,
    /// T61 timeline 渲染出的 2ch S24_3LE 48k WAV（file 输入）。
    pub wav_path: PathBuf,
    /// host stdout JSON 原样落盘路径（证据）。
    pub report_path: PathBuf,
    /// plan 输入通道数（本轮 stereo 单点必须 = 2；= file WAV 声道数）。
    pub input_channels: u16,
    /// host ALSA backend（alsa|fake；证据）。
    pub backend: String,
    /// host 进程超时（ms；0 = 用 AudioSessionRequest::timeout_ms）。
    pub timeout_ms: u64,
    /// plan 字节 sha256（证据/交叉校验用）。
    pub plan_sha256: String,
    /// plan header graph_id（64 hex；与 applied_version 交叉校验）。
    pub plan_graph_id: String,
    /// plan.bin 路径（[paths] plan 的来源；证据）。
    pub plan_path: PathBuf,
    /// compile-report.json 路径（[paths] plan_manifest；证据）。
    pub plan_manifest_path: PathBuf,
}

/// 一次会话的播放侧执行方式。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionPlayback {
    /// 既有 aplay 管道直放（默认；sweep_capture 固定走这里）。
    Aplay,
    /// khaen-rt-host plan-arena 从 file 输入播放（remeasure/holdout 可选）。
    HostPlan(HostPlanPlayback),
}

/// 一次采集会话的设备请求。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSessionRequest {
    /// 测量模式。
    pub mode: StageMeasurementMode,
    /// 采样率。
    pub sample_rate: u32,
    /// block/period 帧数。
    pub block_frames: u32,
    /// 播放通道数。
    pub playback_channels: u16,
    /// 采集设备读取的通道数（UMIK=1；MOTU 环回=n_in）。
    pub capture_channels: u16,
    /// MOTU 环回参考通道（0-based；UMIK 为 None）。
    pub capture_channel: Option<u16>,
    /// 设备样本格式（S24_3LE）。
    pub sample_format: u16,
    /// 播放设备。
    pub playback_device: String,
    /// 采集设备。
    pub capture_device: String,
    /// 采集先起后等待 settle（ms）。
    pub settle_ms: u64,
    /// 会话超时（ms；播放时长 + settle + 裕量，由 backend 计算）。
    pub timeout_ms: u64,
    /// 期望采集帧数（时间轴总帧数；用于完整性检查）。
    pub expected_capture_frames: u64,
    /// 播放侧执行方式（默认 Aplay；sweep_capture 恒 Aplay）。
    pub playback: SessionPlayback,
}

/// 一个采集读块（原始设备字节 + 墙钟 ns 时间戳）。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCaptureBlock {
    /// 块首样本的墙钟 ns（严格递增）。
    pub mono_ns: u64,
    /// 原始设备字节（UMIK=1ch dense；MOTU=interleaved n_in）。
    pub bytes: Vec<u8>,
}

/// 一次采集会话结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCaptureResult {
    /// 采集原始字节（块按顺序拼接；UMIK=1ch dense；MOTU=interleaved）。
    pub raw: Vec<u8>,
    /// 逐块日志。
    pub blocks: Vec<AudioCaptureBlock>,
    /// 采集帧数。
    pub capture_frames: u64,
    /// xrun/overrun 数。
    pub xruns: u64,
    /// 播放欠载（underrun）数。
    pub underruns: u64,
    /// 估计 seed：capture sample 0 对应的 render frame（负值 = 采集早于 render 0）。
    pub seed_render_frame: f64,
    /// 会话墙钟秒数。
    pub wall_seconds: f64,
    /// 引擎说明（证据）。
    pub notes: String,
}

/// 设备探针结果（preflight 用）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StageProbe {
    /// 播放设备是否可用。
    pub playback_ok: bool,
    /// 采集设备是否可用。
    pub capture_ok: bool,
    /// 采集设备支持的采样率（空 = 未知）。
    pub capture_sample_rates: Vec<u32>,
    /// 采集设备支持的最大通道数（0 = 未知）。
    pub capture_max_channels: u16,
    /// 播放设备支持的采样率（空 = 未知）。
    pub playback_sample_rates: Vec<u32>,
    /// 备注（命令/设备名等）。
    pub notes: String,
}

/// 播放流源：由 T61 timeline 逐 block 渲染 planar f32 并给出设备字节。
pub trait PlaybackSource {
    /// 总帧数（= 时间轴 total_frames）。
    fn total_frames(&self) -> u64;
    /// block 帧数。
    fn block_frames(&self) -> u32;
    /// 播放通道数。
    fn channels(&self) -> u16;
    /// 渲染下一个 block：填 planar（channels × block_frames，dense）与设备字节；
    /// None = 全部写完。取消时返回 E_CANCELLED。
    fn next_block(
        &mut self,
        planar: &mut [f32],
        bytes: &mut Vec<u8>,
    ) -> Result<Option<u32>, StageError>;
}

/// 设备引擎 seam：探针 + 一次「播放 + 采集」会话。
pub trait StageAudioEngine: Send {
    /// 引擎名（证据）。
    fn name(&self) -> &'static str;

    /// 探针（preflight；不播放）。
    fn probe(&mut self, devices: &StageDeviceConfig) -> Result<StageProbe, StageError>;

    /// 执行一次会话：并发播放 + 采集；返回采集结果（错误 = 保护/设备失败）。
    fn run_session(
        &mut self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
    ) -> Result<AudioCaptureResult, StageError>;
}

// ---------------------------------------------------------------------------
// FakeEngine（离线合成；确定性；测试/干跑）
// ---------------------------------------------------------------------------

/// Fake 引擎配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FakeEngineConfig {
    /// 采集先从 render 0 起多少帧（前置静音；决定 seed）。
    pub preroll_frames: u64,
    /// 采集尾部补多少帧（避免 onset 落到末尾外）。
    pub tail_frames: u64,
    /// 采集相对 render 的额外偏移帧（模拟 USB ADC 时基偏差；正值 = 采集晚）。
    pub latency_frames: u64,
    /// 声学延迟（帧；模拟传播延迟）。
    pub delay_frames: u64,
    /// 采集增益（dB）。
    pub gain_db: f64,
    /// 噪声底（dBFS）。
    pub noise_dbfs: f64,
    /// 报告 xrun 数。
    pub xruns: u64,
    /// 报告 underrun 数。
    pub underruns: u64,
    /// 强制设备不可用（负向用例）。
    pub force_unavailable: bool,
    /// 强制超时（负向用例）。
    pub force_timeout: bool,
    /// 强制采集削波（峰值档位 1.0，负向用例）。
    pub force_clip: bool,
    /// 噪声 LCG 种子（确定性）。
    pub noise_seed: u64,
}

impl Default for FakeEngineConfig {
    fn default() -> Self {
        FakeEngineConfig {
            preroll_frames: 2_400,
            tail_frames: 2_400,
            latency_frames: 0,
            delay_frames: 0,
            gain_db: 0.0,
            noise_dbfs: -90.0,
            xruns: 0,
            underruns: 0,
            force_unavailable: false,
            force_timeout: false,
            force_clip: false,
            noise_seed: 0x5EED_5EED_5EED_5EED,
        }
    }
}

/// 离线合成引擎：把 playback 流（planar f32）合成为 UMIK/MOTU 采集字节。
#[derive(Debug, Clone)]
pub struct FakeEngine {
    config: FakeEngineConfig,
}

impl FakeEngine {
    /// 构造。
    pub fn new(config: FakeEngineConfig) -> Self {
        FakeEngine { config }
    }

    /// 配置。
    pub fn config(&self) -> &FakeEngineConfig {
        &self.config
    }

    fn write_s24(sample: f32, out: &mut Vec<u8>) {
        let scaled = (sample as f64 * 8_388_608.0).round_ties_even();
        let code = scaled.clamp(-8_388_608.0, 8_388_607.0) as i32;
        let bytes = code.to_le_bytes();
        out.extend_from_slice(&bytes[0..3]);
    }

    fn next_noise(state: &mut u64) -> f32 {
        // PCG 风格 LCG：仅用于确定性伪噪声（非加密/非 RT）。
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = ((*state >> 33) as u32) as f32 / (u32::MAX as f32);
        unit * 2.0 - 1.0
    }
}

impl StageAudioEngine for FakeEngine {
    fn name(&self) -> &'static str {
        "fake-synth"
    }

    fn probe(&mut self, devices: &StageDeviceConfig) -> Result<StageProbe, StageError> {
        if self.config.force_unavailable {
            return Err(StageError::unavailable(
                "fake engine: devices forced unavailable",
            ));
        }
        Ok(StageProbe {
            playback_ok: true,
            capture_ok: true,
            capture_sample_rates: vec![devices.sample_rate],
            capture_max_channels: devices.capture_channels,
            playback_sample_rates: vec![devices.sample_rate],
            notes: format!(
                "fake-synth: playback {}ch, capture {}ch @ {} Hz",
                devices.playback_channels, devices.capture_channels, devices.sample_rate
            ),
        })
    }

    fn run_session(
        &mut self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
    ) -> Result<AudioCaptureResult, StageError> {
        if self.config.force_unavailable {
            return Err(StageError::unavailable(
                "fake engine: devices forced unavailable",
            ));
        }
        if self.config.force_timeout {
            return Err(StageError::timeout("fake engine: forced session timeout"));
        }
        let start = Instant::now();
        let block = request.block_frames as usize;
        let playback_channels = request.playback_channels as usize;
        let capture_channels = request.capture_channels as usize;
        let capture_channel = request.capture_channel.unwrap_or(0) as usize;
        if block == 0
            || playback_channels == 0
            || capture_channels == 0
            || capture_channel >= capture_channels
        {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                "fake engine: invalid session geometry",
            ));
        }
        let total_frames = playback.total_frames();
        let capture_frames = self
            .config
            .preroll_frames
            .saturating_add(total_frames)
            .saturating_add(self.config.tail_frames)
            .saturating_add(self.config.delay_frames);
        let capture_blocks = capture_frames.div_ceil(block as u64) as usize;
        let gain = linear_of_dbfs(self.config.gain_db);
        let noise = linear_of_dbfs(self.config.noise_dbfs);
        let mut delay_line: std::collections::VecDeque<f32> =
            std::collections::VecDeque::with_capacity(self.config.delay_frames as usize + 1);
        for _ in 0..self.config.delay_frames {
            delay_line.push_back(0.0);
        }
        // 先把整条 playback 流读成 mono 混音（fake 引擎离线合成，可承受短会话内存）。
        let mut planar = vec![0.0f32; playback_channels * block];
        let mut playback_bytes = Vec::with_capacity(playback_channels * block * 3);
        let mut mono: Vec<f32> = Vec::with_capacity(total_frames as usize);
        while (mono.len() as u64) < total_frames {
            playback_bytes.clear();
            let Some(frames) = playback.next_block(&mut planar, &mut playback_bytes)? else {
                break;
            };
            if frames as usize != block {
                return Err(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("fake engine: playback block frames {frames} != {block}"),
                ));
            }
            for frame in 0..block {
                let mut sum = 0.0f32;
                for channel in 0..playback_channels {
                    sum += planar[channel * block + frame];
                }
                mono.push(sum * gain as f32);
            }
        }
        if (mono.len() as u64) != total_frames {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "fake engine: playback stream delivered {} of {} frames",
                    mono.len(),
                    total_frames
                ),
            ));
        }

        let mut raw = Vec::with_capacity(capture_blocks * block * capture_channels * 3);
        let mut blocks = Vec::with_capacity(capture_blocks);
        let mut mono_ns = mono_now_ns();
        let ns_per_block = (block as u64 * 1_000_000_000) / request.sample_rate as u64;
        let mut noise_state = self.config.noise_seed;
        let lead = self.config.preroll_frames + self.config.latency_frames;

        for block_index in 0..capture_blocks {
            let mut bytes = Vec::with_capacity(block * capture_channels * 3);
            for frame in 0..block {
                let capture_pos = block_index as u64 * block as u64 + frame as u64;
                let signal = if capture_pos < lead {
                    delay_line.push_back(0.0);
                    delay_line.pop_front().unwrap_or(0.0)
                } else {
                    let render_frame = capture_pos - lead;
                    let source = mono.get(render_frame as usize).copied().unwrap_or(0.0);
                    delay_line.push_back(source);
                    delay_line.pop_front().unwrap_or(0.0)
                };
                let mut sample = signal as f64 + noise * Self::next_noise(&mut noise_state) as f64;
                if self.config.force_clip && capture_pos == self.config.preroll_frames {
                    sample = 1.0;
                }
                let sample = sample.clamp(-1.0, 1.0) as f32;
                for channel in 0..capture_channels {
                    if channel == capture_channel {
                        Self::write_s24(sample, &mut bytes);
                    } else {
                        bytes.extend_from_slice(&[0, 0, 0]);
                    }
                }
            }
            raw.extend_from_slice(&bytes);
            blocks.push(AudioCaptureBlock { mono_ns, bytes });
            mono_ns = mono_ns.saturating_add(ns_per_block);
        }

        // seed = render frame 0 对应的 capture sample index（采集早于 render 0 时为正）。
        let seed = self.config.preroll_frames as f64 + self.config.latency_frames as f64;
        Ok(AudioCaptureResult {
            raw,
            capture_frames,
            blocks,
            xruns: self.config.xruns,
            underruns: self.config.underruns,
            seed_render_frame: seed,
            wall_seconds: start.elapsed().as_secs_f64(),
            notes: format!(
                "fake-synth: preroll={} delay={} gain={} dB noise={} dBFS",
                self.config.preroll_frames,
                self.config.delay_frames,
                self.config.gain_db,
                self.config.noise_dbfs
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// ToolsEngine（板端 smoke：ALSA arecord/aplay 子进程，S24_3LE raw）
// ---------------------------------------------------------------------------

/// ALSA 工具子进程引擎（arecord 采集 + aplay 播放）。
///
/// 说明：这是**板端 smoke 桥接路径**，与 calibrate/BOARD-T1-T5-RUNBOOK.md 的既定板端工具链
/// （arecord/aplay + S24_3LE raw）一致；生产实时路径（新 host 内 ALSA 线程 + SPSC）属
/// T17/T26 接线，由 StageAudioEngine seam 注入替换。不启动 Camilla/旧 C 进程。
#[derive(Debug, Clone)]
pub struct ToolsEngine {
    /// arecord 可执行文件（默认 PATH 中 arecord）。
    pub arecord_bin: String,
    /// aplay 可执行文件（默认 PATH 中 aplay）。
    pub aplay_bin: String,
}

impl Default for ToolsEngine {
    fn default() -> Self {
        ToolsEngine {
            arecord_bin: "arecord".to_string(),
            aplay_bin: "aplay".to_string(),
        }
    }
}

impl ToolsEngine {
    /// 默认（PATH 中的 arecord/aplay）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 解析 --dump-hw-params stderr 中的 RATE/CHANNELS（失败则返回空 = 未知，不猜）。
    fn parse_hw_params_caps(text: &str) -> (Vec<u32>, u16) {
        let mut rates = Vec::new();
        let mut channels = 0u16;
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("RATE:") {
                for token in rest.split_whitespace() {
                    if let Ok(value) = token.parse::<u32>() {
                        if !rates.contains(&value) {
                            rates.push(value);
                        }
                    }
                }
            } else if let Some(rest) = line.strip_prefix("CHANNELS:") {
                for token in rest.split_whitespace() {
                    if let Ok(value) = token.parse::<u16>() {
                        channels = channels.max(value);
                    }
                }
            }
        }
        (rates, channels)
    }

    fn probe_one(
        &self,
        binary: &str,
        device: &str,
        channels: u16,
        sample_rate: u32,
        duration_seconds: Option<u64>,
    ) -> Result<(bool, Vec<u32>, u16, String), StageError> {
        // 显式 raw + S24_3LE + 通道/采样率：否则 aplay 会先读文件头失败、arecord 用默认
        // S16 在 UMIK/MOTU 上直接 "Sample format non available"，dump 根本走不到。
        // 播放侧 /dev/null = EOF（0 帧，不出声）；采集侧 -d 1 保证有界（绝不无限录）。
        let channels_text = channels.to_string();
        let rate_text = sample_rate.to_string();
        let seconds_text = duration_seconds.map(|seconds| seconds.to_string());
        let mut command = Command::new(binary);
        command.args([
            "-q",
            "--dump-hw-params",
            "-t",
            "raw",
            "-f",
            "S24_3LE",
            "-c",
            channels_text.as_str(),
            "-r",
            rate_text.as_str(),
        ]);
        if let Some(seconds) = seconds_text.as_deref() {
            command.args(["-d", seconds]);
        }
        command.args(["-D", device, "/dev/null"]);
        let output = command.stdin(Stdio::null()).output().map_err(|error| {
            StageError::unavailable(format!(
                "spawn {binary} for {device}: {error} (board smoke requires Linux ALSA \
                     arecord/aplay; local dry-run uses --engine fake)"
            ))
        })?;
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let (rates, channels) = Self::parse_hw_params_caps(&stderr);
        // 真实设备的 --dump-hw-params 用法天然是"先 dump、再打开设备播放/录制文件"：
        // /dev/null 无法作为合法 WAV/原始流，aplay/arecord 在 dump 完之后必然以非零退出
        // （aplay: read_header；arecord: 默认 S16 在该设备不可用）。因此**以 dump 是否成功
        // 为准**（出现 "HW Params of device"/RATE/CHANNELS），而不是进程退出码；
        // 设备被占用/不存在时不会产生 dump，仍如实 false。
        let dumped = stderr.contains("HW Params of device") || !rates.is_empty() || channels > 0;
        if !output.status.success() && !dumped {
            return Ok((
                false,
                Vec::new(),
                0,
                format!(
                    "{binary} -D {device} --dump-hw-params exit {:?}; stderr: {}",
                    output.status,
                    stderr.trim().chars().take(200).collect::<String>()
                ),
            ));
        }
        Ok((
            true,
            rates,
            channels,
            format!(
                "{binary} {device}: hwparams dumped (exit {:?} after dump)",
                output.status
            ),
        ))
    }
}

impl StageAudioEngine for ToolsEngine {
    fn name(&self) -> &'static str {
        "alsa-tools"
    }

    fn probe(&mut self, devices: &StageDeviceConfig) -> Result<StageProbe, StageError> {
        let (playback_ok, playback_rates, _, playback_note) = self.probe_one(
            &self.aplay_bin,
            &devices.playback_device,
            devices.playback_channels,
            devices.sample_rate,
            None,
        )?;
        let (capture_ok, capture_rates, capture_channels, capture_note) = self.probe_one(
            &self.arecord_bin,
            &devices.capture_device,
            devices.capture_channels,
            devices.sample_rate,
            Some(1),
        )?;
        Ok(StageProbe {
            playback_ok,
            capture_ok,
            capture_sample_rates: capture_rates,
            capture_max_channels: capture_channels,
            playback_sample_rates: playback_rates,
            notes: format!("{playback_note}; {capture_note}"),
        })
    }

    fn run_session(
        &mut self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
    ) -> Result<AudioCaptureResult, StageError> {
        // 二次测量可选走 plan-arena host（只对 Remeasure/Holdout 注入；sweep 恒 Aplay）。
        if let SessionPlayback::HostPlan(spec) = &request.playback {
            return self.run_session_host_plan(request, playback, spec);
        }
        let start = Instant::now();
        let block = request.block_frames as usize;
        let capture_channels = request.capture_channels as usize;
        let playback_channels = request.playback_channels as usize;
        if block == 0 || capture_channels == 0 || playback_channels == 0 {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                "alsa-tools: zero block/channels",
            ));
        }
        let capture_period_bytes = block * capture_channels * 3;
        let playback_period_bytes = block * playback_channels * 3;
        let rate = request.sample_rate.to_string();
        let block_text = request.block_frames.to_string();
        let buffer_text = (request.block_frames as u32 * 4).to_string();
        let capture_channels_text = request.capture_channels.to_string();
        let playback_channels_text = request.playback_channels.to_string();

        let mut capture_child = Command::new(&self.arecord_bin)
            .args([
                "-q",
                "-t",
                "raw",
                "-f",
                "S24_3LE",
                "-c",
                capture_channels_text.as_str(),
                "-r",
                rate.as_str(),
                "-D",
                request.capture_device.as_str(),
                "--period-size",
                block_text.as_str(),
                "--buffer-size",
                buffer_text.as_str(),
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                StageError::unavailable(format!(
                    "spawn {} for capture {}: {error}",
                    self.arecord_bin, request.capture_device
                ))
            })?;
        let capture_stdout = capture_child
            .stdout
            .take()
            .ok_or_else(|| StageError::new(E_CAPTURE_FAILED, "arecord stdout unavailable"))?;
        let capture_stderr = capture_child
            .stderr
            .take()
            .ok_or_else(|| StageError::new(E_CAPTURE_FAILED, "arecord stderr unavailable"))?;

        let (block_tx, block_rx): (Sender<AudioCaptureBlock>, Receiver<AudioCaptureBlock>) =
            mpsc::channel();
        let reader = thread::spawn(move || {
            let mut stdout = capture_stdout;
            let mut buffer = vec![0u8; capture_period_bytes];
            loop {
                match stdout.read_exact(&mut buffer) {
                    Ok(()) => {
                        if block_tx
                            .send(AudioCaptureBlock {
                                mono_ns: mono_now_ns(),
                                bytes: buffer.clone(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut text = String::new();
            let mut stream = capture_stderr;
            let _ = stream.read_to_string(&mut text);
            text
        });

        // 采集先起；settle 后开始播放（UMIK USB 需要稳定时间）。
        thread::sleep(Duration::from_millis(request.settle_ms));

        let mut playback_child = match Command::new(&self.aplay_bin)
            .args([
                "-q",
                "-t",
                "raw",
                "-f",
                "S24_3LE",
                "-c",
                playback_channels_text.as_str(),
                "-r",
                rate.as_str(),
                "-D",
                request.playback_device.as_str(),
                "--period-size",
                block_text.as_str(),
                "--buffer-size",
                buffer_text.as_str(),
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = capture_child.kill();
                let _ = capture_child.wait();
                return Err(StageError::unavailable(format!(
                    "spawn {} for playback {}: {error}",
                    self.aplay_bin, request.playback_device
                )));
            }
        };
        let mut playback_stdin = playback_child
            .stdin
            .take()
            .ok_or_else(|| StageError::new(E_PLAYBACK_FAILED, "aplay stdin unavailable"))?;
        let playback_stderr = playback_child.stderr.take();
        let playback_stderr_reader = thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = playback_stderr {
                let _ = stream.read_to_string(&mut text);
            }
            text
        });

        let deadline = start + Duration::from_millis(request.timeout_ms);
        let mut planar = vec![0.0f32; playback_channels * block];
        let mut playback_bytes = Vec::with_capacity(playback_period_bytes);
        let mut captured: Vec<AudioCaptureBlock> = Vec::new();
        let mut first_capture_ns: Option<u64> = None;
        let mut first_playback_ns: Option<u64> = None;
        let mut timed_out = false;
        let mut playback_error: Option<StageError> = None;

        loop {
            if Instant::now() > deadline {
                timed_out = true;
                break;
            }
            playback_bytes.clear();
            let next = playback.next_block(&mut planar, &mut playback_bytes)?;
            let Some(frames) = next else { break };
            if frames as usize != block {
                playback_error = Some(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("playback block frames {frames} != {block}"),
                ));
                break;
            }
            if first_playback_ns.is_none() {
                first_playback_ns = Some(mono_now_ns());
            }
            if let Err(error) = playback_stdin.write_all(&playback_bytes) {
                playback_error = Some(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("write playback block: {error}"),
                ));
                break;
            }
            while let Ok(block) = block_rx.try_recv() {
                if first_capture_ns.is_none() {
                    first_capture_ns = Some(block.mono_ns);
                }
                captured.push(block);
            }
        }
        drop(playback_stdin);

        // 等 aplay drain 结束；超时/失败则 kill。
        let mut aplay_ok = true;
        loop {
            match playback_child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        aplay_ok = false;
                    }
                    break;
                }
                Ok(None) => {
                    if Instant::now() > deadline {
                        timed_out = true;
                        let _ = playback_child.kill();
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => {
                    let _ = playback_child.kill();
                    break;
                }
            }
        }
        let _ = capture_child.kill();
        let _ = capture_child.wait();
        let _ = reader.join();
        let capture_stderr_text = stderr_reader.join().unwrap_or_default();
        let playback_stderr_text = playback_stderr_reader.join().unwrap_or_default();
        let mut remaining: Vec<AudioCaptureBlock> = block_rx.try_iter().collect();
        for block in remaining.drain(..) {
            if first_capture_ns.is_none() {
                first_capture_ns = Some(block.mono_ns);
            }
            captured.push(block);
        }
        if timed_out {
            return Err(StageError::timeout(format!(
                "session exceeded {} ms (captured {} blocks)",
                request.timeout_ms,
                captured.len()
            )));
        }
        if let Some(error) = playback_error {
            return Err(error);
        }
        if !aplay_ok {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "aplay exited non-zero; stderr: {}",
                    playback_stderr_text.trim()
                ),
            ));
        }
        if captured.is_empty() {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                format!(
                    "arecord produced no blocks; stderr: {}",
                    capture_stderr_text.trim()
                ),
            ));
        }
        let mut raw = Vec::with_capacity(captured.len() * capture_period_bytes);
        for block in &captured {
            raw.extend_from_slice(&block.bytes);
        }
        let capture_frames = (captured.len() * block) as u64;
        let overruns = capture_stderr_text.matches("overrun").count() as u64;
        let underruns = playback_stderr_text.matches("underrun").count() as u64;
        let seed = match (first_playback_ns, first_capture_ns) {
            (Some(play_ns), Some(cap_ns)) if play_ns > cap_ns => {
                // render frame 0 对应的 capture sample index（采集先起 => 正值）。
                (play_ns - cap_ns) as f64 * request.sample_rate as f64 / 1_000_000_000.0
            }
            _ => 0.0,
        };
        Ok(AudioCaptureResult {
            raw,
            capture_frames,
            blocks: captured,
            xruns: overruns,
            underruns,
            seed_render_frame: seed,
            wall_seconds: start.elapsed().as_secs_f64(),
            notes: format!(
                "alsa-tools: arecord={} aplay={} settle={}ms overruns={} underruns={}",
                request.capture_device,
                request.playback_device,
                request.settle_ms,
                overruns,
                underruns
            ),
        })
    }
}

impl ToolsEngine {
    /// 二次测量（Remeasure/Holdout）：T61 timeline → 2ch WAV → plan-arena host 播放，
    /// arecord 采集路径与 aplay 模式完全相同（只换播放侧）。
    ///
    /// 失败语义按既有 E_PLAYBACK_FAILED：host 启动失败/非零退出/超时都如实失败（不降级、
    /// 不伪造采集）。播放侧没有任何合成激励生成：WAV 字节来自 `TimelinePlaybackSource`
    /// （T61 timeline + T22 S24_3LE 转换），这里只落 WAV 容器与写 rt-host toml。
    fn run_session_host_plan(
        &self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
        spec: &HostPlanPlayback,
    ) -> Result<AudioCaptureResult, StageError> {
        let start = Instant::now();
        let block = request.block_frames as usize;
        let capture_channels = request.capture_channels as usize;
        let input_channels = spec.input_channels as usize;
        if block == 0 || capture_channels == 0 || input_channels == 0 {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                "alsa-tools host-plan: zero block/channels",
            ));
        }
        if playback.channels() as usize != input_channels {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "host-plan: T61 playback channels {} != plan.n_in {}（stereo 2ch 单点路径要求逐通道一致）",
                    playback.channels(),
                    input_channels
                ),
            ));
        }
        // 1) 先把整段 T61 timeline 渲染成 WAV（file 输入）；不打开设备。
        let wav_bytes = render_host_plan_input_wav(playback, spec, request.sample_rate)?;
        if let Some(parent) = spec.wav_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("host-plan: create wav dir {}: {error}", parent.display()),
                )
            })?;
        }
        std::fs::write(&spec.wav_path, &wav_bytes).map_err(|error| {
            StageError::new(
                E_PLAYBACK_FAILED,
                format!("host-plan: write wav {}: {error}", spec.wav_path.display()),
            )
        })?;
        // 2) 写 rt-host 配置（plan-arena + file 输入）；同一文本也进证据。
        if let Some(parent) = spec.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("host-plan: create config dir {}: {error}", parent.display()),
                )
            })?;
        }
        std::fs::write(&spec.config_path, spec.config_text.as_bytes()).map_err(|error| {
            StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "host-plan: write rt-host config {}: {error}",
                    spec.config_path.display()
                ),
            )
        })?;

        // 3) arecord 采集路径与 aplay 模式一致：先起采集，settle 后起播放（host）。
        let capture_period_bytes = block * capture_channels * 3;
        let rate = request.sample_rate.to_string();
        let block_text = request.block_frames.to_string();
        let buffer_text = (request.block_frames as u32 * 4).to_string();
        let capture_channels_text = request.capture_channels.to_string();
        let mut capture_child = Command::new(&self.arecord_bin)
            .args([
                "-q",
                "-t",
                "raw",
                "-f",
                "S24_3LE",
                "-c",
                capture_channels_text.as_str(),
                "-r",
                rate.as_str(),
                "-D",
                request.capture_device.as_str(),
                "--period-size",
                block_text.as_str(),
                "--buffer-size",
                buffer_text.as_str(),
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                StageError::unavailable(format!(
                    "spawn {} for capture {}: {error}",
                    self.arecord_bin, request.capture_device
                ))
            })?;
        let capture_stdout = capture_child
            .stdout
            .take()
            .ok_or_else(|| StageError::new(E_CAPTURE_FAILED, "arecord stdout unavailable"))?;
        let capture_stderr = capture_child
            .stderr
            .take()
            .ok_or_else(|| StageError::new(E_CAPTURE_FAILED, "arecord stderr unavailable"))?;
        let (block_tx, block_rx): (Sender<AudioCaptureBlock>, Receiver<AudioCaptureBlock>) =
            mpsc::channel();
        let reader = thread::spawn(move || {
            let mut stdout = capture_stdout;
            let mut buffer = vec![0u8; capture_period_bytes];
            loop {
                match stdout.read_exact(&mut buffer) {
                    Ok(()) => {
                        if block_tx
                            .send(AudioCaptureBlock {
                                mono_ns: mono_now_ns(),
                                bytes: buffer.clone(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut text = String::new();
            let mut stream = capture_stderr;
            let _ = stream.read_to_string(&mut text);
            text
        });

        thread::sleep(Duration::from_millis(request.settle_ms));

        // 4) 起 host（playback = plan-arena plan）。settle 之后才播放，保持采集先起语义。
        let host_started_ns = mono_now_ns();
        let mut host_child = match Command::new(&spec.host_bin)
            .arg("run")
            .arg("--config")
            .arg(&spec.config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = capture_child.kill();
                let _ = capture_child.wait();
                let _ = reader.join();
                return Err(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!(
                        "host-plan: spawn {} 失败：{error}（离线需 --host-bin 指向已构建的 khaen-rt-host；板端为 /opt/tspi-rt-stack/current/bin/khaen-rt-host）",
                        spec.host_bin.display()
                    ),
                ));
            }
        };
        let host_stdout = host_child.stdout.take();
        let host_output_reader = thread::spawn(move || {
            let mut bytes: Vec<u8> = Vec::new();
            if let Some(mut stream) = host_stdout {
                let _ = stream.read_to_end(&mut bytes);
            }
            bytes
        });
        let host_stderr = host_child.stderr.take();
        let host_stderr_reader = thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = host_stderr {
                let _ = stream.read_to_string(&mut text);
            }
            text
        });

        let timeout_ms = if spec.timeout_ms > 0 {
            spec.timeout_ms
        } else {
            request.timeout_ms
        };
        let deadline = start + Duration::from_millis(timeout_ms);
        let mut captured: Vec<AudioCaptureBlock> = Vec::new();
        let mut first_capture_ns: Option<u64> = None;
        let mut timed_out = false;
        let mut host_status: Option<std::process::ExitStatus> = None;
        loop {
            while let Ok(capture_block) = block_rx.try_recv() {
                if first_capture_ns.is_none() {
                    first_capture_ns = Some(capture_block.mono_ns);
                }
                captured.push(capture_block);
            }
            match host_child.try_wait() {
                Ok(Some(status)) => {
                    host_status = Some(status);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = host_child.kill();
                    let _ = host_child.wait();
                    let _ = capture_child.kill();
                    let _ = capture_child.wait();
                    let _ = reader.join();
                    return Err(StageError::new(
                        E_PLAYBACK_FAILED,
                        format!("host-plan: 等待 rt-host 失败：{error}"),
                    ));
                }
            }
            if Instant::now() > deadline {
                timed_out = true;
                let _ = host_child.kill();
                let _ = host_child.wait();
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        // 5) host 退出即停采集（与 aplay 模式一致），收集剩余块与两边 stderr/stdout。
        let _ = capture_child.kill();
        let _ = capture_child.wait();
        let _ = reader.join();
        let capture_stderr_text = stderr_reader.join().unwrap_or_default();
        let host_stdout_bytes = host_output_reader.join().unwrap_or_default();
        let host_stderr_text = host_stderr_reader.join().unwrap_or_default();
        while let Ok(capture_block) = block_rx.try_recv() {
            if first_capture_ns.is_none() {
                first_capture_ns = Some(capture_block.mono_ns);
            }
            captured.push(capture_block);
        }
        // host stdout JSON 原样落盘（证据；写失败不阻断播放结论，但会写进 notes）。
        let mut report_note = String::new();
        if !host_stdout_bytes.is_empty() {
            if let Some(parent) = spec.report_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(error) = std::fs::write(&spec.report_path, &host_stdout_bytes) {
                report_note = format!(" (host report write failed: {error})");
            }
        }
        if timed_out {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "host-plan: rt-host 超时 {} ms（{} 被 kill；stderr 尾部：{}）",
                    timeout_ms,
                    spec.host_bin.display(),
                    tail_chars(&host_stderr_text, 400)
                ),
            ));
        }
        let Some(status) = host_status else {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                "host-plan: rt-host 无退出状态（内部状态不一致）",
            ));
        };
        if !status.success() {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "host-plan: rt-host 退出码 {:?}；stderr 尾部：{}{}",
                    status.code(),
                    tail_chars(&host_stderr_text, 800),
                    report_note
                ),
            ));
        }
        if captured.is_empty() {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                format!(
                    "host-plan: arecord produced no blocks; stderr: {}",
                    capture_stderr_text.trim()
                ),
            ));
        }
        let mut raw = Vec::with_capacity(captured.len() * capture_period_bytes);
        for capture_block in &captured {
            raw.extend_from_slice(&capture_block.bytes);
        }
        let capture_frames = (captured.len() * block) as u64;
        let overruns = capture_stderr_text.matches("overrun").count() as u64;
        // host 报告 JSON 里的 ALSA 计数（不要用 stderr 子串：JSON 字段名本身含 underrun）。
        let host_xruns = json_uint_field(&host_stdout_bytes, "xruns").unwrap_or(0);
        let host_underruns = json_uint_field(&host_stdout_bytes, "underruns").unwrap_or(0);
        let seed = match first_capture_ns {
            Some(cap_ns) if host_started_ns > cap_ns => {
                (host_started_ns - cap_ns) as f64 * request.sample_rate as f64 / 1_000_000_000.0
            }
            _ => 0.0,
        };
        Ok(AudioCaptureResult {
            raw,
            capture_frames,
            blocks: captured,
            xruns: overruns + host_xruns,
            underruns: host_underruns,
            seed_render_frame: seed,
            wall_seconds: start.elapsed().as_secs_f64(),
            notes: format!(
                "host-plan: arecord={} rt-host={} backend={} exit=0 wav={} plan_sha256={} host_xruns={} host_underruns={} seed_source=spawn-clock{}",
                request.capture_device,
                spec.host_bin.display(),
                spec.backend,
                spec.wav_path.display(),
                spec.plan_sha256,
                host_xruns,
                host_underruns,
                report_note
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// T61 播放流源（时间轴逐 block render -> planar f32 + 设备字节）
// ---------------------------------------------------------------------------

/// 从 T61 SweepPlayer 逐 block 渲染的播放流（含取消/峰值门检查）。
pub struct TimelinePlaybackSource {
    player: SweepPlayer,
    playback_format: FormatConfig,
    cancel: CancelToken,
    next_block_seq: u64,
    peak_ceiling_linear: f64,
    max_peak: f32,
    frames_written: u64,
}

impl TimelinePlaybackSource {
    /// 构造（校验设备参数并预建 T22 playback 格式配置）。
    pub fn new(
        player: SweepPlayer,
        devices: &StageDeviceConfig,
        cancel: CancelToken,
        peak_ceiling_dbfs: f64,
    ) -> Result<Self, StageFailure> {
        let playback_format = playback_format_config(devices)?;
        let peak_ceiling_linear = linear_of_dbfs(peak_ceiling_dbfs);
        Ok(TimelinePlaybackSource {
            player,
            playback_format,
            cancel,
            next_block_seq: 0,
            peak_ceiling_linear,
            max_peak: 0.0,
            frames_written: 0,
        })
    }

    /// 已写出的帧数。
    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// 生成样本峰值（线性）。
    pub fn max_peak(&self) -> f32 {
        self.max_peak
    }
}

/// 构造 playback 侧 T22 FormatConfig（只消费 out 侧；in 侧给 1ch 使 validate 通过）。
fn playback_format_config(devices: &StageDeviceConfig) -> Result<FormatConfig, StageFailure> {
    let bytes = devices.bytes_per_sample() as u32;
    let period = devices.block_frames as u32;
    let n_out = devices.playback_channels as u32;
    let hw = TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: sample_format::S24_3LE_INTERLEAVED,
        n_in: 1,
        n_out: devices.playback_channels,
        sample_rate: devices.sample_rate,
        period_frames: period,
        buffer_frames: devices.buffer_frames,
        in_stride_bytes: period * bytes,
        out_stride_bytes: period * n_out * bytes,
        flags: 0,
        reserved: [0; 4],
    };
    format::validate(&FormatConfig {
        hw,
        dither: DitherCfg::Off,
    })
    .map_err(|status| {
        StageFailure::failed(
            E_CONFIG_INVALID,
            format!("T22 playback FormatConfig rejected: {status:?}"),
        )
    })?;
    Ok(FormatConfig {
        hw,
        dither: DitherCfg::Off,
    })
}

impl PlaybackSource for TimelinePlaybackSource {
    fn total_frames(&self) -> u64 {
        self.player.total_frames()
    }

    fn block_frames(&self) -> u32 {
        self.player.timeline().block_frames as u32
    }

    fn channels(&self) -> u16 {
        self.player.timeline().playback_channels
    }

    fn next_block(
        &mut self,
        planar: &mut [f32],
        bytes: &mut Vec<u8>,
    ) -> Result<Option<u32>, StageError> {
        if self.cancel.is_requested() {
            return Err(StageError::cancelled(
                "sweep playback cancelled by job cancel token",
            ));
        }
        if self.next_block_seq >= self.player.total_blocks() {
            return Ok(None);
        }
        let info = self
            .player
            .render_block(self.next_block_seq, planar)
            .map_err(|error| {
                let failure = error.stage_failure();
                StageError::new(failure.code, failure.message)
            })?;
        let mut peak = 0.0f32;
        for sample in planar.iter() {
            if !sample.is_finite() {
                return Err(StageError::new(
                    E_PLAYBACK_FAILED,
                    "generated playback sample is non-finite",
                ));
            }
            let value = sample.abs();
            if value > peak {
                peak = value;
            }
        }
        if peak as f64 > self.peak_ceiling_linear * 1.000_001 + f64::EPSILON {
            return Err(StageError::new(
                E_LEVEL_UNSAFE,
                format!(
                    "generated playback peak {} dBFS exceeds ceiling {} dBFS",
                    dbfs_of(peak as f64),
                    dbfs_of(self.peak_ceiling_linear)
                ),
            ));
        }
        if peak > self.max_peak {
            self.max_peak = peak;
        }
        let block = self.player.timeline().block_frames as usize;
        let period_bytes = block * self.player.timeline().playback_channels as usize * 3;
        bytes.clear();
        bytes.resize(period_bytes, 0);
        let _stats = format::convert_playback_strided(
            &self.playback_format,
            planar,
            bytes.as_mut_slice(),
            block,
        )
        .map_err(|status| {
            StageError::new(
                E_PLAYBACK_FAILED,
                format!("T22 convert_playback rejected: {status:?}"),
            )
        })?;
        self.next_block_seq = self.next_block_seq.saturating_add(1);
        self.frames_written = self
            .frames_written
            .saturating_add(info.desc.valid_frames as u64);
        Ok(Some(info.desc.valid_frames))
    }
}

// ---------------------------------------------------------------------------
// 二次测量 plan-arena host 播放（stereo 2.0 单点：T61 timeline -> 2ch WAV -> host）
// ---------------------------------------------------------------------------
//
// 为什么 sweep_capture 不能走 host-plan：首次测量必须测“未加校正”的房间响应，再据此拟合
// plan；若首次测量就经过 plan，测到的是已校正状态，拟合/验证闭环逻辑错误。因此 host-plan
// 只对 Remeasure / IndependentHoldout 生效。
//
// 为什么只做 stereo 2.0 单点（2ch）：rt-host 的 `[capture] source="file"` 目前只接受
// 2ch(2.0 strict) / 8ch(4.1.2 strict)；且 plan 输入通道（n_in）必须与 T61 timeline 的
// 播放通道逐通道对应。4.1.2/7ch 路径需要 8 通道 plan 输入适配，本轮 **NOT_IMPLEMENTED**
// （明确拒绝，不硬塞）。

/// 一次测量会话的播放方式（sweep_capture 恒 Aplay；二次测量可 HostPlan）。
#[derive(Debug, Clone, PartialEq)]
pub enum MeasurementPlayback {
    /// 既有 aplay 直放。
    Aplay,
    /// plan-arena host + file 输入（stereo 2.0 单点 2ch plan）。
    HostPlan(HostPlanPlayback),
}

/// plan header 的播放几何（host 启动前要与之对齐的字段；不读系数区）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlanGeometry {
    /// plan ABI 版本（1/2；host 都接受）。
    pub abi_version: u16,
    /// 采样率。
    pub sample_rate: u32,
    /// block 编码帧（必须 = 设备 period）。
    pub block_size: u16,
    /// plan 输入通道（本轮必须 = 2）。
    pub n_in: u16,
    /// plan 输出容器通道数（不能超过设备 playback 宽度）。
    pub n_out: u16,
    /// worker 数（2/4；必须 = host [host] workers，否则 host 拒绝启动）。
    pub workers: u8,
    /// plan graph_id（32B）。
    pub graph_id: [u8; 32],
}

impl HostPlanGeometry {
    /// graph_id → 64 lowercase hex（与 applied_version 交叉校验）。
    pub fn graph_id_hex(&self) -> String {
        crate::compile_apply::hex32(&self.graph_id)
    }
}

/// 已解析的 remeasure plan 产物（plan.bin + compile-report manifest + header 几何）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRemeasurePlan {
    /// plan.bin 路径（host [paths] plan）。
    pub plan_path: PathBuf,
    /// compile-report.json（host [paths] plan_manifest；校验 plan_sha256/graph_id）。
    pub manifest_path: PathBuf,
    /// plan 字节 sha256。
    pub plan_sha256: String,
    /// header 几何。
    pub geometry: HostPlanGeometry,
    /// 来源：explicit / apply-bundle / compile-artifact（证据）。
    pub source: &'static str,
}

/// 安全解析 plan header 几何字段（不 unsafe；只读前 128B 的固定偏移）。
///
/// plan header 布局冻结于 core-v1 §11.10 / errata E1：
/// magic@0(u32)、abi_version@4(u16)、header_size@6(u16)、sample_rate@12(u32)、
/// block_size@16(u16)、n_in@18(u16)、n_out@20(u16)、workers@32(u8)、graph_id@36..68。
pub fn read_host_plan_geometry(bytes: &[u8]) -> Result<HostPlanGeometry, String> {
    if bytes.len() < 128 {
        return Err(format!(
            "plan 字节 {} < 128（header 不完整；不是 T66 compile/plan.bin）",
            bytes.len()
        ));
    }
    let u16_at = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let u32_at = |offset: usize| {
        u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    };
    let magic = u32_at(0);
    if magic != khaen_core::abi::TSPI_PLAN_MAGIC {
        return Err(format!(
            "plan header.magic={magic:#010x} != TSPI_PLAN_MAGIC={:#010x}（不是 plan.bin）",
            khaen_core::abi::TSPI_PLAN_MAGIC
        ));
    }
    let abi_version = u16_at(4);
    if !khaen_core::abi::plan_abi_supported(abi_version) {
        return Err(format!(
            "plan header.abi_version={abi_version} 不被支持（只接受 ABI v1/v2）"
        ));
    }
    if u16_at(6) != 128 {
        return Err(format!("plan header.header_size={} != 128", u16_at(6)));
    }
    if bytes[34] != 0 {
        return Err(format!(
            "plan header.precision={}（只允许 0=f32）",
            bytes[34]
        ));
    }
    let mut graph_id = [0u8; 32];
    graph_id.copy_from_slice(&bytes[36..68]);
    Ok(HostPlanGeometry {
        abi_version,
        sample_rate: u32_at(12),
        block_size: u16_at(16),
        n_in: u16_at(18),
        n_out: u16_at(20),
        workers: bytes[32],
        graph_id,
    })
}

/// 解析 remeasure 播放要用的 plan 产物。
///
/// 解析顺序（**不猜路径**，全部来自代码里的 artifact 约定）：
/// 1. `host.plan_path` 显式覆盖（retest/replay；manifest 取同目录 compile-report.json）；
/// 2. `host.apply_bundle_dir`（CLI `--apply-host` 传 `<out>/host-apply`）：
///    `plan.bin` + `compile-report.json` 是 apply 阶段真实交给 host 的同一份字节；
/// 3. job artifacts 的 `compile/plan.bin` + `compile/compile-report.json`
///    （`PLAN_ARTIFACT_PATH`/`COMPILE_REPORT_ARTIFACT_PATH`；--apply 离线 ack 时也由
///    Compile stage 写出）。
///
/// 任一缺失 → `E_STAGE_REMEASURE_PLAN_MISSING`（fail-closed，不降级到 aplay）。
/// `applied_version` 非空时 plan header graph_id 必须与之一致（防止 replay/旧 bundle 漂移）。
pub fn resolve_remeasure_plan(
    artifacts_dir: &Path,
    host: &RemeasureHostConfig,
    applied_version: Option<&str>,
) -> Result<ResolvedRemeasurePlan, StageFailure> {
    let compile_plan = artifacts_dir.join(crate::compile_apply::PLAN_ARTIFACT_PATH);
    let compile_manifest = artifacts_dir.join(crate::compile_apply::COMPILE_REPORT_ARTIFACT_PATH);
    let (source, plan_path, manifest_path, expected) = if let Some(plan_path) = &host.plan_path {
        let manifest_path = plan_path
            .parent()
            .map(|parent| parent.join(crate::host_apply::HOST_MANIFEST_FILE))
            .unwrap_or_else(|| PathBuf::from(crate::host_apply::HOST_MANIFEST_FILE));
        (
            "explicit",
            plan_path.clone(),
            manifest_path.clone(),
            format!(
                "显式 --remeasure-plan {plan_path:?} 及其同目录 {}",
                manifest_path.display()
            ),
        )
    } else if let Some(bundle) = &host.apply_bundle_dir {
        let plan_path = bundle.join(crate::host_apply::HOST_PLAN_FILE);
        let manifest_path = bundle.join(crate::host_apply::HOST_MANIFEST_FILE);
        if plan_path.is_file() {
            (
                "apply-bundle",
                plan_path,
                manifest_path,
                String::new(),
            )
        } else {
            (
                "compile-artifact",
                compile_plan.clone(),
                compile_manifest.clone(),
                format!(
                    "apply bundle {} 无 {}；job artifacts 的 {} 也无",
                    bundle.display(),
                    crate::host_apply::HOST_PLAN_FILE,
                    crate::compile_apply::PLAN_ARTIFACT_PATH
                ),
            )
        }
    } else {
        (
            "compile-artifact",
            compile_plan.clone(),
            compile_manifest.clone(),
            format!(
                "job artifacts 的 {} 与 {}",
                crate::compile_apply::PLAN_ARTIFACT_PATH,
                crate::compile_apply::COMPILE_REPORT_ARTIFACT_PATH
            ),
        )
    };
    if !plan_path.is_file() || !manifest_path.is_file() {
        return Err(StageFailure::failed(
            E_REMEASURE_PLAN_MISSING,
            format!(
                "remeasure-playback=host-plan 需要 apply/compile 的 plan 产物（{source}）：{expected}；plan={} exists={}；                 manifest={} exists={}。本 job 必须先走到 Compile/Apply（--apply），或显式                  --remeasure-plan <plan.bin>。拒绝回退 aplay（否则测量路径仍不含 plan，                 “校正量”不可测）。",
                plan_path.display(),
                plan_path.is_file(),
                manifest_path.display(),
                manifest_path.is_file(),
            ),
        ));
    }
    let plan_bytes = std::fs::read(&plan_path).map_err(|error| {
        StageFailure::failed(
            E_REMEASURE_PLAN_MISSING,
            format!("读取 plan {} 失败：{error}", plan_path.display()),
        )
    })?;
    let geometry = read_host_plan_geometry(&plan_bytes).map_err(|message| {
        StageFailure::failed(
            E_REMEASURE_PLAYBACK_CONFIG,
            format!("plan {} 几何解析失败：{message}", plan_path.display()),
        )
    })?;
    if let Some(version) = applied_version {
        let expected_graph = version
            .rsplit_once(':')
            .map(|(_, graph)| graph)
            .filter(|graph| !graph.is_empty())
            .ok_or_else(|| {
                StageFailure::failed(
                    E_REMEASURE_PLAYBACK_CONFIG,
                    format!(
                        "applied_version {version:?} 不是 pv-<n>:<graph_id> 形式；无法确认测量 plan 与已 apply plan 一致"
                    ),
                )
            })?;
        let actual_graph = geometry.graph_id_hex();
        if !actual_graph.eq_ignore_ascii_case(expected_graph) {
            return Err(StageFailure::failed(
                E_REMEASURE_PLAYBACK_CONFIG,
                format!(
                    "plan artifact（{source}）graph_id={actual_graph} != applied_version {version:?} 的 graph_id={expected_graph}；                     测量会经过错误的 plan（stale artifact），拒绝启动"
                ),
            ));
        }
    }
    Ok(ResolvedRemeasurePlan {
        plan_path,
        manifest_path,
        plan_sha256: sha256_hex(&plan_bytes),
        geometry,
        source,
    })
}

/// stereo 2.0 单点口径校验（7ch/4.1.2 = NOT_IMPLEMENTED，明确拒绝）。
///
/// `require_device_width`：真实设备引擎（Tools/Auto）为 true —— plan.n_out 必须 ≤ 设备
/// playback 宽度（否则 host 会拒绝启动）；FakeEngine 离线合成不启动 host，只做配置接线，
/// 传 false（summary 会如实标 simulated/host_executed=false）。
pub fn validate_remeasure_host_plan(
    geometry: &HostPlanGeometry,
    layout: &str,
    outputs: &[SweepOutput],
    devices: &StageDeviceConfig,
    require_device_width: bool,
) -> Result<(), StageFailure> {
    let not_implemented = |detail: String| {
        StageFailure::failed(
            E_REMEASURE_PLAYBACK_CONFIG,
            format!(
                "remeasure-playback=host-plan 本轮只实现 stereo-2.0 单点（2ch plan 输入 / 2ch file 源）；                 7ch/4.1.2 = NOT_IMPLEMENTED：{detail}"
            ),
        )
    };
    if layout.trim() != "stereo-2.0" {
        return Err(not_implemented(format!("request.layout={layout:?}")));
    }
    if geometry.n_in != 2 {
        return Err(not_implemented(format!("plan.n_in={}", geometry.n_in)));
    }
    if outputs.len() != 2 {
        return Err(not_implemented(format!(
            "outputs 数 {} != 2（需要 FL/FR 两路）",
            outputs.len()
        )));
    }
    let mut channels: Vec<u32> = outputs.iter().map(|output| output.channel).collect();
    channels.sort_unstable();
    if channels != vec![0, 1] {
        return Err(not_implemented(format!(
            "outputs playback channels {channels:?} != [0, 1]（plan 输入 FL/FR 与设备 ch0/1 逐通道对应）"
        )));
    }
    if geometry.sample_rate != devices.sample_rate {
        return Err(StageFailure::failed(
            E_REMEASURE_PLAYBACK_CONFIG,
            format!(
                "plan.sample_rate={} != 设备 {} Hz（plan 与 host 配置不一致）",
                geometry.sample_rate, devices.sample_rate
            ),
        ));
    }
    if geometry.block_size != devices.block_frames {
        return Err(StageFailure::failed(
            E_REMEASURE_PLAYBACK_CONFIG,
            format!(
                "plan.block_size={} != 设备 period {}（plan 与 host block 不一致）",
                geometry.block_size, devices.block_frames
            ),
        ));
    }
    if geometry.workers != 2 && geometry.workers != 4 {
        return Err(StageFailure::failed(
            E_REMEASURE_PLAYBACK_CONFIG,
            format!("plan.workers={} 只允许 2/4", geometry.workers),
        ));
    }
    if require_device_width && geometry.n_out > devices.playback_channels {
        return Err(StageFailure::failed(
            E_REMEASURE_PLAYBACK_CONFIG,
            format!(
                "plan.n_out={} > 设备 playback_channels={}（host 会拒绝启动；需要至少 plan.n_out 宽的播放设备）",
                geometry.n_out, devices.playback_channels
            ),
        ));
    }
    Ok(())
}

/// 生成 remeasure 用 rt-host plan-arena 配置文本（file 输入 + plan + manifest）。
pub fn remeasure_host_config_text(
    plan_path: &Path,
    manifest_path: &Path,
    wav_path: &Path,
    devices: &StageDeviceConfig,
    geometry: &HostPlanGeometry,
    host: &RemeasureHostConfig,
) -> String {
    use std::fmt::Write as _;
    let toml_path = |path: &Path| path.to_string_lossy().replace('\\', "/");
    let mut text = String::with_capacity(1024);
    text.push_str("# tspi-cal remeasure playback: T61 timeline -> 2ch WAV -> plan-arena host (stereo 2.0 points)\n");
    text.push_str("[host]\n");
    let _ = writeln!(text, "block_size = {}", geometry.block_size);
    let _ = writeln!(text, "sample_rate = {}", devices.sample_rate);
    let _ = writeln!(text, "workers = {}", geometry.workers);
    text.push_str("graph = \"plan-arena\"\n");
    // 测量必须出声：safe_mode 强制 normal（mute 会把激励静音，测到的是静音/本底）。
    text.push_str("safe_mode = \"normal\"\n");
    text.push_str("[alsa]\n");
    let _ = writeln!(text, "backend = \"{}\"", host.backend);
    // file 输入与 loopback 互斥：必须是 synth 输入模式（设备节拍仍由 duplex 提供）。
    text.push_str("io_mode = \"synth\"\n");
    let _ = writeln!(text, "device = \"{}\"", devices.playback_device);
    let _ = writeln!(text, "capture_channels = {}", host.capture_channels);
    let _ = writeln!(text, "playback_channels = {}", devices.playback_channels);
    let _ = writeln!(text, "period_frames = {}", devices.block_frames);
    let _ = writeln!(text, "buffer_frames = {}", devices.buffer_frames);
    text.push_str("[web]\n");
    text.push_str("bind = \"127.0.0.1:1\"\n");
    text.push_str("[paths]\n");
    let _ = writeln!(text, "plan = \"{}\"", toml_path(plan_path));
    let _ = writeln!(text, "plan_manifest = \"{}\"", toml_path(manifest_path));
    text.push_str("[capture]\n");
    text.push_str("source = \"file\"\n");
    let _ = writeln!(text, "path = \"{}\"", toml_path(wav_path));
    text.push_str("format = \"wav\"\n");
    // eof=exit：文件最后一帧写完即优雅退出（不补静音拖长测量窗口）。
    text.push_str("eof = \"exit\"\n");
    text.push_str("[run]\n");
    // 0 = 不限 block 数，播到 file EOF 退出。
    text.push_str("max_blocks = 0\n");
    text
}

/// 渲染 host file 输入的 WAV：把 PlaybackSource 整段 block 写成 2ch S24_3LE WAV。
///
/// 不生成任何激励（激励来自既有 T61 timeline + T22 S24_3LE 转换）；只落 WAV 容器。
fn render_host_plan_input_wav(
    playback: &mut dyn PlaybackSource,
    spec: &HostPlanPlayback,
    sample_rate: u32,
) -> Result<Vec<u8>, StageError> {
    let channels = playback.channels();
    let block = playback.block_frames() as usize;
    let total_frames = playback.total_frames();
    if channels == 0 || block == 0 {
        return Err(StageError::new(
            E_PLAYBACK_FAILED,
            "host-plan: playback source has zero channels/block",
        ));
    }
    if channels != spec.input_channels {
        return Err(StageError::new(
            E_PLAYBACK_FAILED,
            format!(
                "host-plan: playback source {channels}ch != plan.n_in {}",
                spec.input_channels
            ),
        ));
    }
    let frame_bytes = channels as usize * 3;
    let mut planar = vec![0.0f32; channels as usize * block];
    let mut block_bytes: Vec<u8> = Vec::with_capacity(block * frame_bytes);
    let mut pcm: Vec<u8> = Vec::with_capacity(total_frames as usize * frame_bytes);
    let mut frames_written = 0u64;
    loop {
        block_bytes.clear();
        let Some(frames) = playback.next_block(&mut planar, &mut block_bytes)? else {
            break;
        };
        let frames = frames as usize;
        if frames == 0 || frames > block {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!("host-plan: playback block frames {frames} outside 1..={block}"),
            ));
        }
        let wanted = frames * frame_bytes;
        if block_bytes.len() < wanted {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!(
                    "host-plan: playback block bytes {} < {wanted}（T22 S24_3LE 转换不足）",
                    block_bytes.len()
                ),
            ));
        }
        pcm.extend_from_slice(&block_bytes[..wanted]);
        frames_written = frames_written.saturating_add(frames as u64);
    }
    if frames_written != total_frames {
        return Err(StageError::new(
            E_PLAYBACK_FAILED,
            format!(
                "host-plan: rendered {frames_written} frames != T61 timeline {total_frames} frames"
            ),
        ));
    }
    if pcm.len() > (u32::MAX as usize) - 64 {
        return Err(StageError::new(
            E_PLAYBACK_FAILED,
            format!("host-plan: WAV data {} bytes exceeds RIFF 32-bit size", pcm.len()),
        ));
    }
    Ok(encode_wav_pcm24_interleaved(&pcm, channels, sample_rate))
}

/// 44B 标准 RIFF/WAVE 头 + interleaved S24_3LE data（host file 输入要求）。
pub fn encode_wav_pcm24_interleaved(pcm: &[u8], channels: u16, sample_rate: u32) -> Vec<u8> {
    let data_bytes = pcm.len() as u32;
    let block_align = channels as u32 * 3;
    let byte_rate = sample_rate.saturating_mul(block_align);
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32.saturating_add(data_bytes)).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&(block_align as u16).to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// 从 host 报告 JSON 文本里取一个无符号整数字段（简单扫描；host 报告字段固定）。
fn json_uint_field(text: &[u8], key: &str) -> Option<u64> {
    let text = String::from_utf8_lossy(text);
    let needle = format!("\"{key}\"");
    let mut start = 0usize;
    while let Some(relative) = text[start..].find(&needle) {
        let after_key = start + relative + needle.len();
        start = after_key;
        let Some(colon) = text[after_key..].find(':') else {
            continue;
        };
        let rest = text[after_key + colon + 1..].trim_start();
        let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse::<u64>().ok();
        }
    }
    None
}

/// 二次测量播放证据 JSON（写进 remeasure/holdout summary；aplay 模式如实标注 plan=absent）。
pub fn host_plan_playback_evidence_json(
    playback: &MeasurementPlayback,
    engine_name: &str,
) -> String {
    let mut out = String::with_capacity(512);
    match playback {
        MeasurementPlayback::Aplay => {
            out.push_str("{\"mode\":\"aplay\",\"plan_in_playback_path\":false,");
            out.push_str("\"note\":\"sweep/remeasure playback is aplay direct; plan-arena not in this path\"}");
        }
        MeasurementPlayback::HostPlan(host) => {
            let host_executed = engine_name != "fake-synth";
            out.push_str("{\"mode\":\"host-plan\",\"plan_in_playback_path\":true,");
            out.push_str("\"host_executed\":");
            out.push_str(if host_executed { "true" } else { "false" });
            out.push_str(",\"engine\":");
            push_json_text(&mut out, engine_name);
            out.push_str(",\"host_bin\":");
            push_json_text(&mut out, &host.host_bin.to_string_lossy());
            out.push_str(",\"host_backend\":");
            push_json_text(&mut out, &host.backend);
            out.push_str(",\"plan_sha256\":");
            push_json_text(&mut out, &host.plan_sha256);
            out.push_str(",\"plan_graph_id\":");
            push_json_text(&mut out, &host.plan_graph_id);
            out.push_str(",\"plan_path\":");
            push_json_text(&mut out, &host.plan_path.to_string_lossy());
            out.push_str(",\"plan_manifest_path\":");
            push_json_text(&mut out, &host.plan_manifest_path.to_string_lossy());
            out.push_str(",\"plan_input_channels\":");
            out.push_str(&host.input_channels.to_string());
            out.push_str(",\"plan_input_wav\":");
            push_json_text(&mut out, &host.wav_path.to_string_lossy());
            out.push_str(",\"rt_host_config\":");
            push_json_text(&mut out, &host.config_path.to_string_lossy());
            out.push_str(",\"host_report\":");
            push_json_text(&mut out, &host.report_path.to_string_lossy());
            out.push_str(",\"stereo_2_0_single_point\":true,\"seven_ch_or_4_1_2\":\"NOT_IMPLEMENTED\"");
            if !host_executed {
                out.push_str(",\"note\":\"engine=fake-synth: measurement synthesized offline; host not spawned (simulated, not acoustic)\"");
            }
            out.push('}');
        }
    }
    out
}

/// 尾部 N 个字符（错误消息里给 host stderr 摘要；不整段刷屏）。
fn tail_chars(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    chars[chars.len() - limit..].iter().collect()
}

// ---------------------------------------------------------------------------
// 测量核心（T61 timeline + T62 采集/时基；可离线单测）
// ---------------------------------------------------------------------------

/// 声学 UMIK 采集 raw 的 job artifact 相对路径。
pub const ACOUSTIC_CAPTURE_RELATIVE_PATH: &str = "capture/umik.raw";
/// MOTU 电学环回采集 raw 的 job artifact 相对路径。
pub const LOOPBACK_CAPTURE_RELATIVE_PATH: &str = "capture/motu-reference.raw";

/// 一次测量会话的汇总（证据/质量门输入；不含原始样本）。
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementSummary {
    /// schema。
    pub schema: &'static str,
    /// 测量模式。
    pub mode: StageMeasurementMode,
    /// session id（与 timeline/捕获一致）。
    pub session_id: String,
    /// 采样率。
    pub sample_rate: u32,
    /// block 帧数。
    pub block_frames: u16,
    /// 播放通道数。
    pub playback_channels: u16,
    /// 采集读取通道数。
    pub capture_channels: u16,
    /// 实际驱动电平（dBFS）。
    pub drive_level_dbfs: f64,
    /// MOTU 主输出 trim（dB）。
    pub k_main_trim_db: f64,
    /// 采集峰值（dBFS）。
    pub capture_peak_dbfs: f64,
    /// 数字削波样本数。
    pub clipped_samples: u64,
    /// nonfinite 样本数。
    pub nonfinite_samples: u64,
    /// 采集帧数。
    pub capture_frames: u64,
    /// 采集时长（秒）。
    pub capture_seconds: f64,
    /// 时间轴总帧数（完整性门基准）。
    pub timeline_frames: u64,
    /// 前静音噪声底（dBFS；None=不可用）。
    pub noise_floor_dbfs: Option<f64>,
    /// 宽带 SNR（dB；None=不可估计）。
    pub snr_db: Option<f64>,
    /// SNR 估计 + 冻结门证据（None=不可估计；新增字段，旧证据仍可读）。
    pub snr: Option<SnrGateReport>,
    /// xrun/overrun 数。
    pub xruns: u64,
    /// 播放欠载数。
    pub underruns: u64,
    /// 估计 seed（capture sample 0 对应 render frame）。
    pub seed_render_frame: f64,
    /// onset 有效 slot 数。
    pub valid_slots: u32,
    /// slot 总数。
    pub total_slots: u32,
    /// 是否全部 slot onset 有效。
    pub all_onsets_valid: bool,
    /// 引擎名。
    pub engine: String,
    /// 会话墙钟秒数。
    pub wall_seconds: f64,
    /// 引擎状态说明。
    pub notes: String,
}

impl MeasurementSummary {
    /// 报告 JSON（确定性）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(2048);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_text(&mut out, self.schema);
        out.push_str(",\"mode\":");
        push_json_text(&mut out, self.mode.as_str());
        out.push_str(",\"session_id\":");
        push_json_text(&mut out, &self.session_id);
        out.push_str(&format!(",\"sample_rate\":{}", self.sample_rate));
        out.push_str(&format!(",\"block_frames\":{}", self.block_frames));
        out.push_str(&format!(
            ",\"playback_channels\":{}",
            self.playback_channels
        ));
        out.push_str(&format!(",\"capture_channels\":{}", self.capture_channels));
        out.push_str(",\"drive_level_dbfs\":");
        push_json_f64(&mut out, self.drive_level_dbfs);
        out.push_str(",\"k_main_trim_db\":");
        push_json_f64(&mut out, self.k_main_trim_db);
        out.push_str(",\"capture_peak_dbfs\":");
        push_json_f64(&mut out, self.capture_peak_dbfs);
        out.push_str(&format!(",\"clipped_samples\":{}", self.clipped_samples));
        out.push_str(&format!(
            ",\"nonfinite_samples\":{}",
            self.nonfinite_samples
        ));
        out.push_str(&format!(",\"capture_frames\":{}", self.capture_frames));
        out.push_str(",\"capture_seconds\":");
        push_json_f64(&mut out, self.capture_seconds);
        out.push_str(&format!(",\"timeline_frames\":{}", self.timeline_frames));
        out.push_str(",\"noise_floor_dbfs\":");
        push_json_opt_f64(&mut out, self.noise_floor_dbfs);
        out.push_str(",\"snr_db\":");
        push_json_opt_f64(&mut out, self.snr_db);
        out.push_str(",\"snr_estimator\":");
        match &self.snr {
            Some(report) => out.push_str(&report.to_json()),
            None => out.push_str("null"),
        }
        out.push_str(&format!(",\"xruns\":{}", self.xruns));
        out.push_str(&format!(",\"underruns\":{}", self.underruns));
        out.push_str(",\"seed_render_frame\":");
        push_json_f64(&mut out, self.seed_render_frame);
        out.push_str(&format!(",\"valid_slots\":{}", self.valid_slots));
        out.push_str(&format!(",\"total_slots\":{}", self.total_slots));
        out.push_str(&format!(",\"all_onsets_valid\":{}", self.all_onsets_valid));
        out.push_str(",\"engine\":");
        push_json_text(&mut out, &self.engine);
        out.push_str(",\"wall_seconds\":");
        push_json_f64(&mut out, self.wall_seconds);
        out.push_str(",\"notes\":");
        push_json_text(&mut out, &self.notes);
        out.push('}');
        out
    }
}

/// 一次测量的完整输出（raw + 报告 JSON）。
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementOutput {
    /// 汇总。
    pub summary: MeasurementSummary,
    /// T61 时间轴 JSON。
    pub timeline_json: String,
    /// T62 采集报告 JSON。
    pub capture_report_json: String,
    /// 采集 raw 字节。
    pub raw: Vec<u8>,
    /// 解码后的单通道采集样本（f32；RT60/CSD 反卷积输入；电学环回为空）。
    pub samples: Vec<f32>,
    /// T61 时间轴（RT60/CSD 反卷积输入）。
    pub timeline: Option<SweepTimeline>,
    /// raw 的 artifact 相对路径。
    pub raw_relative_path: String,
}

/// 质量门单项结果。
#[derive(Debug, Clone, PartialEq)]
pub struct StageGateResult {
    /// 门名。
    pub name: &'static str,
    /// 失败时映射的稳定错误码。
    pub code: &'static str,
    /// 是否通过。
    pub passed: bool,
    /// 细节。
    pub detail: String,
}

impl StageGateResult {
    /// JSON 对象。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(192);
        out.push('{');
        out.push_str("\"name\":");
        push_json_text(&mut out, self.name);
        out.push_str(",\"code\":");
        push_json_text(&mut out, self.code);
        out.push_str(&format!(",\"passed\":{}", self.passed));
        out.push_str(",\"detail\":");
        push_json_text(&mut out, &self.detail);
        out.push('}');
        out
    }
}

/// 汇总逐项质量门（不短路；供 quality 证据）。
pub fn protection_gates(
    summary: &MeasurementSummary,
    config: &StageBackendConfig,
) -> Vec<StageGateResult> {
    let protection = &config.protection;
    let mut gates = Vec::with_capacity(5);
    gates.push(StageGateResult {
        name: "xrun",
        code: E_CAPTURE_XRUN,
        passed: summary.xruns <= protection.max_xruns && summary.underruns <= protection.max_xruns,
        detail: format!(
            "xruns={} underruns={} (max {})",
            summary.xruns, summary.underruns, protection.max_xruns
        ),
    });
    gates.push(StageGateResult {
        name: "capture_peak",
        code: E_CAPTURE_CLIP,
        passed: summary.capture_peak_dbfs <= protection.peak_ceiling_dbfs
            && summary.clipped_samples == 0
            && summary.nonfinite_samples == 0,
        detail: format!(
            "peak={} dBFS clipped={} nonfinite={} (ceiling {} dBFS)",
            summary.capture_peak_dbfs,
            summary.clipped_samples,
            summary.nonfinite_samples,
            protection.peak_ceiling_dbfs
        ),
    });
    let min_frames = (summary.timeline_frames as f64 * protection.min_capture_fraction) as u64;
    gates.push(StageGateResult {
        name: "capture_complete",
        code: E_CAPTURE_INCOMPLETE,
        passed: summary.capture_frames >= min_frames,
        detail: format!(
            "capture_frames={} min={} (timeline {} x {})",
            summary.capture_frames,
            min_frames,
            summary.timeline_frames,
            protection.min_capture_fraction
        ),
    });
    gates.push(StageGateResult {
        name: "onsets",
        code: E_CAPTURE_ONSET_INVALID,
        passed: summary.total_slots > 0
            && summary.valid_slots > 0
            && (!protection.require_all_onsets || summary.all_onsets_valid),
        detail: format!(
            "valid_slots={}/{} all_valid={} require_all={}",
            summary.valid_slots,
            summary.total_slots,
            summary.all_onsets_valid,
            protection.require_all_onsets
        ),
    });
    let snr_detail = match &summary.snr {
        Some(report) => format!(
            "snr_db={:?} min={} dB mode={} estimator={} onset={}@{:.3}s noise_window={:.3}..{:.3}s \
             transient_rejected={} (T63 band-limited gate still required)",
            summary.snr_db,
            protection.min_snr_db,
            report.mode,
            report.schema,
            report.onset_source,
            report.onset_seconds,
            report.noise_window_seconds[0],
            report.noise_window_seconds[1],
            report.transient_rejected
        ),
        None => format!(
            "snr_db={:?} min={} dB mode={} (not estimable; T63 band-limited gate still required)",
            summary.snr_db, protection.min_snr_db, SNR_GATE_MODE_ROBUST_BROADBAND
        ),
    };
    gates.push(StageGateResult {
        name: "snr",
        code: E_SNR_BELOW_MIN,
        passed: summary
            .snr_db
            .map(|snr| snr >= protection.min_snr_db)
            .unwrap_or(false),
        detail: snr_detail,
    });
    gates
}

/// 逐项质量门 → 首个失败的 StageFailure（全过返回 Ok）。
pub fn evaluate_protection(
    summary: &MeasurementSummary,
    config: &StageBackendConfig,
) -> Result<(), StageFailure> {
    for gate in protection_gates(summary, config) {
        if !gate.passed {
            return Err(StageFailure::failed(gate.code, gate.detail));
        }
    }
    Ok(())
}

/// 质量门 JSON（数组）。
pub fn gates_json(gates: &[StageGateResult]) -> String {
    let mut out = String::with_capacity(512);
    out.push('[');
    for (index, gate) in gates.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&gate.to_json());
    }
    out.push(']');
    out
}

/// capture crate 错误 → StageFailure（稳定码 E_STAGE_CAPTURE_FINALIZE）。
fn capture_failure(error: CaptureError) -> StageFailure {
    StageFailure::failed(E_CAPTURE_FINALIZE, format!("T62 capture: {error:?}"))
}

/// T61 错误 → StageFailure（稳定码 E_STAGE_SWEEP_CONFIG；取消保留 cancelled 标志）。
fn sweep_failure(error: SweepError) -> StageFailure {
    if error.is_cancelled() {
        return StageFailure::cancelled(error.to_string());
    }
    let failure = error.stage_failure();
    StageFailure::failed(
        E_SWEEP_CONFIG,
        format!("{}: {}", failure.code, failure.message),
    )
}

/// 会话超时（ms）：时间轴时长 + 2×settle + 保护裕量。
fn session_timeout_ms(
    timeline_frames: u64,
    sample_rate: u32,
    devices: &StageDeviceConfig,
    protection: &StageProtectionConfig,
) -> u64 {
    let rate = sample_rate.max(1) as f64;
    let session_ms = (timeline_frames as f64 / rate * 1000.0).ceil() as u64;
    session_ms
        .saturating_add(devices.settle_ms.saturating_mul(2))
        .saturating_add(protection.timeout_slack_ms)
        .saturating_add(1_000)
}

/// 构造 T61 会话（配置严格校验；不触碰设备）。
fn build_sweep_session(
    config: &StageBackendConfig,
    session_id: &str,
    layout: &str,
    level_dbfs: f64,
    kind: SweepSessionKind,
) -> Result<SweepPlayer, StageFailure> {
    let devices = &config.devices;
    let sweep_cfg = &config.sweep;
    let stimulus = SweepStimulus::log_sweep(
        sweep_cfg.start_hz,
        sweep_cfg.end_hz,
        sweep_cfg.seconds,
        level_dbfs,
        sweep_cfg.fade_ms,
        sweep_cfg.repeats,
        sweep_cfg.repeat_gap_seconds,
    );
    let mut sweep = match kind {
        SweepSessionKind::Acoustic => SweepConfig::acoustic(session_id, config.outputs.clone()),
        SweepSessionKind::Loopback => SweepConfig::loopback(session_id, config.outputs.clone()),
    };
    sweep.epoch_id = config.epoch_id;
    sweep.graph_id = config.graph_id.clone();
    sweep.layout = layout.to_string();
    sweep.sample_rate = devices.sample_rate;
    sweep.block_frames = devices.block_frames;
    sweep.playback_channels = devices.playback_channels;
    sweep.n_in = devices.capture_channels;
    sweep.outputs = config.outputs.clone();
    sweep.stimulus = stimulus;
    sweep.k_main_trim_db = sweep_cfg.k_main_trim_db;
    sweep.trim_policy = crate::sweep::TrimPolicy::RecordOnly;
    sweep.pre_seconds = sweep_cfg.pre_seconds;
    sweep.post_seconds = sweep_cfg.post_seconds;
    sweep.protection = SweepProtection::new(config.protection.peak_ceiling_dbfs, true);
    sweep.epoch_start_mono_ns = 0;
    sweep.block_deadline_ns = 0;
    sweep.plan_version = 1;
    SweepPlayer::new(sweep).map_err(sweep_failure)
}

/// SNR 估计 + 冻结门值的证据（T3 remeasure 断崖修复后的口径）。
///
/// 修复前 compute_snr_db 的噪声窗是固定的 [0, seed+pre_frames)，apply-host 退出时的
/// 流切换 pop 落在里面 → SNR 15.99→0.98（g0-g1 snr-cliff.json）。现在：
/// - onset 用 `noise_est` 的能量门从采集里检出（seed+pre_frames 只作搜索锚点）；
/// - 噪声用分块 RMS 中位数 + 瞬态/最高分位剔除（post-sweep 尾段自然入选）；
/// - 门值 `gate_min_snr_db` 可配置，并**原样写进 summary 证据**（冻结口径）。
#[derive(Debug, Clone, PartialEq)]
pub struct SnrGateReport {
    /// 估计器 schema（tspi-robust-snr/v1）。
    pub schema: &'static str,
    /// 门实现口径（robust-broadband；T63 带限/相干门仍 NOT_WIRED）。
    pub mode: &'static str,
    /// 冻结的 SNR 门值（dB）。
    pub gate_min_snr_db: f64,
    /// 估计 SNR（dB）。
    pub snr_db: f64,
    /// 是否过门。
    pub passed: bool,
    /// onset 来源（detected / expected-fallback）。
    pub onset_source: &'static str,
    /// onset 时刻（秒，相对采集起点）。
    pub onset_seconds: f64,
    /// 分块帧数。
    pub block_frames: u64,
    /// 噪声证据窗（秒）。
    pub noise_window_seconds: [f64; 2],
    /// 稳健噪声 RMS（dBFS）。
    pub noise_rms_dbfs: f64,
    /// 采用噪声窗的分块数。
    pub noise_blocks_used: u32,
    /// 因信号被排除的分块数。
    pub noise_blocks_excluded_signal: u32,
    /// 因最高分位被剔除的分块数。
    pub noise_blocks_trimmed: u32,
    /// 信号窗（秒）。
    pub signal_window_seconds: [f64; 2],
    /// 信号 RMS（dBFS）。
    pub signal_rms_dbfs: f64,
    /// 采集开头最大块 RMS（dBFS；pop 证据）。
    pub pre_onset_max_dbfs: f64,
    /// 采集开头最大块 RMS 时刻（秒）。
    pub pre_onset_max_seconds: f64,
    /// 是否检出高于噪声的瞬态（pop 等）。
    pub transient_rejected: bool,
}

impl SnrGateReport {
    /// 从稳健估计 + 冻结门值构造。
    pub fn from_estimate(
        estimate: &RobustSnrEstimate,
        sample_rate_hz: u32,
        min_snr_db: f64,
    ) -> Self {
        let rate = sample_rate_hz.max(1) as f64;
        SnrGateReport {
            schema: estimate.schema,
            mode: SNR_GATE_MODE_ROBUST_BROADBAND,
            gate_min_snr_db: min_snr_db,
            snr_db: estimate.snr_db,
            passed: estimate.snr_db >= min_snr_db,
            onset_source: estimate.onset_source.as_str(),
            onset_seconds: estimate.onset_sample / rate,
            block_frames: estimate.block_frames as u64,
            noise_window_seconds: [
                estimate.noise_window_start as f64 / rate,
                estimate.noise_window_end as f64 / rate,
            ],
            noise_rms_dbfs: estimate.noise_rms_dbfs,
            noise_blocks_used: estimate.noise_blocks_used,
            noise_blocks_excluded_signal: estimate.noise_blocks_excluded_signal,
            noise_blocks_trimmed: estimate.noise_blocks_trimmed,
            signal_window_seconds: [
                estimate.signal_window_start as f64 / rate,
                estimate.signal_window_end as f64 / rate,
            ],
            signal_rms_dbfs: estimate.signal_rms_dbfs,
            pre_onset_max_dbfs: estimate.pre_onset_max_dbfs,
            pre_onset_max_seconds: estimate.pre_onset_max_seconds,
            transient_rejected: estimate.transient_rejected,
        }
    }

    /// 确定性 JSON（写进 MeasurementSummary 证据）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(640);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_text(&mut out, self.schema);
        out.push_str(",\"mode\":");
        push_json_text(&mut out, self.mode);
        out.push_str(",\"gate_min_snr_db\":");
        push_json_f64(&mut out, self.gate_min_snr_db);
        out.push_str(",\"snr_db\":");
        push_json_f64(&mut out, self.snr_db);
        out.push_str(",\"passed\":");
        out.push_str(if self.passed { "true" } else { "false" });
        out.push_str(",\"onset_source\":");
        push_json_text(&mut out, self.onset_source);
        out.push_str(",\"onset_seconds\":");
        push_json_f64(&mut out, self.onset_seconds);
        out.push_str(&format!(",\"block_frames\":{}", self.block_frames));
        out.push_str(",\"noise_window_seconds\":[");
        push_json_f64(&mut out, self.noise_window_seconds[0]);
        out.push(',');
        push_json_f64(&mut out, self.noise_window_seconds[1]);
        out.push(']');
        out.push_str(",\"noise_rms_dbfs\":");
        push_json_f64(&mut out, self.noise_rms_dbfs);
        out.push_str(&format!(
            ",\"noise_blocks_used\":{}",
            self.noise_blocks_used
        ));
        out.push_str(&format!(
            ",\"noise_blocks_excluded_signal\":{}",
            self.noise_blocks_excluded_signal
        ));
        out.push_str(&format!(
            ",\"noise_blocks_trimmed\":{}",
            self.noise_blocks_trimmed
        ));
        out.push_str(",\"signal_window_seconds\":[");
        push_json_f64(&mut out, self.signal_window_seconds[0]);
        out.push(',');
        push_json_f64(&mut out, self.signal_window_seconds[1]);
        out.push(']');
        out.push_str(",\"signal_rms_dbfs\":");
        push_json_f64(&mut out, self.signal_rms_dbfs);
        out.push_str(",\"pre_onset_max_dbfs\":");
        push_json_f64(&mut out, self.pre_onset_max_dbfs);
        out.push_str(",\"pre_onset_max_seconds\":");
        push_json_f64(&mut out, self.pre_onset_max_seconds);
        out.push_str(",\"transient_rejected\":");
        out.push_str(if self.transient_rejected {
            "true"
        } else {
            "false"
        });
        out.push('}');
        out
    }
}

/// 稳健 SNR 估计（含窗口/来源证据）。
///
/// seed_render_frame = render frame 0 对应的采集 sample 索引（采集先起 => 正值；
/// 顶层调用方传 `AudioCaptureResult::seed_render_frame`，不得再取负号）。
pub fn snr_estimate(
    samples: &[f32],
    grid: &RepeatGrid,
    seed_render_frame: f64,
) -> Option<RobustSnrEstimate> {
    let first_frame = grid.first_render_frame()? as f64;
    let expected_onset = seed_render_frame + first_frame;
    noise_est::estimate(
        samples,
        grid.sample_rate_hz,
        grid.sweep_frames,
        Some(expected_onset),
        &RobustSnrConfig::default(),
    )
}

/// 宽带 SNR（dB；稳健估计）。保留原签名供既有调用/测试使用。
pub fn compute_snr_db(samples: &[f32], grid: &RepeatGrid, seed_render_frame: f64) -> Option<f64> {
    snr_estimate(samples, grid, seed_render_frame).map(|estimate| estimate.snr_db)
}

fn base_audio_request(config: &StageBackendConfig, timeline_frames: u64) -> AudioSessionRequest {
    AudioSessionRequest {
        mode: config.mode,
        sample_rate: config.devices.sample_rate,
        block_frames: config.devices.block_frames as u32,
        playback_channels: config.devices.playback_channels,
        capture_channels: config.devices.capture_channels,
        capture_channel: config.devices.capture_channel,
        sample_format: config.devices.sample_format,
        playback_device: config.devices.playback_device.clone(),
        capture_device: config.devices.capture_device.clone(),
        settle_ms: config.devices.settle_ms,
        timeout_ms: session_timeout_ms(
            timeline_frames,
            config.devices.sample_rate,
            &config.devices,
            &config.protection,
        ),
        expected_capture_frames: timeline_frames,
        playback: SessionPlayback::Aplay,
    }
}

/// 从设备交错字节中抽取单通道 dense 字节（S24_3LE；声学 UMIK 多通道设备适配）。
///
/// 背景：UMIK-1 在部分主机只暴露 2ch 的 hw PCM（dsp-host card0 = 2ch S24_3LE FL/FR），
/// arecord 只能按设备通道宽度读；T62 UmikCapture 的输入契约是 1ch dense。
/// 本函数只做"按声明的 capture_channel 取一路"，不做混音/平均（不得引入不存在的信号）。
fn extract_channel_dense(
    interleaved: &[u8],
    channels: u16,
    channel: u16,
    bytes_per_sample: usize,
) -> Vec<u8> {
    let channels = channels as usize;
    let channel = channel as usize;
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let frame_bytes = channels * bytes_per_sample;
    if frame_bytes == 0 {
        return Vec::new();
    }
    let frames = interleaved.len() / frame_bytes;
    let mut out = Vec::with_capacity(frames * bytes_per_sample);
    for frame in 0..frames {
        let offset = (frame * channels + channel) * bytes_per_sample;
        out.extend_from_slice(&interleaved[offset..offset + bytes_per_sample]);
    }
    out
}

/// 声学 UMIK 测量（T61 timeline + T62 finalize）；返回 raw/报告 + 汇总，并执行保护门。
///
/// 默认播放路径 = aplay（既有一致行为）；需要 remeasure/holdout 经过 plan 时用
/// `run_acoustic_measurement_with_playback(..., MeasurementPlayback::HostPlan(..))`。
pub fn run_acoustic_measurement(
    config: &StageBackendConfig,
    engine: &mut dyn StageAudioEngine,
    session_id: &str,
    layout: &str,
    requested_level_dbfs: f64,
    cancel: &CancelToken,
    enforce_protection: bool,
) -> Result<MeasurementOutput, StageFailure> {
    run_acoustic_measurement_with_playback(
        config,
        engine,
        session_id,
        layout,
        requested_level_dbfs,
        cancel,
        enforce_protection,
        MeasurementPlayback::Aplay,
    )
}

/// 声学 UMIK 测量（显式播放方式；host-plan 时 T61 timeline 渲染为 2ch WAV 交给 plan-arena host）。
pub fn run_acoustic_measurement_with_playback(
    config: &StageBackendConfig,
    engine: &mut dyn StageAudioEngine,
    session_id: &str,
    layout: &str,
    requested_level_dbfs: f64,
    cancel: &CancelToken,
    enforce_protection: bool,
    playback: MeasurementPlayback,
) -> Result<MeasurementOutput, StageFailure> {
    config.validate()?;
    if config.mode != StageMeasurementMode::AcousticUmik {
        return Err(invalid(
            "mode",
            "run_acoustic_measurement requires acoustic mode",
        ));
    }
    // host-plan 只渲染 plan 输入（2ch）；aplay 用原设备播放宽度（既有行为不变）。
    let mut render_config = config.clone();
    let session_playback = match &playback {
        MeasurementPlayback::Aplay => SessionPlayback::Aplay,
        MeasurementPlayback::HostPlan(host) => {
            if layout.trim() != "stereo-2.0" {
                return Err(StageFailure::failed(
                    E_REMEASURE_PLAYBACK_CONFIG,
                    format!(
                        "remeasure-playback=host-plan 本轮只实现 stereo-2.0 单点；layout={layout:?} 是 NOT_IMPLEMENTED"
                    ),
                ));
            }
            if config.outputs.len() != 2
                || {
                    let mut channels: Vec<u32> =
                        config.outputs.iter().map(|output| output.channel).collect();
                    channels.sort_unstable();
                    channels != vec![0, 1]
                }
            {
                return Err(StageFailure::failed(
                    E_REMEASURE_PLAYBACK_CONFIG,
                    "remeasure-playback=host-plan 本轮只实现 stereo-2.0 单点（outputs 必须正好 FL@0/FR@1）；7ch/4.1.2 = NOT_IMPLEMENTED",
                ));
            }
            if host.input_channels != 2 {
                return Err(StageFailure::failed(
                    E_REMEASURE_PLAYBACK_CONFIG,
                    format!(
                        "host-plan input_channels={} 只支持 2（stereo 2.0 单点）",
                        host.input_channels
                    ),
                ));
            }
            render_config.devices.playback_channels = host.input_channels;
            SessionPlayback::HostPlan(host.clone())
        }
    };
    let level = config
        .level
        .resolve_for(requested_level_dbfs, StageMeasurementMode::AcousticUmik)?;
    let mic = config
        .mic
        .as_ref()
        .ok_or_else(|| StageFailure::failed(E_MIC_DECLARATION, "acoustic mode requires UMIK"))?
        .clone();
    let player = build_sweep_session(
        &render_config,
        session_id,
        layout,
        level,
        SweepSessionKind::Acoustic,
    )?;
    let timeline = player.timeline().clone();
    let grid = RepeatGrid::from_timeline(&timeline).map_err(capture_failure)?;
    let mut source = TimelinePlaybackSource::new(
        player,
        &render_config.devices,
        cancel.clone(),
        config.protection.peak_ceiling_dbfs,
    )?;
    let mut audio_request = base_audio_request(&render_config, timeline.total_frames);
    audio_request.playback = session_playback;
    let capture = engine
        .run_session(&audio_request, &mut source)
        .map_err(|error| error.stage_failure())?;
    if capture.capture_frames as f64 + capture.seed_render_frame < timeline.total_frames as f64 {
        return Err(StageFailure::failed(
            E_CAPTURE_INCOMPLETE,
            format!(
                "capture covers {} frames (seed {}), timeline {} (engine {}); aborting before                  finalize",
                capture.capture_frames, capture.seed_render_frame, timeline.total_frames,
                capture.notes
            ),
        ));
    }
    let mut umik_config = UmikCaptureConfig::new(session_id, mic.clone());
    umik_config.epoch_id = config.epoch_id;
    umik_config.graph_id = config.graph_id.clone();
    umik_config.epoch_start_mono_ns = 0;
    umik_config.chunk_frames = config.devices.block_frames as u32;
    umik_config.capture_start_render_frame = Some(capture.seed_render_frame);
    umik_config.expected_total_frames = None;
    let mut umik = UmikCapture::new(umik_config).map_err(capture_failure)?;
    // 设备读块通道数 >1（UMIK-1 2ch）时，按 capture_channel 抽 1ch dense 再进 T62。
    let pick_channel = config.devices.capture_channel.unwrap_or(0);
    let device_channels = config.devices.capture_channels;
    for block in &capture.blocks {
        let selected;
        let bytes: &[u8] = if device_channels > 1 {
            selected = extract_channel_dense(
                &block.bytes,
                device_channels,
                pick_channel,
                config.devices.bytes_per_sample(),
            );
            &selected
        } else {
            &block.bytes
        };
        umik.push_block(block.mono_ns, bytes)
            .map_err(capture_failure)?;
    }
    let stream = umik.finish().map_err(capture_failure)?;
    // RT60/CSD 后处理需要解码后的采集样本（反卷积输入）；finalize 会消费 stream。
    let capture_samples = stream.samples.clone();
    let snr = snr_estimate(&stream.samples, &grid, capture.seed_render_frame);
    let snr_db = snr.as_ref().map(|estimate| estimate.snr_db);
    let plan = ReferencePlan::acoustic(&mic, None).map_err(capture_failure)?;
    let report = match capture::finalize_acoustic_capture(
        stream,
        &mic,
        &grid,
        &plan,
        &OnsetConfig::default(),
        ACOUSTIC_CAPTURE_RELATIVE_PATH,
        None,
    ) {
        Ok(report) => report,
        Err(error) => {
            // 噪声压过扫频时 T62 会诚实报 NoOnsets；若 SNR 本身已低于门，SNR 门是更可行动的
            // 判据（且与旧行为一致），优先返回 E_STAGE_SNR_BELOW_MIN，并保留 onset 原因。
            // T3 实测（snr≈17.5 > 15 dB）时 NoOnsets 仍原样上抛，不会被掩盖。
            if matches!(error, CaptureError::NoOnsets { .. })
                && snr_db
                    .map(|snr| snr < config.protection.min_snr_db)
                    .unwrap_or(false)
            {
                return Err(StageFailure::failed(
                    E_SNR_BELOW_MIN,
                    format!(
                        "T62 capture: {error}; snr_db={snr_db:?} min={} dB (noise dominates onset detection)",
                        config.protection.min_snr_db
                    ),
                ));
            }
            return Err(capture_failure(error));
        }
    };
    let summary = MeasurementSummary {
        schema: STAGE_BACKEND_SCHEMA,
        mode: config.mode,
        session_id: session_id.to_string(),
        sample_rate: config.devices.sample_rate,
        block_frames: config.devices.block_frames,
        playback_channels: config.devices.playback_channels,
        capture_channels: config.devices.capture_channels,
        drive_level_dbfs: level,
        k_main_trim_db: config.sweep.k_main_trim_db,
        capture_peak_dbfs: report.stream.capture_peak_dbfs,
        clipped_samples: report.stream.clipped_samples,
        nonfinite_samples: report.stream.nonfinite_samples,
        capture_frames: report.stream.frames,
        capture_seconds: report.stream.seconds,
        timeline_frames: timeline.total_frames,
        noise_floor_dbfs: report.stream.noise_floor_dbfs,
        snr_db,
        snr: snr.as_ref().map(|estimate| {
            SnrGateReport::from_estimate(
                estimate,
                config.devices.sample_rate,
                config.protection.min_snr_db,
            )
        }),
        xruns: capture.xruns,
        underruns: capture.underruns,
        seed_render_frame: capture.seed_render_frame,
        valid_slots: report
            .timebase
            .slots
            .len()
            .saturating_sub(report.timebase.onsets_invalid_slots as usize)
            as u32,
        total_slots: report.timebase.slots.len() as u32,
        all_onsets_valid: report.timebase.all_slots_valid,
        engine: engine.name().to_string(),
        wall_seconds: capture.wall_seconds,
        notes: format!(
            "{} device_capture_channels={} pick_channel={}",
            capture.notes, device_channels, pick_channel
        ),
    };
    if enforce_protection {
        evaluate_protection(&summary, config)?;
    }
    Ok(MeasurementOutput {
        summary,
        timeline_json: timeline.to_json(),
        capture_report_json: report.to_json(),
        raw: capture.raw,
        samples: capture_samples,
        timeline: Some(timeline),
        raw_relative_path: ACOUSTIC_CAPTURE_RELATIVE_PATH.to_string(),
    })
}

/// MOTU 电学环回测量（参考麦 ch0 同源；T62 electrical finalize）。
pub fn run_loopback_measurement(
    config: &StageBackendConfig,
    engine: &mut dyn StageAudioEngine,
    session_id: &str,
    layout: &str,
    requested_level_dbfs: f64,
    cancel: &CancelToken,
    enforce_protection: bool,
) -> Result<MeasurementOutput, StageFailure> {
    config.validate()?;
    if config.mode != StageMeasurementMode::ElectricalLoopback {
        return Err(invalid(
            "mode",
            "run_loopback_measurement requires electrical_loopback mode",
        ));
    }
    let level = config.level.resolve_for(
        requested_level_dbfs,
        StageMeasurementMode::ElectricalLoopback,
    )?;
    let player = build_sweep_session(
        config,
        session_id,
        layout,
        level,
        SweepSessionKind::Loopback,
    )?;
    let timeline = player.timeline().clone();
    let grid = RepeatGrid::from_timeline(&timeline).map_err(capture_failure)?;
    let mut source = TimelinePlaybackSource::new(
        player,
        &config.devices,
        cancel.clone(),
        config.protection.peak_ceiling_dbfs,
    )?;
    let audio_request = base_audio_request(config, timeline.total_frames);
    let capture = engine
        .run_session(&audio_request, &mut source)
        .map_err(|error| error.stage_failure())?;
    if capture.capture_frames as f64 + capture.seed_render_frame < timeline.total_frames as f64 {
        return Err(StageFailure::failed(
            E_CAPTURE_INCOMPLETE,
            format!(
                "capture covers {} frames (seed {}), timeline {} (engine {}); aborting before                  finalize",
                capture.capture_frames, capture.seed_render_frame, timeline.total_frames,
                capture.notes
            ),
        ));
    }
    let capture_channel = config.devices.capture_channel.ok_or_else(|| {
        StageFailure::failed(
            E_CONFIG_INVALID,
            "electrical loopback requires capture_channel",
        )
    })?;
    let channel = ElectricalReferenceChannel::new(
        config.devices.capture_device.clone(),
        capture_channel,
        SampleFormat::S24_3LE,
    );
    let duplex_format = duplex_format_config(&config.devices)?;
    let mut reference_config = ElectricalReferenceConfig::new(channel.clone(), duplex_format)
        .map_err(|error| {
            StageFailure::failed(E_CONFIG_INVALID, format!("reference config: {error:?}"))
        })?;
    reference_config.session_id = session_id.to_string();
    reference_config.epoch_id = config.epoch_id;
    reference_config.graph_id = config.graph_id.clone();
    reference_config.epoch_start_mono_ns = 0;
    reference_config.capture_start_render_frame = Some(capture.seed_render_frame);
    reference_config.expected_total_frames = None;
    let mut reference =
        ElectricalReferenceCapture::new(reference_config).map_err(capture_failure)?;
    for block in &capture.blocks {
        reference
            .push_block(block.mono_ns, &block.bytes)
            .map_err(capture_failure)?;
    }
    let stream = reference.finish().map_err(capture_failure)?;
    let capture_samples = stream.samples.clone();
    let snr = snr_estimate(&stream.samples, &grid, capture.seed_render_frame);
    let snr_db = snr.as_ref().map(|estimate| estimate.snr_db);
    let plan = ReferencePlan::electrical_loopback(channel).map_err(capture_failure)?;
    let report = capture::finalize_electrical_loopback(
        stream,
        &grid,
        &plan,
        &OnsetConfig::default(),
        LOOPBACK_CAPTURE_RELATIVE_PATH,
    )
    .map_err(capture_failure)?;
    let summary = MeasurementSummary {
        schema: STAGE_BACKEND_SCHEMA,
        mode: config.mode,
        session_id: session_id.to_string(),
        sample_rate: config.devices.sample_rate,
        block_frames: config.devices.block_frames,
        playback_channels: config.devices.playback_channels,
        capture_channels: config.devices.capture_channels,
        drive_level_dbfs: level,
        k_main_trim_db: config.sweep.k_main_trim_db,
        capture_peak_dbfs: report.stream.capture_peak_dbfs,
        clipped_samples: report.stream.clipped_samples,
        nonfinite_samples: report.stream.nonfinite_samples,
        capture_frames: report.stream.frames,
        capture_seconds: report.stream.seconds,
        timeline_frames: timeline.total_frames,
        noise_floor_dbfs: report.stream.noise_floor_dbfs,
        snr_db,
        snr: snr.as_ref().map(|estimate| {
            SnrGateReport::from_estimate(
                estimate,
                config.devices.sample_rate,
                config.protection.min_snr_db,
            )
        }),
        xruns: capture.xruns,
        underruns: capture.underruns,
        seed_render_frame: capture.seed_render_frame,
        valid_slots: report
            .timebase
            .slots
            .len()
            .saturating_sub(report.timebase.onsets_invalid_slots as usize)
            as u32,
        total_slots: report.timebase.slots.len() as u32,
        all_onsets_valid: report.timebase.all_slots_valid,
        engine: engine.name().to_string(),
        wall_seconds: capture.wall_seconds,
        notes: capture.notes.clone(),
    };
    if enforce_protection {
        evaluate_protection(&summary, config)?;
    }
    Ok(MeasurementOutput {
        summary,
        timeline_json: timeline.to_json(),
        capture_report_json: report.to_json(),
        raw: capture.raw,
        samples: capture_samples,
        timeline: Some(timeline),
        raw_relative_path: LOOPBACK_CAPTURE_RELATIVE_PATH.to_string(),
    })
}

/// 构造 MOTU 双工 FormatConfig（T22；in=capture_channels，out=playback_channels）。
fn duplex_format_config(devices: &StageDeviceConfig) -> Result<FormatConfig, StageFailure> {
    let bytes = devices.bytes_per_sample() as u32;
    let period = devices.block_frames as u32;
    let hw = TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: sample_format::S24_3LE_INTERLEAVED,
        n_in: devices.capture_channels,
        n_out: devices.playback_channels,
        sample_rate: devices.sample_rate,
        period_frames: period,
        buffer_frames: devices.buffer_frames,
        in_stride_bytes: period * devices.capture_channels as u32 * bytes,
        out_stride_bytes: period * devices.playback_channels as u32 * bytes,
        flags: 0,
        reserved: [0; 4],
    };
    format::validate(&FormatConfig {
        hw,
        dither: DitherCfg::Off,
    })
    .map_err(|status| {
        StageFailure::failed(
            E_CONFIG_INVALID,
            format!("T22 duplex FormatConfig rejected: {status:?}"),
        )
    })?;
    Ok(FormatConfig {
        hw,
        dither: DitherCfg::Off,
    })
}

/// 按模式执行一次测量。
pub fn run_measurement(
    config: &StageBackendConfig,
    engine: &mut dyn StageAudioEngine,
    session_id: &str,
    layout: &str,
    requested_level_dbfs: f64,
    cancel: &CancelToken,
    enforce_protection: bool,
) -> Result<MeasurementOutput, StageFailure> {
    run_measurement_with_playback(
        config,
        engine,
        session_id,
        layout,
        requested_level_dbfs,
        cancel,
        enforce_protection,
        MeasurementPlayback::Aplay,
    )
}

/// 显式播放方式的测量入口（sweep_capture 恒 Aplay；二次测量可 HostPlan）。
pub fn run_measurement_with_playback(
    config: &StageBackendConfig,
    engine: &mut dyn StageAudioEngine,
    session_id: &str,
    layout: &str,
    requested_level_dbfs: f64,
    cancel: &CancelToken,
    enforce_protection: bool,
    playback: MeasurementPlayback,
) -> Result<MeasurementOutput, StageFailure> {
    match config.mode {
        StageMeasurementMode::AcousticUmik => run_acoustic_measurement_with_playback(
            config,
            engine,
            session_id,
            layout,
            requested_level_dbfs,
            cancel,
            enforce_protection,
            playback,
        ),
        StageMeasurementMode::ElectricalLoopback => {
            if !matches!(playback, MeasurementPlayback::Aplay) {
                return Err(StageFailure::failed(
                    E_REMEASURE_PLAYBACK_CONFIG,
                    "remeasure-playback=host-plan is NOT_IMPLEMENTED for electrical loopback                      (host and reference capture would contend for the same MOTU device)",
                ));
            }
            run_loopback_measurement(
                config,
                engine,
                session_id,
                layout,
                requested_level_dbfs,
                cancel,
                enforce_protection,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// 未接线 stage 的 delegate（如实 NOT_RUN；禁止伪造成功）
// ---------------------------------------------------------------------------

/// 未接线 stage 的委托接口（T63-T66 复合接线由后续任务注入；默认 NotWiredDelegate）。
pub trait StageDelegate: Send {
    /// 执行一个未在本 backend 内实现的 stage。
    fn run(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure>;
}

/// 默认 delegate：未接线 stage 如实失败（E_STAGE_NOT_WIRED），不写任何伪 artifact。
#[derive(Debug, Default, Clone, Copy)]
pub struct NotWiredDelegate;

impl StageDelegate for NotWiredDelegate {
    fn run(&mut self, stage: JobStage, _ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        Err(StageFailure::failed(
            E_STAGE_NOT_WIRED,
            format!(
                "stage {} is not wired in the real stage backend yet (fit/solve/compile/apply \
                 chain pending T63-T66 composition); refusing to fake success",
                stage.as_str()
            ),
        ))
    }
}

/// 单个 artifact 写入（路径冲突时用 .rN 后缀重试，保证 stage 可重入）。
fn write_artifact_lenient(
    ctx: &StageCtx<'_>,
    kind: ArtifactKind,
    path: &str,
    bytes: &[u8],
    version: &str,
) -> Result<(), StageFailure> {
    match ctx.write_artifact(kind.clone(), path, bytes, version) {
        Ok(_) => Ok(()),
        Err(error) if error.code == "E_ARTIFACT_IMMUTABLE" => {
            for attempt in 2..=8u32 {
                let candidate = retry_path(path, attempt);
                match ctx.write_artifact(kind.clone(), &candidate, bytes, version) {
                    Ok(_) => return Ok(()),
                    Err(retry_error) if retry_error.code == "E_ARTIFACT_IMMUTABLE" => continue,
                    Err(retry_error) => return Err(retry_error),
                }
            }
            Err(error)
        }
        Err(error) => Err(error),
    }
}

/// path -> path.rN（保留扩展名；无扩展名则直接追加）。
fn retry_path(path: &str, attempt: u32) -> String {
    match path.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => {
            format!("{stem}.r{attempt}.{extension}")
        }
        _ => format!("{path}.r{attempt}"),
    }
}

// ---------------------------------------------------------------------------
// StageBackend（T60 CalibrationJobBackend 真实测量实现）
// ---------------------------------------------------------------------------

/// 真实校准 stage backend：preflight/sweep_capture/quality_check/remeasure/holdout 真实测量，
/// 其余 stage 交给 delegate（默认如实 NOT_WIRED）。
pub struct StageBackend {
    config: StageBackendConfig,
    engine: Box<dyn StageAudioEngine>,
    delegate: Box<dyn StageDelegate>,
    /// RT60/CSD 后处理配置（默认开启；--no-rt60 关闭）。
    rt60: Rt60StageConfig,
    last_job: Option<u64>,
    last_summary: Option<MeasurementSummary>,
    last_timeline_json: Option<String>,
    last_capture_report_json: Option<String>,
    last_probe: Option<StageProbe>,
}

impl StageBackend {
    /// 构造（配置严格校验；不触碰设备）。
    pub fn new(
        config: StageBackendConfig,
        engine: Box<dyn StageAudioEngine>,
    ) -> Result<Self, StageFailure> {
        config.validate()?;
        Ok(StageBackend {
            config,
            engine,
            delegate: Box::new(NotWiredDelegate),
            rt60: Rt60StageConfig::default(),
            last_job: None,
            last_summary: None,
            last_timeline_json: None,
            last_capture_report_json: None,
            last_probe: None,
        })
    }

    /// 由配置构造（引擎按 EngineKind 选择）。
    pub fn from_config(config: StageBackendConfig) -> Result<Self, StageFailure> {
        let engine = config.build_engine();
        Self::new(config, engine)
    }

    /// 注入 delegate（T63-T66 接线用；默认 NotWiredDelegate）。
    pub fn with_delegate(mut self, delegate: Box<dyn StageDelegate>) -> Self {
        self.delegate = delegate;
        self
    }

    /// 注入 RT60/CSD 后处理配置（默认开启；CLI --no-rt60 -> Rt60StageConfig::disabled()）。
    pub fn with_rt60(mut self, config: Rt60StageConfig) -> Self {
        self.rt60 = config;
        self
    }

    /// 当前 RT60 配置（只读）。
    pub fn rt60_config(&self) -> &Rt60StageConfig {
        &self.rt60
    }

    /// 写 RT60/CSD artifacts（失败只写 warning，绝不阻断 stage）。
    fn write_rt60_artifacts(
        &self,
        ctx: &mut StageCtx<'_>,
        output: &MeasurementOutput,
        prefix: &str,
        stage: &str,
    ) {
        let files = build_rt60_artifacts(&self.rt60, output, prefix, stage);
        for file in files {
            if let Err(error) = write_artifact_lenient(
                ctx,
                ArtifactKind::Other(file.kind.to_string()),
                &file.path,
                &file.bytes,
                RT60_ARTIFACT_VERSION,
            ) {
                // artifact 写失败也不阻断主测量流程；stderr 如实告警（job 继续）。
                eprintln!(
                    "warning: rt60 artifact {} write failed: {} {}",
                    file.path, error.code, error.message
                );
            }
        }
    }

    /// 登记 host-plan 播放证据（plan-input.wav / rt-host.toml / host-report.json）。
    ///
    /// 只有真实执行 host 的引擎才会写出这些文件（FakeEngine 不启动 host → 跳过；summary
    /// 的 plan_playback.host_executed=false 如实标注 simulated）。host-report 每次内容不同，
    /// 走 write_artifact_lenient（重入时落 .rN）。
    fn register_host_plan_evidence(
        &self,
        ctx: &mut StageCtx<'_>,
        playback: &MeasurementPlayback,
    ) {
        let MeasurementPlayback::HostPlan(host) = playback else {
            return;
        };
        let artifacts_dir = ctx.artifacts_dir();
        for (kind, path) in [
            ("remeasure_host_plan_input", &host.wav_path),
            ("remeasure_host_rt_host_config", &host.config_path),
            ("remeasure_host_report", &host.report_path),
        ] {
            if !path.is_file() {
                continue;
            }
            let Some(relative) = path.strip_prefix(&artifacts_dir).ok().map(|relative| {
                relative
                    .to_string_lossy()
                    .replace('\\', "/")
            }) else {
                // 自定义 --remeasure-work-dir 在 artifacts 之外：不登记，但仍留在磁盘上。
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            if let Err(error) = write_artifact_lenient(
                ctx,
                ArtifactKind::Other(kind.to_string()),
                &relative,
                &bytes,
                STAGE_BACKEND_VERSION,
            ) {
                eprintln!(
                    "warning: remeasure host-plan evidence {} write failed: {} {}",
                    relative, error.code, error.message
                );
            }
        }
    }

    /// 配置。
    pub fn config(&self) -> &StageBackendConfig {
        &self.config
    }

    /// 最近一次 preflight 探针（证据）。
    pub fn last_probe(&self) -> Option<&StageProbe> {
        self.last_probe.as_ref()
    }

    /// 最近一次测量汇总（证据）。
    pub fn last_summary(&self) -> Option<&MeasurementSummary> {
        self.last_summary.as_ref()
    }

    /// 公共 preflight 校验（也供 CLI/测试直接调用）。
    pub fn check_request(&self, request: &JobRequest) -> Result<f64, StageFailure> {
        self.config.validate()?;
        request
            .validate()
            .map_err(|message| StageFailure::failed(E_CONFIG_INVALID, message))?;
        if !request.audible_sweep_authorized {
            return Err(StageFailure::failed(
                E_LEVEL_NOT_AUTHORIZED,
                "audible sweep must be explicitly authorized (P08)",
            ));
        }
        self.config.check_request_rate(request.sample_rate)?;
        self.config
            .level
            .resolve_for(request.drive_level_dbfs as f64, self.config.mode)
    }

    fn stage_label(&self, stage: JobStage, job_id: u64) -> String {
        format!(
            "{}-{}-j{:04}",
            self.config.session_label,
            stage.as_str(),
            job_id
        )
    }

    fn preflight_report_json(
        &self,
        request: &JobRequest,
        level: f64,
        probe: &StageProbe,
    ) -> String {
        let mut out = String::with_capacity(2048);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_text(&mut out, STAGE_BACKEND_SCHEMA);
        out.push_str(",\"stage\":\"preflight\"");
        out.push_str(",\"mode\":");
        push_json_text(&mut out, self.config.mode.as_str());
        out.push_str(",\"sample_rate\":");
        out.push_str(&request.sample_rate.to_string());
        out.push_str(",\"drive_level_dbfs\":");
        push_json_f64(&mut out, level);
        out.push_str(",\"hot_level_authorized\":");
        out.push_str(&self.config.level.hot_level_authorized.to_string());
        out.push_str(",\"peak_ceiling_dbfs\":");
        push_json_f64(&mut out, self.config.protection.peak_ceiling_dbfs);
        out.push_str(",\"k_main_trim_db\":");
        push_json_f64(&mut out, self.config.sweep.k_main_trim_db);
        out.push_str(",\"layout\":");
        push_json_text(&mut out, &request.layout);
        out.push_str(",\"job_label\":");
        push_json_text(&mut out, &request.label);
        out.push_str(",\"outputs\":[");
        for (index, output) in self.config.outputs.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            out.push_str(&format!("\"slot\":{},", output.slot));
            out.push_str("\"speaker\":");
            push_json_text(&mut out, &output.speaker);
            out.push_str(&format!(",\"channel\":{}", output.channel));
            out.push('}');
        }
        out.push(']');
        out.push_str(",\"devices\":{");
        out.push_str("\"playback_device\":");
        push_json_text(&mut out, &self.config.devices.playback_device);
        out.push_str(&format!(
            ",\"playback_channels\":{}",
            self.config.devices.playback_channels
        ));
        out.push_str(",\"capture_device\":");
        push_json_text(&mut out, &self.config.devices.capture_device);
        out.push_str(&format!(
            ",\"capture_channels\":{}",
            self.config.devices.capture_channels
        ));
        out.push_str(",\"capture_channel\":");
        match self.config.devices.capture_channel {
            Some(channel) => out.push_str(&channel.to_string()),
            None => out.push_str("null"),
        }
        out.push_str(",\"block_frames\":");
        out.push_str(&self.config.devices.block_frames.to_string());
        out.push_str(",\"buffer_frames\":");
        out.push_str(&self.config.devices.buffer_frames.to_string());
        out.push('}');
        out.push_str(",\"probe\":{");
        out.push_str(&format!("\"playback_ok\":{}", probe.playback_ok));
        out.push_str(&format!(",\"capture_ok\":{}", probe.capture_ok));
        out.push_str(",\"capture_sample_rates\":[");
        for (index, rate) in probe.capture_sample_rates.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&rate.to_string());
        }
        out.push(']');
        out.push_str(&format!(
            ",\"capture_max_channels\":{}",
            probe.capture_max_channels
        ));
        out.push_str(",\"notes\":");
        push_json_text(&mut out, &probe.notes);
        out.push('}');
        out.push_str(",\"status\":\"PASS\"");
        out.push('}');
        out
    }

    fn preflight(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.report_progress(0.1);
        let request = ctx.request().clone();
        let level = self.check_request(&request)?;
        let probe = self
            .engine
            .probe(&self.config.devices)
            .map_err(|error| error.stage_failure())?;
        if !probe.playback_ok || !probe.capture_ok {
            return Err(StageFailure::failed(
                E_DEVICE_UNAVAILABLE,
                format!(
                    "device probe failed (playback_ok={} capture_ok={}): {}",
                    probe.playback_ok, probe.capture_ok, probe.notes
                ),
            ));
        }
        if !probe.capture_sample_rates.is_empty()
            && !probe
                .capture_sample_rates
                .contains(&self.config.devices.sample_rate)
        {
            return Err(StageFailure::failed(
                E_DEVICE_UNAVAILABLE,
                format!(
                    "capture device does not list {} Hz (supported: {:?})",
                    self.config.devices.sample_rate, probe.capture_sample_rates
                ),
            ));
        }
        if probe.capture_max_channels > 0
            && probe.capture_max_channels < self.config.devices.capture_channels
        {
            return Err(StageFailure::failed(
                E_DEVICE_UNAVAILABLE,
                format!(
                    "capture device max channels {} < declared {}",
                    probe.capture_max_channels, self.config.devices.capture_channels
                ),
            ));
        }
        let report = self.preflight_report_json(&request, level, &probe);
        write_artifact_lenient(
            ctx,
            ArtifactKind::Other("preflight_report".to_string()),
            "preflight/report.json",
            report.as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        self.last_probe = Some(probe);
        ctx.report_progress(1.0);
        Ok(())
    }

    /// 一次测量（播放方式显式传入；sweep_capture 恒 Aplay，二次测量可 HostPlan）。
    fn measurement(
        &mut self,
        ctx: &mut StageCtx<'_>,
        playback: MeasurementPlayback,
    ) -> Result<MeasurementOutput, StageFailure> {
        let request = ctx.request().clone();
        let level = self.check_request(&request)?;
        let job_id = ctx.job_id().0;
        let session_id = self.stage_label(JobStage::SweepCapture, job_id);
        let cancel = ctx.cancel_token();
        ctx.report_progress(0.05);
        crate::stage_backend::run_measurement_with_playback(
            &self.config,
            &mut *self.engine,
            &session_id,
            &request.layout,
            level,
            &cancel,
            true,
            playback,
        )
    }

    fn sweep_capture(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        // 首次测量必须测未校正状态：sweep_capture 恒 aplay（host-plan 只给二次测量）。
        let output = self.measurement(ctx, MeasurementPlayback::Aplay)?;
        let job_id = ctx.job_id().0;
        ctx.report_progress(0.92);
        write_artifact_lenient(
            ctx,
            ArtifactKind::Other("sweep_timeline".to_string()),
            "sweep/timeline.json",
            output.timeline_json.as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        write_artifact_lenient(
            ctx,
            ArtifactKind::RawCapture,
            &output.raw_relative_path,
            &output.raw,
            STAGE_BACKEND_VERSION,
        )?;
        write_artifact_lenient(
            ctx,
            ArtifactKind::Other("capture_report".to_string()),
            "capture/report.json",
            output.capture_report_json.as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        write_artifact_lenient(
            ctx,
            ArtifactKind::Other("measurement_summary".to_string()),
            "capture/summary.json",
            output.summary.to_json().as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        // goal-6：测量阶段后处理 -> RT60/CSD（默认开启；失败只 warning，不阻断）。
        self.write_rt60_artifacts(ctx, &output, "", JobStage::SweepCapture.as_str());
        self.last_job = Some(job_id);
        self.last_summary = Some(output.summary);
        self.last_timeline_json = Some(output.timeline_json);
        self.last_capture_report_json = Some(output.capture_report_json);
        ctx.report_progress(1.0);
        Ok(())
    }

    fn quality_report_json(&self, summary: &MeasurementSummary) -> (String, bool) {
        let gates = protection_gates(summary, &self.config);
        let passed = gates.iter().all(|gate| gate.passed);
        let mut out = String::with_capacity(2048);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_text(&mut out, STAGE_BACKEND_SCHEMA);
        out.push_str(",\"stage\":\"quality_check\"");
        out.push_str(",\"status\":");
        push_json_text(&mut out, if passed { "PASS" } else { "FAIL" });
        out.push_str(",\"summary\":");
        out.push_str(&summary.to_json());
        out.push_str(",\"gates\":");
        out.push_str(&gates_json(&gates));
        out.push_str(
            ",\"notes\":\"broadband SNR gate is a conservative approximation; T63 \
                      band-limited SNR/coherence gates still required for T6 PASS\"",
        );
        out.push('}');
        (out, passed)
    }

    fn quality_check(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        let job_id = ctx.job_id().0;
        let summary = match (&self.last_job, &self.last_summary) {
            (Some(last_job), Some(summary)) if *last_job == job_id => summary.clone(),
            _ => {
                return Err(StageFailure::failed(
                    E_STAGE_RESUME_PENDING,
                    "quality_check requires the sweep_capture measurement summary in memory; \
                     after a process restart rerun the job from sweep_capture (checkpoint resume \
                     re-enters the in-progress stage, not a completed one)",
                ))
            }
        };
        ctx.report_progress(0.3);
        let (report, passed) = self.quality_report_json(&summary);
        write_artifact_lenient(
            ctx,
            ArtifactKind::QualityReport,
            "quality/report.json",
            report.as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        ctx.report_progress(0.9);
        if !passed {
            evaluate_protection(&summary, &self.config)?;
        }
        ctx.report_progress(1.0);
        Ok(())
    }

    /// 解析二次测量的播放方式（默认 aplay；host-plan 时从 apply/compile 产物解析 plan）。
    fn remeasure_playback(
        &self,
        ctx: &mut StageCtx<'_>,
        stage: JobStage,
    ) -> Result<MeasurementPlayback, StageFailure> {
        if self.config.remeasure_playback != RemeasurePlayback::HostPlan {
            return Ok(MeasurementPlayback::Aplay);
        }
        let resolved = resolve_remeasure_plan(
            &ctx.artifacts_dir(),
            &self.config.remeasure_host,
            ctx.applied_version().as_deref(),
        )?;
        validate_remeasure_host_plan(
            &resolved.geometry,
            &ctx.request().layout,
            &self.config.outputs,
            &self.config.devices,
            self.config.engine != EngineKind::Fake,
        )?;
        let work_dir = self
            .config
            .remeasure_host
            .work_dir
            .clone()
            .unwrap_or_else(|| {
                ctx.artifacts_dir()
                    .join(format!("{}-host-plan", stage.as_str()))
            });
        std::fs::create_dir_all(&work_dir).map_err(|error| {
            StageFailure::failed(
                E_REMEASURE_PLAYBACK_CONFIG,
                format!(
                    "create remeasure host-plan work dir {}: {error}",
                    work_dir.display()
                ),
            )
        })?;
        let wav_path = work_dir.join("plan-input.wav");
        let config_path = work_dir.join(crate::host_apply::HOST_CONFIG_FILE);
        let report_path = work_dir.join(crate::host_apply::HOST_REPORT_FILE);
        let config_text = remeasure_host_config_text(
            &resolved.plan_path,
            &resolved.manifest_path,
            &wav_path,
            &self.config.devices,
            &resolved.geometry,
            &self.config.remeasure_host,
        );
        Ok(MeasurementPlayback::HostPlan(HostPlanPlayback {
            host_bin: self.config.remeasure_host.host_bin.clone(),
            config_path,
            config_text,
            wav_path,
            report_path,
            input_channels: resolved.geometry.n_in,
            backend: self.config.remeasure_host.backend.clone(),
            timeout_ms: 0,
            plan_sha256: resolved.plan_sha256,
            plan_graph_id: resolved.geometry.graph_id_hex(),
            plan_path: resolved.plan_path,
            plan_manifest_path: resolved.manifest_path,
        }))
    }

    fn secondary_measure(
        &mut self,
        ctx: &mut StageCtx<'_>,
        stage: JobStage,
    ) -> Result<(), StageFailure> {
        // apply-host 退出（MOTU USB 播放流关闭/altset 切换）→ remeasure 采集之间必须留
        // settle：g0-g1 snr-cliff.json 证明流切换 pop（t≈6 ms、−31.57 dBFS）会污染噪声窗。
        // 只对真实设备引擎生效；FakeEngine（离线合成，无流切换）不等待，保持单测快速。
        if stage == JobStage::Remeasure
            && self.config.apply_settle_ms > 0
            && self.config.engine != EngineKind::Fake
        {
            thread::sleep(Duration::from_millis(self.config.apply_settle_ms));
        }
        ctx.cancel_token().check()?;
        // 播放方式在测量前解析：plan 产物缺失/几何不符必须 fail-closed（不回退 aplay）。
        let playback = self.remeasure_playback(ctx, stage)?;
        let output = self.measurement(ctx, playback.clone())?;
        let job_id = ctx.job_id().0;
        let (kind, directory) = match stage {
            JobStage::Remeasure => (ArtifactKind::RemeasureReport, "remeasure"),
            JobStage::IndependentHoldout => (ArtifactKind::HoldoutReport, "holdout"),
            other => {
                return Err(StageFailure::failed(
                    E_STAGE_NOT_WIRED,
                    format!("secondary_measure does not handle stage {}", other.as_str()),
                ))
            }
        };
        // host-plan 播放证据：wav/toml/host-report 由 ToolsEngine 真实写出时登记 artifact。
        self.register_host_plan_evidence(ctx, &playback);
        let summary_path = format!("{directory}/summary.json");
        let report_path = format!("{directory}/capture-report.json");
        let raw_path = format!("{directory}/capture.raw");
        let mut report = String::with_capacity(2048);
        report.push('{');
        report.push_str("\"schema\":");
        push_json_text(&mut report, STAGE_BACKEND_SCHEMA);
        report.push_str(",\"stage\":");
        push_json_text(&mut report, stage.as_str());
        report.push_str(",\"summary\":");
        report.push_str(&output.summary.to_json());
        report.push_str(",\"capture_report\":");
        report.push_str(&output.capture_report_json);
        report.push_str(",\"plan_playback\":");
        report.push_str(&host_plan_playback_evidence_json(&playback, self.engine.name()));
        report.push('}');
        write_artifact_lenient(
            ctx,
            kind,
            &summary_path,
            report.as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        write_artifact_lenient(
            ctx,
            ArtifactKind::Other("capture_report".to_string()),
            &report_path,
            output.capture_report_json.as_bytes(),
            STAGE_BACKEND_VERSION,
        )?;
        write_artifact_lenient(
            ctx,
            ArtifactKind::RawCapture,
            &raw_path,
            &output.raw,
            STAGE_BACKEND_VERSION,
        )?;
        // goal-6：复测/holdout 同样记录 RT60/CSD（artifact 前缀区分）。
        self.write_rt60_artifacts(ctx, &output, directory, stage.as_str());
        self.last_job = Some(job_id);
        self.last_summary = Some(output.summary);
        Ok(())
    }
}

impl CalibrationJobBackend for StageBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::Preflight => self.preflight(ctx),
            JobStage::SweepCapture => self.sweep_capture(ctx),
            JobStage::QualityCheck => self.quality_check(ctx),
            JobStage::Remeasure => self.secondary_measure(ctx, JobStage::Remeasure),
            JobStage::IndependentHoldout => {
                self.secondary_measure(ctx, JobStage::IndependentHoldout)
            }
            other => self.delegate.run(other, ctx),
        }
    }
}

// ---------------------------------------------------------------------------
// 板端 smoke 一步式入口（CLI 用；写全套证据文件）
// ---------------------------------------------------------------------------

/// 板端 smoke 证据包。
#[derive(Debug, Clone, PartialEq)]
pub struct BoardMeasurementEvidence {
    /// 保护门是否全过。
    pub passed: bool,
    /// 测量汇总。
    pub summary: MeasurementSummary,
    /// 逐项保护门。
    pub gates: Vec<StageGateResult>,
    /// 引擎名。
    pub engine: String,
    /// 是否离线合成（simulated；不得当真实板端证据）。
    pub simulated: bool,
    /// 写出文件相对路径（含 sha256）。
    pub files: Vec<(String, u64, String)>,
    /// 证据根目录。
    pub out_dir: String,
}

/// 板端 smoke：preflight 探针 + 一次真实测量 + 保护门 + 证据文件（raw/报告/manifest）。
///
/// 失败时仍尽力写出已产生证据；返回 Err = 保护/设备失败（调用方按 exit code 处理）。
pub fn run_board_measurement(
    config: &StageBackendConfig,
    out_dir: &std::path::Path,
) -> Result<BoardMeasurementEvidence, StageFailure> {
    let mut engine = config.build_engine();
    run_board_measurement_with_engine(config, &mut *engine, out_dir)
}

/// 板端 smoke（注入引擎版；测试/干跑用 fake 引擎时证据必须标 simulated）。
pub fn run_board_measurement_with_engine(
    config: &StageBackendConfig,
    engine: &mut dyn StageAudioEngine,
    out_dir: &std::path::Path,
) -> Result<BoardMeasurementEvidence, StageFailure> {
    use std::fs;
    config.validate()?;
    fs::create_dir_all(out_dir).map_err(|error| {
        StageFailure::failed(
            E_CONFIG_INVALID,
            format!("create out dir {}: {error}", out_dir.display()),
        )
    })?;
    let probe = engine
        .probe(&config.devices)
        .map_err(|error| error.stage_failure())?;
    if !probe.playback_ok || !probe.capture_ok {
        return Err(StageFailure::failed(
            E_DEVICE_UNAVAILABLE,
            format!("device probe failed: {}", probe.notes),
        ));
    }
    let level = config.level.default_level();
    // 干跑/短会话默认：板端 smoke 用 smoke 参数；正式 T2/T4 由 CLI --seconds 等覆盖。
    let cancel = CancelToken::new();
    let session_id = format!("{}-board", config.session_label);
    let output = run_measurement(config, engine, &session_id, "4.1.2", level, &cancel, false)?;
    let gates = protection_gates(&output.summary, config);
    let passed = gates.iter().all(|gate| gate.passed);
    let simulated = engine.name() == "fake-synth";
    let mut files: Vec<(String, u64, String)> = Vec::new();
    let mut write = |relative: &str, bytes: &[u8]| -> Result<(), StageFailure> {
        let path = out_dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                StageFailure::failed(
                    E_CONFIG_INVALID,
                    format!("create dir {}: {error}", parent.display()),
                )
            })?;
        }
        fs::write(&path, bytes).map_err(|error| {
            StageFailure::failed(
                E_CONFIG_INVALID,
                format!("write {}: {error}", path.display()),
            )
        })?;
        files.push((relative.to_string(), bytes.len() as u64, sha256_hex(bytes)));
        Ok(())
    };

    let mut preflight = String::with_capacity(1024);
    preflight.push('{');
    preflight.push_str("\"schema\":");
    push_json_text(&mut preflight, STAGE_BACKEND_SCHEMA);
    preflight.push_str(",\"stage\":\"preflight\"");
    preflight.push_str(",\"mode\":");
    push_json_text(&mut preflight, config.mode.as_str());
    preflight.push_str(",\"engine\":");
    push_json_text(&mut preflight, engine.name());
    preflight.push_str(",\"simulated\":");
    preflight.push_str(&simulated.to_string());
    preflight.push_str(",\"probe\":{");
    preflight.push_str(&format!("\"playback_ok\":{}", probe.playback_ok));
    preflight.push_str(&format!(",\"capture_ok\":{}", probe.capture_ok));
    preflight.push_str(",\"capture_sample_rates\":[");
    for (index, rate) in probe.capture_sample_rates.iter().enumerate() {
        if index > 0 {
            preflight.push(',');
        }
        preflight.push_str(&rate.to_string());
    }
    preflight.push(']');
    preflight.push_str(",\"notes\":");
    push_json_text(&mut preflight, &probe.notes);
    preflight.push('}');
    preflight.push_str(",\"status\":\"PASS\"");
    preflight.push('}');
    write("preflight/report.json", preflight.as_bytes())?;
    write("sweep/timeline.json", output.timeline_json.as_bytes())?;
    let raw_name = output
        .raw_relative_path
        .rsplit('/')
        .next()
        .unwrap_or("capture.raw");
    write(&format!("capture/{raw_name}"), &output.raw)?;
    write("capture/report.json", output.capture_report_json.as_bytes())?;
    write("capture/summary.json", output.summary.to_json().as_bytes())?;

    let mut quality = String::with_capacity(1024);
    quality.push('{');
    quality.push_str("\"schema\":");
    push_json_text(&mut quality, STAGE_BACKEND_SCHEMA);
    quality.push_str(",\"stage\":\"quality_check\"");
    quality.push_str(",\"status\":");
    push_json_text(&mut quality, if passed { "PASS" } else { "FAIL" });
    quality.push_str(",\"gates\":");
    quality.push_str(&gates_json(&gates));
    quality.push_str(
        ",\"notes\":\"broadband SNR gate is a conservative approximation; T63 \
                     band-limited SNR/coherence gates still required for T6 PASS\"",
    );
    quality.push('}');
    write("quality/report.json", quality.as_bytes())?;

    let mut manifest = String::with_capacity(4096);
    manifest.push('{');
    manifest.push_str("\"schema\":");
    push_json_text(&mut manifest, STAGE_BACKEND_SCHEMA);
    manifest.push_str(",\"kind\":\"board_measurement_evidence\"");
    manifest.push_str(&format!(",\"generated_unix\":{}", unix_now()));
    manifest.push_str(",\"engine\":");
    push_json_text(&mut manifest, engine.name());
    manifest.push_str(",\"simulated\":");
    manifest.push_str(&simulated.to_string());
    manifest.push_str(",\"board_smoke\":");
    push_json_text(
        &mut manifest,
        if simulated {
            "NOT_RUN (simulated run; real MOTU/UMIK board smoke pending board window)"
        } else {
            "EXECUTED_ON_THIS_HOST"
        },
    );
    manifest.push_str(",\"status\":");
    push_json_text(&mut manifest, if passed { "PASS" } else { "FAIL" });
    manifest.push_str(",\"summary\":");
    manifest.push_str(&output.summary.to_json());
    manifest.push_str(",\"gates\":");
    manifest.push_str(&gates_json(&gates));
    manifest.push_str(",\"files\":[");
    for (index, (relative, bytes, sha256)) in files.iter().enumerate() {
        if index > 0 {
            manifest.push(',');
        }
        manifest.push('{');
        manifest.push_str("\"path\":");
        push_json_text(&mut manifest, relative);
        manifest.push_str(&format!(",\"bytes\":{bytes}"));
        manifest.push_str(",\"sha256\":");
        push_json_text(&mut manifest, sha256);
        manifest.push('}');
    }
    manifest.push(']');
    manifest.push('}');
    fs::write(out_dir.join("manifest.json"), manifest.as_bytes()).map_err(|error| {
        StageFailure::failed(E_CONFIG_INVALID, format!("write manifest: {error}"))
    })?;

    if !passed {
        evaluate_protection(&output.summary, config)?;
    }
    Ok(BoardMeasurementEvidence {
        passed,
        summary: output.summary,
        gates,
        engine: engine.name().to_string(),
        simulated,
        files,
        out_dir: out_dir.to_string_lossy().to_string(),
    })
}

// ---------------------------------------------------------------------------
// 测试注册（leaf 私有测试自注册；integrator 不要再写 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod t67_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/T67/stage_backend.rs"
    ));
}

#[cfg(test)]
mod t75_rt60_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/T75/rt60_integration.rs"
    ));
}

#[cfg(test)]
mod t76_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/T76/host_plan_remeasure.rs"
    ));
}

