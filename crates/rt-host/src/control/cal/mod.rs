//! rt-host 进程内校准模式（T70f-c）：cal_status / cal_enter / cal_leave /
//! cal_measure / cal_remeasure / cal_solve。
//!
//! 设计来源：旧 rt-host 的 cal_control.rs + cal_umik.rs（legacy/pre-rewrite，
//! rt-stack/crates/rt-host/src/）。旧实现的 op 集合与安全语义保留；音频线程钩子
//! （mimo12k / overlay_live / apply_live）不复用，改为当前 Runtime 的校准播放 seam
//! （crates/host/src/runtime.rs：begin_calibration_playback / run_calibration_block）。
//!
//! # 边界（如实）
//!
//! - 校准是 rt-host 的一种模式，不新开 socket、不换设备 owner、不抢 PCM：播放仍由
//!   块线程写它已打开的 MOTU；UMIK 采集在控制面线程（非实时）用 ALSA capture 读。
//! - 测量链路复用 khaen-calibration 的 T61 时间轴 / T62 采集时基 / T67 stage 后端，
//!   不在 rt-host 里重写扫频、反卷积、时基、质量门数学。
//! - 设备 absent / 静音 / 安全故障时测量请求返回明确错误（device_absent /
//!   audio_blocked），**不做假测量**；离线合成引擎（fake）的测量结果必须标
//!   simulated，设计结果标 PREDICTED_ONLY。
//! - 求解只产出 design JSON + 建议 plan（写 artifact），**不提交**、不改正在跑的输出。
//! - 旧 emperor 12k 专有逻辑不复用；单点求解只接当前已有能力
//!   （ir_extract 反卷积 + peq_fit 目标 FIR + fir_plan 编译）。
//!
//! # 旧常量出处（音高/时长/电平/预延迟）
//!
//! 全部来自 legacy/pre-rewrite rt-stack/crates/rt-host/src/cal_control.rs：
//! SWEEP_RATE_HZ=48000、SWEEP_SECONDS=5.0、SWEEP_LEVEL=0.25118864（= −12 dBFS）、
//! 20 Hz..20 kHz、pre 1.5 s；cal_umik.rs：UMIK 48 kHz / S24_3LE / period 128 /
//! buffer 512 / 采集 2ch 取第 1 路。重复遍数 3 与 gap 0.5 s 取 T61/T62 声学口径
//! （时钟 ppm 需要多遍；单遍无法估计 drift）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use khaen_calibration::capture::UmikDeviceConfig;
use khaen_calibration::ir_extract::{extract_irs, DeconvolvedIr, IrExtractConfig};
use khaen_calibration::job::{CancelToken, StageFailure};
use khaen_calibration::stage_backend::{
    encode_wav_pcm24_interleaved, run_acoustic_measurement, MeasurementOutput, StageAudioEngine,
    StageBackendConfig, StageDeviceConfig, StageError, StageLevelConfig, StageMeasurementMode,
    StageProtectionConfig, StageSweepConfig, DEFAULT_APPLY_SETTLE_MS, STAGE_UMIK_SAMPLE_RATE_HZ,
};
use khaen_calibration::sweep::PLACEHOLDER_GRAPH_ID;
use khaen_host::control::{error as host_error, ControlError};
use khaen_host::engine::EngineConfig;
use khaen_host::plan::Plan;
use serde_json::{json, Map, Value};

use crate::control::reconfigure::{ReconfigGate, RECONFIGURING_LABEL};
use crate::control::snapshot::{RuntimeSnapshot, SharedSnapshot};

use engine::{CalEngineFactory, CalProgress};

pub mod engine;
pub mod solve;
#[cfg(test)]
mod tests;

// ===========================================================================
// 常量 / 错误码
// ===========================================================================

/// 本模块接通的 op（status.wired 用；全部经 ControlCore 分发）。
pub const CAL_OPS: [&str; 6] = [
    "cal_status",
    "cal_enter",
    "cal_leave",
    "cal_measure",
    "cal_remeasure",
    "cal_solve",
];

/// 校准开关环境变量：off/0/false 关闭，其它（不设）= 打开。
pub const CAL_ENV: &str = "KHAEN_CAL";
/// 校准产物目录（默认 /var/lib/khaen/cal）。
pub const CAL_DIR_ENV: &str = "KHAEN_CAL_DIR";
/// UMIK 采集设备（ALSA 名；默认 hw:0,0 = 旧 cal_umik.rs 的板端设备）。
pub const CAL_UMIK_DEVICE_ENV: &str = "KHAEN_UMIK_DEVICE";
/// UMIK 序列号（声明必填；缺失 → mic_declaration_missing，不猜）。
pub const CAL_UMIK_SERIAL_ENV: &str = "KHAEN_UMIK_SERIAL";
/// UMIK 校准文件名（声明必填）。
pub const CAL_UMIK_CAL_FILE_ENV: &str = "KHAEN_UMIK_CAL_FILE";
/// UMIK 校准文件 sha256（声明必填；64 lowercase hex）。
pub const CAL_UMIK_CAL_SHA_ENV: &str = "KHAEN_UMIK_CAL_SHA256";
/// UMIK 朝向（默认 90deg = 多声道听音位口径）。
pub const CAL_UMIK_ORIENTATION_ENV: &str = "KHAEN_UMIK_ORIENTATION";
/// 采集设备读块通道数（UMIK-1 在部分主机只暴露 2ch，默认 2；1ch 设备设 1）。
pub const CAL_CAPTURE_CHANNELS_ENV: &str = "KHAEN_CAL_CAPTURE_CHANNELS";
/// 播放通道列表（逗号分隔 DAC 通道号；默认 = 旧 cal_control 的 4.1.2 物理列表 0,1,2,3,4,5,7）。
pub const CAL_CHANNELS_ENV: &str = "KHAEN_CAL_CHANNELS";
/// layout id（默认 4.1.2，与旧 status 一致；新栈 T57 布局解析器未接线，通道列表是显式声明）。
pub const CAL_LAYOUT_ENV: &str = "KHAEN_CAL_LAYOUT";
/// 驱动电平（dBFS；默认 −12 = 旧 SWEEP_LEVEL 0.25118864）。
pub const CAL_LEVEL_ENV: &str = "KHAEN_CAL_LEVEL_DBFS";
/// 引擎选择（auto|fake；默认 auto。非 Linux 的 auto 降级 fake 并标 simulated）。
pub const CAL_ENGINE_ENV: &str = "KHAEN_CAL_ENGINE";

/// 默认产物目录。
pub const DEFAULT_CAL_DIR: &str = "/var/lib/khaen/cal";
/// 默认 UMIK 设备（旧 cal_umik.rs：hw:0,0）。
pub const DEFAULT_UMIK_DEVICE: &str = "hw:0,0";
/// 默认朝向（多声道听音位）。
pub const DEFAULT_UMIK_ORIENTATION: &str = "90deg";
/// 默认 layout。
pub const DEFAULT_LAYOUT: &str = "4.1.2";
/// 默认播放通道（旧 cal_control status: channels [0,1,2,3,4,5,7]）。
pub const DEFAULT_CHANNELS: [u32; 7] = [0, 1, 2, 3, 4, 5, 7];
/// 默认驱动电平（−12 dBFS；旧 SWEEP_LEVEL）。
pub const DEFAULT_LEVEL_DBFS: f64 = -12.0;
/// 默认采集读块通道数（旧 cal_umik 按 2ch 打开）。
pub const DEFAULT_CAPTURE_CHANNELS: u16 = 2;
/// 采集读块选中的通道（2ch 时取第 1 路；T62 契约仍是 1ch dense）。
pub const DEFAULT_CAPTURE_CHANNEL: u16 = 0;
/// 默认 ALSA buffer 帧数（旧 cal_umik：512）。
pub const DEFAULT_BUFFER_FRAMES: u32 = 512;
/// 默认 settle（ms；T62/T67 板端口径 1200）。
pub const DEFAULT_SETTLE_MS: u64 = 1_200;

/// 旧扫频常量（cal_control.rs）：起始频率。
pub const LEGACY_SWEEP_START_HZ: f64 = 20.0;
/// 旧扫频常量：结束频率。
pub const LEGACY_SWEEP_END_HZ: f64 = 20_000.0;
/// 旧扫频常量：单遍时长（s）。
pub const LEGACY_SWEEP_SECONDS: f64 = 5.0;
/// 旧扫频常量：驱动电平（dBFS；旧 SWEEP_LEVEL=0.25118864）。
pub const LEGACY_SWEEP_LEVEL_DBFS: f64 = -12.0;
/// 旧扫频常量：fade（ms）。
pub const LEGACY_SWEEP_FADE_MS: f64 = 10.0;
/// 旧扫频常量：前静音（s；旧 cal_control 预延迟）。
pub const LEGACY_SWEEP_PRE_SECONDS: f64 = 1.5;
/// 旧扫频常量：后静音（s）。
pub const LEGACY_SWEEP_POST_SECONDS: f64 = 1.5;
/// 重复遍数（T61/T62 声学口径 3 遍：时钟 ppm 需要多遍）。
pub const SWEEP_REPEATS: u32 = 3;
/// 遍间静音（s；T61/T62 声学口径）。
pub const SWEEP_REPEAT_GAP_SECONDS: f64 = 0.5;

/// 稳定错误码（ControlError.code 必须是 &'static str）。
pub mod error {
    /// 当前不在校准模式。
    pub const NOT_IN_CALIBRATION: &str = "not_in_calibration";
    /// 可闻授权缺失（audible=false）。
    pub const AUDIBLE_AUTH_REQUIRED: &str = "audible_authorization_required";
    /// 输出设备缺失。
    pub const DEVICE_ABSENT: &str = "device_absent";
    /// 输出被静音/安全状态要求静音。
    pub const AUDIO_BLOCKED: &str = "audio_blocked";
    /// 块线程还没发布快照/未启动。
    pub const RUNTIME_NOT_RUNNING: &str = "runtime_not_running";
    /// UMIK 打不开/不可用。
    pub const UMIK_OPEN_FAILED: &str = "umik_open_failed";
    /// UMIK 声明缺失（serial/cal_file/cal_sha256）。
    pub const MIC_DECLARATION_MISSING: &str = "mic_declaration_missing";
    /// 运行时格式不支持校准（UMIK 48k 与块长 64/128）。
    pub const FORMAT_UNSUPPORTED: &str = "cal_format_unsupported";
    /// 校准配置非法（通道列表/电平/设备参数）。
    pub const CONFIG_INVALID: &str = "cal_config_invalid";
    /// 已有测量在跑（并发 measure 拒绝）。
    pub const BUSY: &str = "cal_busy";
    /// 没有可用测量结果（solve/remeasure 前必须先 measure）。
    pub const NO_MEASUREMENT: &str = "no_measurement";
    /// 测量失败（异步完成后对 wait 的完成响应；status 里有精确 stage_code）。
    pub const MEASURE_FAILED: &str = "measure_failed";
    /// 求解失败。
    pub const SOLVE_FAILED: &str = "solve_failed";
    /// artifact 写盘失败。
    pub const ARTIFACT_IO: &str = "artifact_io";
}

// ===========================================================================
// 引擎种类 / 配置
// ===========================================================================

/// 引擎选择（环境变量 KHAEN_CAL_ENGINE）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalEngineKind {
    /// 自动：Linux = 真机进程内引擎；其它平台 = fake（标 simulated）。
    Auto,
    /// 离线合成（测试/干跑；标 simulated）。
    Fake,
    /// 真机进程内引擎（Linux ALSA 采集 + 块线程校准播放）；非 Linux 平台没有真实采集路径。
    InProcess,
}

impl CalEngineKind {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            CalEngineKind::Auto => "auto",
            CalEngineKind::Fake => "fake",
            CalEngineKind::InProcess => "in-process",
        }
    }

    /// 解析（未知 = None，不静默降级）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(CalEngineKind::Auto),
            "fake" => Some(CalEngineKind::Fake),
            "in-process" | "inprocess" => Some(CalEngineKind::InProcess),
            _ => None,
        }
    }
}

/// 校准配置（环境变量解析；平台无关，便于测试）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationConfig {
    /// 产物目录。
    pub artifact_dir: PathBuf,
    /// UMIK 采集设备（ALSA 名）。
    pub umik_device: String,
    /// 采集读块通道数。
    pub capture_channels: u16,
    /// 采集读块选中的通道（capture_channels=1 时必须 None）。
    pub capture_channel: Option<u16>,
    /// UMIK 声明（缺失时 None + 原因；不猜）。
    pub mic: Option<UmikDeviceConfig>,
    /// 声明缺失原因（status 如实上报）。
    pub mic_missing_reason: Option<String>,
    /// layout id。
    pub layout: String,
    /// 播放通道列表（DAC 通道号）。
    pub channels: Vec<u32>,
    /// 驱动电平（dBFS）。
    pub level_dbfs: f64,
    /// 引擎种类。
    pub engine: CalEngineKind,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        CalibrationConfig {
            artifact_dir: PathBuf::from(DEFAULT_CAL_DIR),
            umik_device: DEFAULT_UMIK_DEVICE.to_string(),
            capture_channels: DEFAULT_CAPTURE_CHANNELS,
            capture_channel: Some(DEFAULT_CAPTURE_CHANNEL),
            mic: None,
            mic_missing_reason: Some(
                "UMIK declaration missing: set KHAEN_UMIK_SERIAL, KHAEN_UMIK_CAL_FILE and KHAEN_UMIK_CAL_SHA256 (legacy cal_umik.rs opened hw:0,0 but the T62 contract requires an explicit mic declaration)"
                    .to_string(),
            ),
            layout: DEFAULT_LAYOUT.to_string(),
            channels: DEFAULT_CHANNELS.to_vec(),
            level_dbfs: DEFAULT_LEVEL_DBFS,
            engine: CalEngineKind::Auto,
        }
    }
}

/// 配置错误（启动时解析失败；宿主按配置错误处理，不静默回退）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalConfigError {
    message: String,
}

impl CalConfigError {
    fn new(message: impl Into<String>) -> Self {
        CalConfigError {
            message: message.into(),
        }
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for CalConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CalConfigError {}

impl CalibrationConfig {
    /// 从环境变量读取；KHAEN_CAL=off/0/false 返回 None（校准 op 全部 not_wired）。
    pub fn from_env() -> Result<Option<Self>, CalConfigError> {
        Self::parse(
            std::env::var(CAL_ENV).ok(),
            std::env::var(CAL_DIR_ENV).ok(),
            std::env::var(CAL_UMIK_DEVICE_ENV).ok(),
            std::env::var(CAL_UMIK_SERIAL_ENV).ok(),
            std::env::var(CAL_UMIK_CAL_FILE_ENV).ok(),
            std::env::var(CAL_UMIK_CAL_SHA_ENV).ok(),
            std::env::var(CAL_UMIK_ORIENTATION_ENV).ok(),
            std::env::var(CAL_CAPTURE_CHANNELS_ENV).ok(),
            std::env::var(CAL_CHANNELS_ENV).ok(),
            std::env::var(CAL_LAYOUT_ENV).ok(),
            std::env::var(CAL_LEVEL_ENV).ok(),
            std::env::var(CAL_ENGINE_ENV).ok(),
        )
    }

    /// 纯解析（测试不读环境；参数就是对应环境变量的原始值）。
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        enabled: Option<String>,
        dir: Option<String>,
        umik_device: Option<String>,
        umik_serial: Option<String>,
        cal_file: Option<String>,
        cal_sha256: Option<String>,
        orientation: Option<String>,
        capture_channels: Option<String>,
        channels: Option<String>,
        layout: Option<String>,
        level: Option<String>,
        engine: Option<String>,
    ) -> Result<Option<Self>, CalConfigError> {
        let enabled = match enabled.as_deref().map(str::trim) {
            None | Some("") => true,
            Some(value) if value.eq_ignore_ascii_case("on") => true,
            Some(value) if value.eq_ignore_ascii_case("true") => true,
            Some(value) if value.eq_ignore_ascii_case("off") => false,
            Some(value) if value.eq_ignore_ascii_case("false") => false,
            Some("1") => true,
            Some("0") => false,
            Some(other) => {
                return Err(CalConfigError::new(format!(
                    "{CAL_ENV} must be on/off/1/0/true/false, got {other:?}"
                )))
            }
        };
        if !enabled {
            return Ok(None);
        }
        let mut config = CalibrationConfig::default();
        if let Some(dir) = dir.as_deref().map(str::trim) {
            if !dir.is_empty() {
                config.artifact_dir = PathBuf::from(dir);
            }
        }
        if let Some(device) = umik_device.as_deref().map(str::trim) {
            if device.is_empty() {
                return Err(CalConfigError::new(format!(
                    "{CAL_UMIK_DEVICE_ENV} must not be empty"
                )));
            }
            config.umik_device = device.to_string();
        }
        let orientation = match orientation.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_UMIK_ORIENTATION.to_string(),
            Some(value) => value.to_string(),
        };
        let capture_channels = match capture_channels.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_CAPTURE_CHANNELS,
            Some(value) => value.parse::<u16>().map_err(|error| {
                CalConfigError::new(format!(
                    "{CAL_CAPTURE_CHANNELS_ENV} must be an integer, got {value:?}: {error}"
                ))
            })?,
        };
        if capture_channels != 1 && capture_channels != 2 {
            return Err(CalConfigError::new(format!(
                "{CAL_CAPTURE_CHANNELS_ENV} must be 1 or 2 (UMIK read width), got {capture_channels}"
            )));
        }
        config.capture_channels = capture_channels;
        config.capture_channel = if capture_channels == 1 {
            None
        } else {
            Some(DEFAULT_CAPTURE_CHANNEL)
        };
        if let Some(channels) = channels.as_deref().map(str::trim) {
            if !channels.is_empty() {
                let mut parsed = Vec::new();
                for token in channels.split(',') {
                    let token = token.trim();
                    let channel = token.parse::<u32>().map_err(|error| {
                        CalConfigError::new(format!(
                            "{CAL_CHANNELS_ENV} must be comma-separated integers, got {token:?}: {error}"
                        ))
                    })?;
                    parsed.push(channel);
                }
                config.channels = parsed;
            }
        }
        if let Some(layout) = layout.as_deref().map(str::trim) {
            if !layout.is_empty() {
                config.layout = layout.to_string();
            }
        }
        if let Some(level) = level.as_deref().map(str::trim) {
            if !level.is_empty() {
                config.level_dbfs = level.parse::<f64>().map_err(|error| {
                    CalConfigError::new(format!(
                        "{CAL_LEVEL_ENV} must be a number, got {level:?}: {error}"
                    ))
                })?;
            }
        }
        if let Some(engine) = engine.as_deref().map(str::trim) {
            if !engine.is_empty() {
                config.engine = CalEngineKind::parse(engine).ok_or_else(|| {
                    CalConfigError::new(format!(
                        "{CAL_ENGINE_ENV} must be auto|fake, got {engine:?}"
                    ))
                })?;
            }
        }
        // UMIK 声明：四项缺一不可；有文件路径时用文件字节核对 sha256（不一致 = 拒绝声明）。
        let serial = umik_serial.as_deref().map(str::trim).unwrap_or("");
        let cal_file = cal_file.as_deref().map(str::trim).unwrap_or("");
        let cal_sha = cal_sha256.as_deref().map(str::trim).unwrap_or("");
        let mut missing = Vec::new();
        if serial.is_empty() {
            missing.push(CAL_UMIK_SERIAL_ENV);
        }
        if cal_file.is_empty() {
            missing.push(CAL_UMIK_CAL_FILE_ENV);
        }
        if cal_sha.is_empty() {
            missing.push(CAL_UMIK_CAL_SHA_ENV);
        }
        if missing.is_empty() {
            let declared = UmikDeviceConfig::umik1(
                config.umik_device.clone(),
                serial,
                cal_file,
                cal_sha,
                orientation,
            );
            if declared.validate().is_err() {
                config.mic_missing_reason = Some(format!(
                    "UMIK declaration rejected by T62: {}",
                    declared.validate().expect_err("validated as invalid")
                ));
            } else if let Ok(bytes) = fs::read(cal_file) {
                let actual = khaen_host::sha256_hex(&bytes);
                if actual != cal_sha {
                    config.mic_missing_reason = Some(format!(
                        "UMIK cal file sha256 mismatch: {CAL_UMIK_CAL_SHA_ENV} declares {cal_sha}, file {cal_file} hashes to {actual}"
                    ));
                } else {
                    config.mic = Some(declared);
                    config.mic_missing_reason = None;
                }
            } else {
                // 校准文件不在本机：只做声明（T62 不读该文件；sha256 由部署方保证）。
                config.mic = Some(declared);
                config.mic_missing_reason = None;
            }
        } else {
            config.mic_missing_reason = Some(format!(
                "UMIK declaration missing: set {} (T62 acoustic capture requires model/serial/cal_file/cal_sha256; legacy cal_umik.rs opened hw:0,0 without a declaration)",
                missing.join(", ")
            ));
        }
        Ok(Some(config))
    }
}

// ===========================================================================
// 状态机类型
// ===========================================================================

/// 当前模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalMode {
    /// 播放模式（不采集、不占 UMIK）。
    Playback,
    /// 校准模式（UMIK 已探测可用；测量会话内采集）。
    Calibration,
}

impl CalMode {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            CalMode::Playback => "playback",
            CalMode::Calibration => "calibration",
        }
    }
}

/// 测量状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalState {
    /// 没有测量。
    Idle,
    /// 测量进行中。
    Measuring,
    /// 最近一次测量成功。
    Done,
    /// 最近一次测量失败。
    Failed,
}

impl CalState {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            CalState::Idle => "idle",
            CalState::Measuring => "measuring",
            CalState::Done => "done",
            CalState::Failed => "failed",
        }
    }
}

/// 测量失败信息（稳定 code + 精确 stage code + 人读 message）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalFailure {
    /// 本模块稳定错误码（measure_failed / ir_extract_failed / CANCELLED ...）。
    pub code: String,
    /// 校准 crate stage 的精确错误码（E_STAGE_*；没有则 None）。
    pub stage_code: Option<String>,
    /// 人读说明。
    pub message: String,
    /// 是否协作式取消。
    pub cancelled: bool,
}

impl CalFailure {
    fn from_stage(err: &StageFailure) -> Self {
        CalFailure {
            code: err.code.clone(),
            stage_code: Some(err.code.clone()),
            message: err.message.clone(),
            cancelled: err.cancelled,
        }
    }

    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        CalFailure {
            code: code.into(),
            stage_code: None,
            message: message.into(),
            cancelled: false,
        }
    }

    /// JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "stage_code": self.stage_code,
            "message": self.message,
            "cancelled": self.cancelled,
        })
    }
}

/// 一次测量的紧凑摘要（delta 对比 / status 用）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalSummary {
    /// 测量 id。
    pub measure_id: u64,
    /// pass 序号。
    pub pass: u32,
    /// session id。
    pub session_id: String,
    /// 测量模式名。
    pub mode: String,
    /// 引擎名。
    pub engine: String,
    /// 是否离线合成。
    pub simulated: bool,
    /// 采样率。
    pub sample_rate: u32,
    /// 驱动电平（dBFS）。
    pub drive_level_dbfs: f64,
    /// 采集峰值（dBFS）。
    pub capture_peak_dbfs: f64,
    /// 宽带 SNR（dB；None = 不可估计）。
    pub snr_db: Option<f64>,
    /// 采集帧数。
    pub capture_frames: u64,
    /// 时间轴帧数。
    pub timeline_frames: u64,
    /// 有效 onset 槽位数。
    pub valid_slots: u32,
    /// 槽位总数。
    pub total_slots: u32,
    /// UMIK 时钟 ppm（None = 不可估计）。
    pub clock_ppm: Option<f64>,
    /// 时钟 ppm 不确定度（2 sigma）。
    pub clock_ppm_uncertainty_ppm: Option<f64>,
    /// 时钟偏差是否显著。
    pub drift_significant: Option<bool>,
    /// 时钟 ppm 是否在门内。
    pub clock_ppm_ok: Option<bool>,
    /// xrun/overrun 计数（引擎如实报告）。
    pub xruns: u64,
    /// 削波样本数。
    pub clipped_samples: u64,
}

impl CalSummary {
    /// JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "measure_id": self.measure_id,
            "pass": self.pass,
            "session_id": self.session_id,
            "mode": self.mode,
            "engine": self.engine,
            "simulated": self.simulated,
            "sample_rate": self.sample_rate,
            "drive_level_dbfs": json_f64(self.drive_level_dbfs),
            "capture_peak_dbfs": json_f64(self.capture_peak_dbfs),
            "snr_db": json_opt_f64(self.snr_db),
            "capture_frames": self.capture_frames,
            "timeline_frames": self.timeline_frames,
            "valid_slots": self.valid_slots,
            "total_slots": self.total_slots,
            "clock_ppm": json_opt_f64(self.clock_ppm),
            "clock_ppm_uncertainty_ppm": json_opt_f64(self.clock_ppm_uncertainty_ppm),
            "drift_significant": self.drift_significant,
            "clock_ppm_ok": self.clock_ppm_ok,
            "xruns": self.xruns,
            "clipped_samples": self.clipped_samples,
        })
    }
}

/// 最近一次成功测量的完整数据（solve 只用内存里的这份；不重读文件）。
#[derive(Debug, Clone)]
pub struct LastMeasurement {
    /// 摘要（多通道聚合：最差通道口径）。
    pub summary: CalSummary,
    /// 布局 id。
    pub layout: String,
    /// 通道列表。
    pub channels: Vec<u32>,
    /// 反卷积 IR（逐通道；solve 只用这份内存数据，不重读文件）。
    pub irs: Vec<DeconvolvedIr>,
    /// 本会话 artifact 目录。
    pub session_dir: PathBuf,
}

/// 校准模式会话参数（enter 时确定；measure 沿用，避免几何漂移）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionOptions {
    /// layout id。
    pub layout: String,
    /// 播放通道列表。
    pub channels: Vec<u32>,
    /// 驱动电平（dBFS）。
    pub level_dbfs: f64,
}

/// 测量工作线程的输入（一次测量一个）。
struct WorkerArgs {
    config: CalibrationConfig,
    stage: StageBackendConfig,
    options: SessionOptions,
    measure_id: u64,
    pass: u32,
    session_id: String,
    engine: Box<dyn StageAudioEngine>,
    progress: Arc<CalProgress>,
    cancel: CancelToken,
    shared: Arc<Mutex<CalShared>>,
    engine_slot: Arc<Mutex<Option<Box<dyn StageAudioEngine>>>>,
    previous: Option<CalSummary>,
    /// 引擎是否离线合成（simulated；结果/设计必须标）。
    simulated: bool,
}

/// 控制面线程与测量线程共享的校准状态。
struct CalShared {
    state: CalState,
    measure_id: u64,
    pass: u32,
    session_id: String,
    started: Option<Instant>,
    failure: Option<CalFailure>,
    result: Option<Value>,
    summary: Option<CalSummary>,
    last: Option<LastMeasurement>,
    last_solve: Option<Value>,
    /// 最近一次设计摘要（status 用）。
    last_solve_summary: Option<Value>,
    simulated: bool,
    engine: String,
    progress: Arc<CalProgress>,
}

impl CalShared {
    fn new(progress: Arc<CalProgress>) -> Self {
        CalShared {
            state: CalState::Idle,
            measure_id: 0,
            pass: 0,
            session_id: String::new(),
            started: None,
            failure: None,
            result: None,
            summary: None,
            last: None,
            last_solve: None,
            last_solve_summary: None,
            simulated: false,
            engine: String::new(),
            progress,
        }
    }
}

/// 一次测量工作线程句柄。
struct MeasureJob {
    cancel: CancelToken,
    handle: JoinHandle<()>,
}

// ===========================================================================
// 构造参数（ControlCore 注入）
// ===========================================================================

/// 校准装配：配置 + 引擎工厂（由 main/test 决定，测试注入 fake）。
pub struct CalibrationSetup {
    /// 配置。
    pub config: CalibrationConfig,
    /// 引擎工厂。
    pub factory: Box<dyn CalEngineFactory>,
}

impl std::fmt::Debug for CalibrationSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalibrationSetup")
            .field("config", &self.config)
            .field("engine", &self.factory.engine_name())
            .finish_non_exhaustive()
    }
}

// ===========================================================================
// 校准核心（协议状态机；不碰 socket、不碰 Runtime）
// ===========================================================================

/// 进程内校准核心：模式状态机 + 测量线程 + 产物 + 求解。
pub struct CalibrationCore {
    config: CalibrationConfig,
    factory: Box<dyn CalEngineFactory>,
    runtime_format: EngineConfig,
    snapshot: SharedSnapshot,
    mode: CalMode,
    options: Option<SessionOptions>,
    progress: Arc<CalProgress>,
    engine_slot: Arc<Mutex<Option<Box<dyn StageAudioEngine>>>>,
    shared: Arc<Mutex<CalShared>>,
    job: Option<MeasureJob>,
    next_measure_id: u64,
    next_pass: u32,
    /// 重配/校准互斥门（热重配进行中时 cal_enter -> reconfiguring）。
    gate: Arc<ReconfigGate>,
}

impl std::fmt::Debug for CalibrationCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalibrationCore")
            .field("mode", &self.mode)
            .field("options", &self.options)
            .field("next_measure_id", &self.next_measure_id)
            .field("next_pass", &self.next_pass)
            .field("engine", &self.factory.engine_name())
            .field("simulated", &self.factory.simulated())
            .finish_non_exhaustive()
    }
}

impl CalibrationCore {
    /// 新建（ControlCore 构造时调用）。
    pub fn new(
        setup: CalibrationSetup,
        runtime_format: EngineConfig,
        snapshot: SharedSnapshot,
        gate: Arc<ReconfigGate>,
    ) -> Self {
        let progress = Arc::new(CalProgress::new());
        let shared = Arc::new(Mutex::new(CalShared::new(Arc::clone(&progress))));
        CalibrationCore {
            config: setup.config,
            factory: setup.factory,
            runtime_format,
            snapshot,
            mode: CalMode::Playback,
            options: None,
            progress,
            engine_slot: Arc::new(Mutex::new(None)),
            shared,
            job: None,
            next_measure_id: 0,
            next_pass: 0,
            gate,
        }
    }

    /// 当前模式。
    pub fn mode(&self) -> CalMode {
        self.mode
    }

    /// 进程退出/线程收尾：取消测量并等线程退出，丢掉引擎。
    pub fn shutdown(&mut self) {
        self.cancel_and_join();
        let mut slot = lock(&self.engine_slot);
        let _ = slot.take();
        self.mode = CalMode::Playback;
    }

    // ------------------------------------------------------------------
    // 状态
    // ------------------------------------------------------------------

    fn snapshot(&self) -> Option<RuntimeSnapshot> {
        lock(&self.snapshot).clone()
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, CalShared> {
        lock(&self.shared)
    }

    /// status JSON（cal_status 的响应；也接进全局 status 的 calibration 摘要）。
    pub fn status_json(&self) -> Value {
        let (
            state,
            measure_id,
            pass,
            session_id,
            elapsed_ms,
            failure,
            result,
            summary,
            simulated,
            engine,
            progress,
        ) = {
            let shared = self.shared();
            (
                shared.state,
                shared.measure_id,
                shared.pass,
                shared.session_id.clone(),
                shared
                    .started
                    .map(|started| started.elapsed().as_millis() as u64)
                    .unwrap_or(0),
                shared.failure.clone(),
                shared.result.clone(),
                shared.summary.clone(),
                shared.simulated,
                shared.engine.clone(),
                shared.progress.snapshot(),
            )
        };
        let engine_open = lock(&self.engine_slot).is_some();
        let measuring = state == CalState::Measuring;
        let umik_open = self.mode == CalMode::Calibration && (engine_open || measuring);
        let mut reasons = Map::new();
        if self.mode != CalMode::Calibration {
            reasons.insert(
                "mode".to_string(),
                json!("playback mode: cal_measure/cal_remeasure are rejected with not_in_calibration; cal_enter (audible=true) switches to calibration mode"),
            );
        }
        if measuring {
            reasons.insert(
                "measure".to_string(),
                json!("a measurement is running: poll cal_status; a second cal_measure is rejected with cal_busy"),
            );
        }
        if state == CalState::Failed {
            reasons.insert(
                "last_measure".to_string(),
                json!("the last measurement failed: see measure.error.code/stage_code; no fabricated result is reported"),
            );
        }
        if self.config.mic.is_none() {
            reasons.insert(
                "mic".to_string(),
                json!(self
                    .config
                    .mic_missing_reason
                    .clone()
                    .unwrap_or_else(|| "UMIK declaration missing".to_string())),
            );
        }
        if self.factory.simulated() {
            reasons.insert(
                "simulated".to_string(),
                json!(format!(
                    "engine {} is the offline synthetic engine: measurement results are simulated and designs are PREDICTED_ONLY (never a real-device measurement)",
                    self.factory.engine_name()
                )),
            );
        }
        if !self.runtime_format_supported() {
            reasons.insert(
                "format".to_string(),
                json!(self.format_unsupported_reason()),
            );
        }
        if let Some(last) = summary.as_ref() {
            if last.simulated && result.is_some() {
                reasons.insert(
                    "measured".to_string(),
                    json!("measurement completed on the synthetic engine; quality gates ran on synthetic capture, so treat the numbers as a wiring check, not room data"),
                );
            }
        }
        if self.mode == CalMode::Calibration && !measure_in_progress(&state) && summary.is_some() {
            reasons.insert(
                "remeasure".to_string(),
                json!("cal_remeasure requires a previous measurement and reports the delta; cal_solve designs from the latest measurement without touching the running plan"),
            );
        }
        let solve = {
            let shared = self.shared();
            shared.last_solve_summary.clone()
        };
        let device = self.snapshot().map(|snapshot| {
            json!({
                "present": !snapshot.lifecycle.is_absent(),
                "output_started": snapshot.status.output_started,
                "muted": snapshot.status.muted,
                "state": snapshot.status.state.as_str(),
                "absent_since_ms": snapshot.lifecycle.absent_since_ms(),
                "reopen_attempts": snapshot.lifecycle.reopen_attempts(),
                "last_device_error": snapshot.lifecycle.last_device_error(),
            })
        });
        let device_absent = device
            .as_ref()
            .is_some_and(|device| device["present"] == json!(false));
        if device_absent {
            reasons.insert(
                "device".to_string(),
                json!("output device is absent: the block loop is fail-closed and retries with backoff; cal_measure/cal_enter return device_absent until the device recovers (no fabricated measurement)"),
            );
        }
        json!({
            "device": device,
            "ok": true,
            "mode": self.mode.as_str(),
            "capture_source": if self.mode == CalMode::Calibration { "cal" } else { "program" },
            "program_input_reaches_dsp": self.mode != CalMode::Calibration,
            "playback_restored": self.mode == CalMode::Playback,
            "simulated": simulated,
            "engine": engine,
            "real_capture": !self.factory.simulated(),
            "umik": {
                "open": umik_open,
                "device": self.config.umik_device,
                "capture_channels": self.config.capture_channels,
                "capture_channel": self.config.capture_channel,
                "declared": self.config.mic.is_some(),
                "declaration": self.config.mic.as_ref().map(|mic| json!({
                    "model": mic.model,
                    "serial": mic.serial,
                    "cal_file": mic.cal_file,
                    "cal_sha256": mic.cal_sha256,
                    "orientation": mic.orientation,
                    "clock_domain": mic.clock_domain,
                    "sample_rate_hz": mic.sample_rate_hz,
                })),
                "missing_reason": self.config.mic_missing_reason,
            },
            "config": {
                "layout": self.options.as_ref().map(|options| options.layout.clone()).unwrap_or_else(|| self.config.layout.clone()),
                "channels": self.options.as_ref().map(|options| options.channels.clone()).unwrap_or_else(|| self.config.channels.clone()),
                "level_dbfs": self.options.as_ref().map(|options| options.level_dbfs).unwrap_or(self.config.level_dbfs),
                "artifact_dir": self.config.artifact_dir.display().to_string(),
                "runtime_format": {
                    "block_frames": self.runtime_format.block_frames,
                    "sample_rate": self.runtime_format.sample_rate,
                    "channels": self.runtime_format.channels,
                },
                "sweep": sweep_json(),
            },
            "measure": {
                "state": state.as_str(),
                "measure_id": if measure_id == 0 { Value::Null } else { json!(measure_id) },
                "pass": if pass == 0 { Value::Null } else { json!(pass) },
                "session_id": if session_id.is_empty() { Value::Null } else { json!(session_id) },
                "elapsed_ms": elapsed_ms,
                "progress": {
                    "phase": progress.phase,
                    "channel_index": progress.channel_index,
                    "channels_total": progress.channels_total,
                    "blocks_done": progress.blocks_done,
                    "total_blocks": progress.total_blocks,
                    "finished": progress.finished,
                },
                "error": failure.as_ref().map(|failure| failure.to_json()),
                "result": summary.as_ref().map(|summary| summary.to_json()),
            },
            "solve": {
                "available": summary.is_some(),
                "last": solve,
            },
            "reasons": reasons,
        })
    }

    fn status_ok_json(&self, extra: Value) -> Value {
        let mut status = self.status_json();
        if let (Some(object), Some(extra)) = (status.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        status
    }

    fn runtime_format_supported(&self) -> bool {
        self.format_unsupported_reason().is_empty()
    }

    fn format_unsupported_reason(&self) -> String {
        if self.runtime_format.sample_rate != STAGE_UMIK_SAMPLE_RATE_HZ {
            return format!(
                "calibration requires the runtime data-plane format to run at 48000 Hz (UMIK-1 is 48 kHz-only); the running format is {} Hz",
                self.runtime_format.sample_rate
            );
        }
        if !khaen_core::rt::BLOCK_SIZES.contains(&(self.runtime_format.block_frames as u16)) {
            return format!(
                "calibration playback needs a runtime block size of {:?} frames; the running format uses {} frames",
                khaen_core::rt::BLOCK_SIZES,
                self.runtime_format.block_frames
            );
        }
        if self.runtime_format.channels == 0 {
            return "the runtime data-plane format declares zero channels".to_string();
        }
        String::new()
    }

    // ------------------------------------------------------------------
    // op: cal_enter / cal_leave
    // ------------------------------------------------------------------

    /// cal_enter：切校准模式（需 audible=true）、确认 UMIK 可用。
    pub fn enter(&mut self, request: &Map<String, Value>) -> Result<Value, ControlError> {
        // 与热重配互斥（M3b）：重配正在改设备/格式时不允许进校准模式。
        if self.gate.is_busy() {
            return Err(ControlError::new(
                crate::control::reconfigure::error::RECONFIGURING,
                format!(
                    "a hot reconfiguration (channels_commit) is in progress: cal_enter is rejected with {RECONFIGURING_LABEL} until the block loop finishes reconfiguring the output device"
                ),
            ));
        }
        let audible = bool_field(request, "audible")?;
        if !audible {
            return Err(ControlError::new(
                error::AUDIBLE_AUTH_REQUIRED,
                "cal_enter requires \"audible\": true: switching to calibration mode makes the speakers play a sweep; the existing fault/mute state machine still guards the output",
            ));
        }
        self.reap_finished_job();
        if self.job.is_some() {
            return Err(ControlError::new(
                error::BUSY,
                "a calibration measurement is still running; poll cal_status and retry cal_enter after it finishes",
            ));
        }
        if self.mode == CalMode::Calibration {
            return Ok(self.status_ok_json(
                json!({ "entered": false, "note": "already in calibration mode" }),
            ));
        }
        if !self.runtime_format_supported() {
            return Err(ControlError::new(
                error::FORMAT_UNSUPPORTED,
                self.format_unsupported_reason(),
            ));
        }
        let snapshot = self.require_snapshot()?;
        self.ensure_device_present(&snapshot)?;
        self.ensure_not_muted(&snapshot)?;
        let mic = self.mic_or_error()?;
        let options = self.resolve_options(request)?;
        let stage = self.build_stage_config(&mic, &options, audible)?;
        let progress = Arc::new(CalProgress::new());
        let created = self
            .factory
            .create(&stage, Arc::clone(&progress))
            .map_err(|error| engine_error(&error))?;
        // probe 只验证 UMIK 可打开/参数可设；失败 → 保持播放模式（如实回滚）。
        let mut created = created;
        let probe = created
            .probe(&stage.devices)
            .map_err(|error| engine_error(&error))?;
        if !probe.capture_ok {
            return Err(ControlError::new(
                error::UMIK_OPEN_FAILED,
                format!(
                    "UMIK capture device is not usable ({}): {}",
                    self.config.umik_device, probe.notes
                ),
            ));
        }
        {
            let mut slot = lock(&self.engine_slot);
            *slot = Some(created);
        }
        self.mode = CalMode::Calibration;
        self.options = Some(options.clone());
        self.progress = Arc::clone(&progress);
        self.next_pass = 0;
        {
            let mut shared = self.shared();
            shared.state = CalState::Idle;
            shared.measure_id = 0;
            shared.pass = 0;
            shared.session_id.clear();
            shared.started = None;
            shared.failure = None;
            shared.result = None;
            shared.summary = None;
            shared.simulated = self.factory.simulated();
            shared.engine = self.factory.engine_name().to_string();
            shared.progress = progress;
        }
        Ok(self.status_ok_json(json!({
            "entered": true,
            "probe": probe_json(&probe),
            "note": "UMIK probe passed; the capture device is opened per measurement session (not held open while idle) so playback mode never occupies it",
        })))
    }

    /// cal_leave：取消进行中的测量，回播放模式，释放 UMIK。
    pub fn leave(&mut self) -> Result<Value, ControlError> {
        self.cancel_and_join();
        let was_mode = self.mode;
        {
            let mut slot = lock(&self.engine_slot);
            let _ = slot.take();
        }
        self.mode = CalMode::Playback;
        self.options = None;
        self.progress.reset();
        // 不擦除最近测量/求解状态：leave 之后 cal_status 仍要能如实读出证据
        // （测量中离开时，取消会让状态变成 failed/CANCELLED，同样如实保留）。
        if let Ok(mut shared) = self.shared.try_lock() {
            shared.started = None;
        }
        Ok(self.status_ok_json(json!({
            "left": was_mode == CalMode::Calibration,
            "note": "playback mode restored; the UMIK capture device was released and the calibration playback (if any) was ended",
        })))
    }

    // ------------------------------------------------------------------
    // op: cal_measure / cal_remeasure
    // ------------------------------------------------------------------

    /// cal_measure / cal_remeasure：启动一次测量（默认异步；可选 wait_ms 等待完成）。
    pub fn measure(
        &mut self,
        request: &Map<String, Value>,
        remeasure: bool,
    ) -> Result<Value, ControlError> {
        let op = if remeasure {
            "cal_remeasure"
        } else {
            "cal_measure"
        };
        let audible = bool_field(request, "audible")?;
        if !audible {
            return Err(ControlError::new(
                error::AUDIBLE_AUTH_REQUIRED,
                format!("{op} requires \"audible\": true: the measurement plays an audible sweep; the existing fault/mute state machine still guards the output"),
            ));
        }
        if self.mode != CalMode::Calibration {
            return Err(ControlError::new(
                error::NOT_IN_CALIBRATION,
                format!("{op} requires calibration mode: cal_enter (audible=true) first"),
            ));
        }
        self.reap_finished_job();
        if self.job.is_some() {
            return Err(ControlError::new(
                error::BUSY,
                "a calibration measurement is already running; poll cal_status instead of starting a concurrent measurement",
            ));
        }
        let snapshot = self.require_snapshot()?;
        self.ensure_device_present(&snapshot)?;
        self.ensure_not_muted(&snapshot)?;
        let previous = {
            let shared = self.shared();
            shared.summary.clone()
        };
        if remeasure && previous.is_none() {
            return Err(ControlError::new(
                error::NO_MEASUREMENT,
                "cal_remeasure needs a previous measurement in this calibration session; run cal_measure first",
            ));
        }
        let options = match self.options.clone() {
            Some(options) => options,
            None => {
                return Err(ControlError::new(
                    error::NOT_IN_CALIBRATION,
                    "calibration session options are missing; cal_enter again",
                ))
            }
        };
        let mic = self.mic_or_error()?;
        let stage = self.build_stage_config(&mic, &options, audible)?;
        let engine = {
            let mut slot = lock(&self.engine_slot);
            slot.take()
        }
        .ok_or_else(|| {
            ControlError::new(
                error::UMIK_OPEN_FAILED,
                "the calibration engine is not available (its probe/session state was lost); cal_leave + cal_enter again",
            )
        })?;
        let measure_id = self.next_measure_id.saturating_add(1);
        self.next_measure_id = measure_id;
        let pass = self.next_pass.saturating_add(1);
        self.next_pass = pass;
        let session_id = format!("cal-{measure_id:06}-pass{pass:02}");
        let progress = Arc::clone(&self.progress);
        progress.reset();
        let started = Instant::now();
        {
            let mut shared = self.shared();
            shared.state = CalState::Measuring;
            shared.measure_id = measure_id;
            shared.pass = pass;
            shared.session_id = session_id.clone();
            shared.started = Some(started);
            shared.failure = None;
            shared.result = None;
            shared.summary = None;
            shared.simulated = self.factory.simulated();
            shared.engine = self.factory.engine_name().to_string();
            shared.progress = Arc::clone(&progress);
        }
        let worker = WorkerArgs {
            config: self.config.clone(),
            stage,
            options: options.clone(),
            measure_id,
            pass,
            session_id: session_id.clone(),
            engine,
            progress,
            cancel: CancelToken::new(),
            shared: Arc::clone(&self.shared),
            engine_slot: Arc::clone(&self.engine_slot),
            previous: if remeasure { previous } else { None },
            simulated: self.factory.simulated(),
        };
        let cancel = worker.cancel.clone();
        let handle = std::thread::Builder::new()
            .name(format!("khaen-cal-{measure_id}"))
            .spawn(move || run_measure_worker(worker))
            .map_err(|error| {
                ControlError::new(
                    error::ARTIFACT_IO,
                    format!("cannot start the calibration measurement thread: {error}"),
                )
            })?;
        self.job = Some(MeasureJob { cancel, handle });
        let wait_ms = unsigned_field(request, "wait_ms")?
            .unwrap_or(0)
            .min(600_000);
        if wait_ms > 0 {
            wait_for_measurement(&self.shared, Duration::from_millis(wait_ms));
            return self.measure_completion_response(
                measure_id,
                pass,
                &session_id,
                started,
                wait_ms,
            );
        }
        Ok(json!({
            "ok": true,
            "accepted": true,
            "op": op,
            "measure_id": measure_id,
            "pass": pass,
            "session_id": session_id,
            "state": "measuring",
            "simulated": self.factory.simulated(),
            "engine": self.factory.engine_name(),
            "artifacts_dir": self.session_dir(&session_id).display().to_string(),
            "note": "measurement runs on a control-plane thread (playback still happens on the audio thread through the calibration playback seam); poll cal_status; cal_measure with \"wait_ms\" waits for completion",
            "result": Value::Null,
        }))
    }

    fn measure_completion_response(
        &self,
        measure_id: u64,
        pass: u32,
        session_id: &str,
        started: Instant,
        wait_ms: u64,
    ) -> Result<Value, ControlError> {
        let (state, failure, result) = {
            let shared = self.shared();
            (shared.state, shared.failure.clone(), shared.result.clone())
        };
        match state {
            CalState::Done => Ok(result.unwrap_or_else(|| {
                json!({
                    "ok": true,
                    "measure_id": measure_id,
                    "pass": pass,
                    "session_id": session_id,
                    "state": "done",
                })
            })),
            CalState::Failed => {
                let failure = failure
                    .unwrap_or_else(|| CalFailure::new("measure_failed", "measurement failed"));
                Err(ControlError::new(
                    error::MEASURE_FAILED,
                    format!(
                        "measurement {measure_id} (pass {pass}) failed after {} ms [{}]: {}",
                        started.elapsed().as_millis(),
                        failure
                            .stage_code
                            .clone()
                            .unwrap_or_else(|| failure.code.clone()),
                        failure.message
                    ),
                ))
            }
            _ => Ok(json!({
                "ok": true,
                "accepted": true,
                "measure_id": measure_id,
                "pass": pass,
                "session_id": session_id,
                "state": state.as_str(),
                "wait_ms": wait_ms,
                "artifacts_dir": self.session_dir(session_id).display().to_string(),
                "note": "still measuring after wait_ms; poll cal_status",
                "result": Value::Null,
            })),
        }
    }

    // ------------------------------------------------------------------
    // op: cal_solve
    // ------------------------------------------------------------------

    /// cal_solve：用最近测量 + 当前运行时格式算单点设计（FIR），return design JSON +
    /// 建议 plan 摘要；只写 artifact，不提交、不改正在跑的输出。
    pub fn solve(
        &mut self,
        request: &Map<String, Value>,
        current_plan: Option<&Plan>,
    ) -> Result<Value, ControlError> {
        self.reap_finished_job();
        if self.job.is_some() {
            return Err(ControlError::new(
                error::BUSY,
                "a calibration measurement is running; wait for it before cal_solve",
            ));
        }
        let last = {
            let shared = self.shared();
            shared.last.clone()
        };
        let Some(last) = last else {
            return Err(ControlError::new(
                error::NO_MEASUREMENT,
                "cal_solve needs a successful measurement in this calibration session (cal_enter then cal_measure)",
            ));
        };
        let target = match string_field(request, "target")? {
            Some(value) => value.to_string(),
            None => "flat".to_string(),
        };
        if !solve::ALLOWED_TARGETS.contains(&target.as_str()) {
            return Err(ControlError::new(
                host_error::BAD_REQUEST,
                format!(
                    "field target must be one of {:?} (got {target:?})",
                    solve::ALLOWED_TARGETS
                ),
            ));
        }
        let design = solve::solve_design(&last, &self.runtime_format, current_plan, &target)?;
        {
            let mut shared = self.shared();
            shared.last_solve_summary = Some(design.clone());
            shared.last_solve = Some(design.clone());
        }
        Ok(json!({
            "ok": true,
            "applied": false,
            "verdict": "PREDICTED_ONLY",
            "design": design,
            "note": "design artifacts were written next to the measurement; nothing was committed to the running data plane",
        }))
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    fn require_snapshot(&self) -> Result<RuntimeSnapshot, ControlError> {
        self.snapshot().ok_or_else(|| {
            ControlError::new(
                error::RUNTIME_NOT_RUNNING,
                "the rt-host block loop has not published a runtime snapshot yet; calibration needs a running data plane",
            )
        })
    }

    fn ensure_device_present(&self, snapshot: &RuntimeSnapshot) -> Result<(), ControlError> {
        if snapshot.lifecycle.is_absent() {
            let since = snapshot.lifecycle.absent_since_ms();
            let attempts = snapshot.lifecycle.reopen_attempts();
            let last = snapshot
                .lifecycle
                .last_device_error()
                .map(str::to_string)
                .unwrap_or_else(|| "unknown".to_string());
            return Err(ControlError::new(
                error::DEVICE_ABSENT,
                format!(
                    "output device is absent (absent since {since:?} ms, reopen attempts {attempts:?}, last error {last}); calibration cannot produce a real measurement without a running output device"
                ),
            ));
        }
        if !snapshot.status.output_started {
            return Err(ControlError::new(
                error::DEVICE_ABSENT,
                "the output device has not started; calibration cannot play a sweep",
            ));
        }
        Ok(())
    }

    fn ensure_not_muted(&self, snapshot: &RuntimeSnapshot) -> Result<(), ControlError> {
        if snapshot.status.muted
            || snapshot.status.state.requires_mute()
            || snapshot.lifecycle.is_absent()
        {
            return Err(ControlError::new(
                error::AUDIO_BLOCKED,
                format!(
                    "output is muted/safety-muted (muted={}, state={:?}): an audible sweep would be silent, so the measurement is rejected instead of fabricating data",
                    snapshot.status.muted, snapshot.status.state
                ),
            ));
        }
        Ok(())
    }

    fn mic_or_error(&self) -> Result<UmikDeviceConfig, ControlError> {
        self.config.mic.clone().ok_or_else(|| {
            ControlError::new(
                error::MIC_DECLARATION_MISSING,
                self.config
                    .mic_missing_reason
                    .clone()
                    .unwrap_or_else(|| "UMIK declaration missing".to_string()),
            )
        })
    }

    /// enter 的会话参数：请求可显式给 layout/channels/level_dbfs，否则用配置默认。
    fn resolve_options(
        &self,
        request: &Map<String, Value>,
    ) -> Result<SessionOptions, ControlError> {
        let layout = match string_field(request, "layout")? {
            Some(value) => value.to_string(),
            None => self.config.layout.clone(),
        };
        if layout.trim().is_empty() {
            return Err(ControlError::new(
                error::CONFIG_INVALID,
                "field layout must be a non-empty string",
            ));
        }
        let channels = match u32_list_field(request, "channels")? {
            Some(channels) => channels,
            None => self.config.channels.clone(),
        };
        if channels.is_empty() {
            return Err(ControlError::new(
                error::CONFIG_INVALID,
                "calibration channel list is empty: declare the DAC channels to measure (request field channels or KHAEN_CAL_CHANNELS)",
            ));
        }
        if channels.len() > 22 {
            return Err(ControlError::new(
                error::CONFIG_INVALID,
                format!("{} calibration channels declared; the runtime supports at most 22 output channels", channels.len()),
            ));
        }
        let mut seen = Vec::new();
        for channel in &channels {
            if *channel >= self.runtime_format.channels as u32 {
                return Err(ControlError::new(
                    error::CONFIG_INVALID,
                    format!(
                        "calibration channel {channel} is outside the running format ({} channels)",
                        self.runtime_format.channels
                    ),
                ));
            }
            if seen.contains(channel) {
                return Err(ControlError::new(
                    error::CONFIG_INVALID,
                    format!("duplicate calibration channel {channel}"),
                ));
            }
            seen.push(*channel);
        }
        let level_dbfs = match f64_field(request, "level_dbfs")? {
            Some(value) => value,
            None => self.config.level_dbfs,
        };
        if !level_dbfs.is_finite() || !(-60.0..=-1.0).contains(&level_dbfs) {
            return Err(ControlError::new(
                error::CONFIG_INVALID,
                format!("calibration drive level {level_dbfs} dBFS is outside [-60, -1]"),
            ));
        }
        Ok(SessionOptions {
            layout,
            channels,
            level_dbfs,
        })
    }

    /// 组装 T67 stage 配置（几何/电平/扫频常量；不触碰设备）。
    fn build_stage_config(
        &self,
        mic: &UmikDeviceConfig,
        options: &SessionOptions,
        audible: bool,
    ) -> Result<StageBackendConfig, ControlError> {
        if !self.runtime_format_supported() {
            return Err(ControlError::new(
                error::FORMAT_UNSUPPORTED,
                self.format_unsupported_reason(),
            ));
        }
        let outputs: Vec<khaen_calibration::sweep::SweepOutput> = options
            .channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                khaen_calibration::sweep::SweepOutput::new(
                    index as u32,
                    format!("C{channel}"),
                    *channel,
                )
            })
            .collect();
        let devices = StageDeviceConfig {
            // 播放设备由 rt-host 块线程持有（校准播放 seam）；这里只是 T67 配置字段，
            // 真实设备名不属于校准配置（不抢 owner）。
            playback_device: "rt-host-owned".to_string(),
            playback_channels: u16::try_from(self.runtime_format.channels).map_err(|_| {
                ControlError::new(
                    error::FORMAT_UNSUPPORTED,
                    "runtime channel count does not fit u16",
                )
            })?,
            capture_device: self.config.umik_device.clone(),
            capture_channels: self.config.capture_channels,
            capture_channel: self.config.capture_channel,
            sample_rate: STAGE_UMIK_SAMPLE_RATE_HZ,
            block_frames: u16::try_from(self.runtime_format.block_frames).map_err(|_| {
                ControlError::new(
                    error::FORMAT_UNSUPPORTED,
                    "runtime block size does not fit u16",
                )
            })?,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            settle_ms: DEFAULT_SETTLE_MS,
            sample_format: StageDeviceConfig::default().sample_format,
        };
        let stage = StageBackendConfig {
            mode: StageMeasurementMode::AcousticUmik,
            devices,
            mic: Some(mic.clone()),
            level: StageLevelConfig {
                default_drive_level_dbfs: options.level_dbfs,
                min_drive_level_dbfs: -60.0,
                max_drive_level_dbfs: -1.0,
                hot_threshold_dbfs: -30.0,
                hot_level_authorized: audible,
            },
            sweep: StageSweepConfig {
                start_hz: LEGACY_SWEEP_START_HZ,
                end_hz: LEGACY_SWEEP_END_HZ,
                seconds: LEGACY_SWEEP_SECONDS,
                fade_ms: LEGACY_SWEEP_FADE_MS,
                repeats: SWEEP_REPEATS,
                repeat_gap_seconds: SWEEP_REPEAT_GAP_SECONDS,
                pre_seconds: LEGACY_SWEEP_PRE_SECONDS,
                post_seconds: LEGACY_SWEEP_POST_SECONDS,
                k_main_trim_db: 0.0,
            },
            protection: StageProtectionConfig::default(),
            outputs,
            engine: khaen_calibration::stage_backend::EngineKind::Auto,
            session_label: "rt-host-cal".to_string(),
            graph_id: self
                .graph_id()
                .unwrap_or_else(|| PLACEHOLDER_GRAPH_ID.to_string()),
            epoch_id: 1,
            apply_settle_ms: DEFAULT_APPLY_SETTLE_MS,
            remeasure_playback: khaen_calibration::stage_backend::RemeasurePlayback::Aplay,
            remeasure_host: Default::default(),
        };
        stage.validate().map_err(tax_config_error)?;
        Ok(stage)
    }

    fn graph_id(&self) -> Option<String> {
        self.snapshot()
            .and_then(|snapshot| snapshot.status.graph.graph_id.clone())
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.config.artifact_dir.join(session_id)
    }

    /// 回收已结束的测量线程（join；不阻塞运行中的线程）。
    fn reap_finished_job(&mut self) {
        let finished = self
            .job
            .as_ref()
            .map(|job| job.handle.is_finished())
            .unwrap_or(false);
        if !finished {
            return;
        }
        if let Some(job) = self.job.take() {
            let _ = job.handle.join();
        }
    }

    /// 取消并等待测量线程（leave/shutdown 用）。
    fn cancel_and_join(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.request();
            let _ = job.handle.join();
        }
    }
}

fn measure_in_progress(state: &CalState) -> bool {
    *state == CalState::Measuring
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_measurement(shared: &Arc<Mutex<CalShared>>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let state = lock(shared).state;
            if state != CalState::Measuring {
                return;
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ===========================================================================
// 请求字段解析（错误形状与 host ControlState 一致：bad_request）
// ===========================================================================

fn bad_request(message: impl Into<String>) -> ControlError {
    ControlError::new(host_error::BAD_REQUEST, message)
}

fn bool_field(request: &Map<String, Value>, key: &str) -> Result<bool, ControlError> {
    match request.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(bad_request(format!("field {key} must be a boolean"))),
        None => Ok(false),
    }
}

fn string_field<'a>(
    request: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, ControlError> {
    match request.get(key) {
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(format!("field {key} must be a string"))),
    }
}

fn unsigned_field(request: &Map<String, Value>, key: &str) -> Result<Option<u64>, ControlError> {
    match request.get(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| bad_request(format!("field {key} must be a non-negative integer"))),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(format!(
            "field {key} must be a non-negative integer"
        ))),
    }
}

fn f64_field(request: &Map<String, Value>, key: &str) -> Result<Option<f64>, ControlError> {
    match request.get(key) {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Some)
            .ok_or_else(|| bad_request(format!("field {key} must be a finite number"))),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(format!("field {key} must be a finite number"))),
    }
}

fn u32_list_field(
    request: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<u32>>, ControlError> {
    match request.get(key) {
        Some(Value::Array(values)) => {
            let mut channels = Vec::with_capacity(values.len());
            for value in values {
                let Some(number) = value.as_u64() else {
                    return Err(bad_request(format!(
                        "field {key} must be an array of non-negative integers"
                    )));
                };
                let channel = u32::try_from(number).map_err(|_| {
                    bad_request(format!("field {key} channel {number} is too large"))
                })?;
                channels.push(channel);
            }
            Ok(Some(channels))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(format!(
            "field {key} must be an array of non-negative integers"
        ))),
    }
}

fn json_f64(value: f64) -> Value {
    if value.is_finite() {
        Value::from(value)
    } else {
        Value::Null
    }
}

fn json_opt_f64(value: Option<f64>) -> Value {
    match value {
        Some(value) => json_f64(value),
        None => Value::Null,
    }
}

/// 扫频常量 JSON（证据：每项注明来自旧 cal_control.rs 还是 T61/T62 口径）。
fn sweep_json() -> Value {
    json!({
        "start_hz": LEGACY_SWEEP_START_HZ,
        "end_hz": LEGACY_SWEEP_END_HZ,
        "seconds": LEGACY_SWEEP_SECONDS,
        "level_dbfs": LEGACY_SWEEP_LEVEL_DBFS,
        "fade_ms": LEGACY_SWEEP_FADE_MS,
        "repeats": SWEEP_REPEATS,
        "repeat_gap_seconds": SWEEP_REPEAT_GAP_SECONDS,
        "pre_seconds": LEGACY_SWEEP_PRE_SECONDS,
        "post_seconds": LEGACY_SWEEP_POST_SECONDS,
        "source": {
            "rate_hz": "legacy cal_control.rs SWEEP_RATE_HZ=48000",
            "seconds": "legacy cal_control.rs SWEEP_SECONDS=5.0",
            "level": "legacy cal_control.rs SWEEP_LEVEL=0.25118864 (=-12 dBFS)",
            "pre_post": "legacy cal_control.rs pre-roll (1.5 s)",
            "repeats_gap": "T61/T62 acoustic default (3 repeats x 0.5 s gap; clock ppm needs multiple repeats)",
        },
    })
}

fn probe_json(probe: &khaen_calibration::stage_backend::StageProbe) -> Value {
    json!({
        "playback_ok": probe.playback_ok,
        "capture_ok": probe.capture_ok,
        "capture_sample_rates": probe.capture_sample_rates,
        "capture_max_channels": probe.capture_max_channels,
        "playback_sample_rates": probe.playback_sample_rates,
        "notes": probe.notes,
    })
}

/// StageError -> 契约 B ControlError（稳定码映射；精确 stage code 留在 message 里）。
fn engine_error(error: &StageError) -> ControlError {
    use khaen_calibration::stage_backend::{
        E_CAPTURE_CLIP, E_CAPTURE_INCOMPLETE, E_DEVICE_UNAVAILABLE, E_LEVEL_NOT_AUTHORIZED,
        E_MIC_DECLARATION,
    };
    let code = match error.code.as_str() {
        E_DEVICE_UNAVAILABLE => error::DEVICE_ABSENT,
        E_MIC_DECLARATION => error::MIC_DECLARATION_MISSING,
        E_LEVEL_NOT_AUTHORIZED => error::AUDIBLE_AUTH_REQUIRED,
        E_CAPTURE_CLIP | E_CAPTURE_INCOMPLETE => error::CONFIG_INVALID,
        _ => error::UMIK_OPEN_FAILED,
    };
    ControlError::new(code, format!("{}: {}", error.code, error.message))
}

/// T67 配置校验失败 -> ControlError（稳定码按 stage code 映射）。
fn tax_config_error(failure: StageFailure) -> ControlError {
    use khaen_calibration::stage_backend::{E_LEVEL_NOT_AUTHORIZED, E_MIC_DECLARATION};
    let code = match failure.code.as_str() {
        E_MIC_DECLARATION => error::MIC_DECLARATION_MISSING,
        E_LEVEL_NOT_AUTHORIZED => error::AUDIBLE_AUTH_REQUIRED,
        _ => error::CONFIG_INVALID,
    };
    ControlError::new(
        code,
        format!(
            "T67 stage config rejected: {}: {}",
            failure.code, failure.message
        ),
    )
}

// ===========================================================================
// 测量线程：T61/T62/T67 测量 -> 产物 -> IR -> 结果
// ===========================================================================

fn run_measure_worker(args: WorkerArgs) {
    let WorkerArgs {
        config,
        stage,
        options,
        measure_id,
        pass,
        session_id,
        mut engine,
        progress,
        cancel,
        shared,
        engine_slot,
        previous,
        simulated,
    } = args;
    let session_dir = config.artifact_dir.join(&session_id);
    let total_channels = options.channels.len() as u64;
    progress.set_phase("preflight");
    let mut channels: Vec<ChannelMeasurement> = Vec::with_capacity(options.channels.len());
    let mut failure: Option<CalFailure> = None;
    // 多通道 = 逐通道一次 T67 测量会话（T61/T62 的质量门/时基口径按单输出会话设计；
    // 一次会话里依次播多路会让稳健噪声底把后一路扫频当噪声，质量门会失败而不是给假数）。
    for (index, channel) in options.channels.iter().enumerate() {
        if cancel.is_requested() {
            failure = Some(CalFailure {
                code: "CANCELLED".to_string(),
                stage_code: Some("CANCELLED".to_string()),
                message: "calibration measurement cancelled (cal_leave/shutdown)".to_string(),
                cancelled: true,
            });
            break;
        }
        progress.set_channels(index as u64, total_channels);
        progress.set_phase("measuring");
        let channel_session = format!("{session_id}-ch{channel}");
        let channel_dir = config.artifact_dir.join(&channel_session);
        let mut channel_stage = stage.clone();
        channel_stage.outputs = vec![khaen_calibration::sweep::SweepOutput::new(
            0,
            format!("C{channel}"),
            *channel,
        )];
        let measurement = run_acoustic_measurement(
            &channel_stage,
            &mut *engine,
            &channel_session,
            &options.layout,
            options.level_dbfs,
            &cancel,
            true,
        );
        match measurement {
            Err(error) => {
                let cal_failure = CalFailure::from_stage(&error);
                let _ = write_error_artifact(&channel_dir, &cal_failure, None);
                failure = Some(cal_failure);
                break;
            }
            Ok(output) => {
                progress.set_phase("extracting_ir");
                match finish_channel(&config, *channel, &channel_session, &channel_dir, &output) {
                    Ok(measurement) => channels.push(measurement),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
        }
    }
    if failure.is_none() && channels.len() != options.channels.len() {
        failure = Some(CalFailure::new(
            "measurement_failed",
            "internal: not every declared channel produced a measurement",
        ));
    }
    let outcome = match failure {
        Some(failure) => {
            let _ = write_error_artifact(&session_dir, &failure, None);
            progress.set_phase(if failure.cancelled {
                "cancelled"
            } else {
                "failed"
            });
            FinishState::Failed(failure)
        }
        None => {
            progress.set_phase("writing_artifacts");
            match finish_measurement(
                &options,
                measure_id,
                pass,
                &session_id,
                &session_dir,
                &channels,
                previous.as_ref(),
                simulated,
            ) {
                Ok(payload) => {
                    progress.set_finished(true);
                    progress.set_phase("done");
                    FinishState::Done(Box::new(payload))
                }
                Err(failure) => {
                    let _ = write_error_artifact(&session_dir, &failure, None);
                    progress.set_phase("failed");
                    FinishState::Failed(failure)
                }
            }
        }
    };
    finish_worker(engine, &engine_slot, outcome, &shared);
}

/// 逐通道的测量结果（worker 内部聚合用）。
struct ChannelMeasurement {
    channel: u32,
    session_id: String,
    session_dir: PathBuf,
    summary: khaen_calibration::stage_backend::MeasurementSummary,
    clock: ClockSummary,
    quality: Value,
    artifacts: Vec<ArtifactFile>,
    irs: Vec<IrArtifact>,
    measurement: Value,
}

/// 一个通道的完整产物（T62 报告 / WAV / 反卷积 IR / 质量指标）。
fn finish_channel(
    config: &CalibrationConfig,
    channel: u32,
    session_id: &str,
    session_dir: &Path,
    output: &MeasurementOutput,
) -> Result<ChannelMeasurement, CalFailure> {
    let mut artifacts = Vec::new();
    if let Err(error) = write_capture_artifacts(
        session_dir,
        channel,
        output,
        config.capture_channels,
        STAGE_UMIK_SAMPLE_RATE_HZ,
        &mut artifacts,
    ) {
        let failure = CalFailure::new(error::ARTIFACT_IO, error);
        let _ = write_error_artifact(session_dir, &failure, Some(&artifacts));
        return Err(failure);
    }
    let clock = clock_from_capture_report(&output.capture_report_json);
    let timeline = match output.timeline.clone() {
        Some(timeline) => timeline,
        None => {
            let failure = CalFailure::new(
                "measurement_failed",
                "T67 acoustic measurement returned no sweep timeline; IR extraction is impossible",
            );
            let _ = write_error_artifact(session_dir, &failure, Some(&artifacts));
            return Err(failure);
        }
    };
    let irs = match extract_irs(
        &timeline,
        &output.samples,
        output.summary.seed_render_frame,
        &IrExtractConfig::default(),
    ) {
        Ok(irs) => irs,
        Err(error) => {
            let failure = CalFailure {
                code: "ir_extract_failed".to_string(),
                stage_code: None,
                message: format!(
                    "IR extraction rejected the capture on channel {channel}: {error}"
                ),
                cancelled: false,
            };
            let _ = write_error_artifact(session_dir, &failure, Some(&artifacts));
            return Err(failure);
        }
    };
    let ir_artifacts = match write_ir_artifacts(session_dir, channel, &irs) {
        Ok(ir_artifacts) => ir_artifacts,
        Err(error) => {
            let failure = CalFailure::new(error::ARTIFACT_IO, error);
            let _ = write_error_artifact(session_dir, &failure, Some(&artifacts));
            return Err(failure);
        }
    };
    artifacts.extend(ir_artifacts.iter().map(|ir| ir.artifact.clone()));
    let quality = quality_json(&output.summary, &clock);
    let measurement = measurement_json(&output.summary, output, channel);
    Ok(ChannelMeasurement {
        channel,
        session_id: session_id.to_string(),
        session_dir: session_dir.to_path_buf(),
        summary: output.summary.clone(),
        clock,
        quality,
        artifacts,
        irs: ir_artifacts,
        measurement,
    })
}

/// 工作线程收尾：先把引擎放回槽（锁顺序：engine_slot 单独锁、结束后再锁 shared）。
enum FinishState {
    Done(Box<DonePayload>),
    Failed(CalFailure),
}

struct DonePayload {
    summary: CalSummary,
    last: LastMeasurement,
    result: Value,
}

fn finish_worker(
    engine: Box<dyn StageAudioEngine>,
    engine_slot: &Arc<Mutex<Option<Box<dyn StageAudioEngine>>>>,
    state: FinishState,
    shared: &Arc<Mutex<CalShared>>,
) {
    {
        let mut slot = lock(engine_slot);
        *slot = Some(engine);
    }
    let mut guard = lock(shared);
    match state {
        FinishState::Done(payload) => {
            guard.state = CalState::Done;
            guard.summary = Some(payload.summary);
            guard.last = Some(payload.last);
            guard.result = Some(payload.result);
            guard.failure = None;
        }
        FinishState::Failed(failure) => {
            guard.state = CalState::Failed;
            guard.failure = Some(failure);
        }
    }
    guard.started = None;
}

/// 聚合多通道测量结果 -> 摘要 / LastMeasurement / 结果 JSON。
#[allow(clippy::too_many_arguments)]
fn finish_measurement(
    options: &SessionOptions,
    measure_id: u64,
    pass: u32,
    session_id: &str,
    session_dir: &Path,
    channels: &[ChannelMeasurement],
    previous: Option<&CalSummary>,
    simulated: bool,
) -> Result<DonePayload, CalFailure> {
    if channels.is_empty() {
        return Err(CalFailure::new(
            "measurement_failed",
            "no channel measurement was produced",
        ));
    }
    let snr_values: Vec<f64> = channels
        .iter()
        .filter_map(|channel| channel.summary.snr_db)
        .collect();
    let snr_db = if snr_values.len() == channels.len() {
        Some(snr_values.iter().copied().fold(f64::INFINITY, f64::min))
    } else {
        None
    };
    let clock_ppms: Vec<f64> = channels
        .iter()
        .filter_map(|channel| channel.clock.clock_ppm)
        .collect();
    let clock_ppm = if clock_ppms.len() == channels.len() {
        let mut sorted = clock_ppms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(sorted[sorted.len() / 2])
    } else {
        None
    };
    let clock_uncertainty = channels
        .iter()
        .filter_map(|channel| channel.clock.clock_ppm_uncertainty_ppm)
        .fold(None::<f64>, |acc, value| {
            Some(acc.map(|current| current.max(value)).unwrap_or(value))
        });
    let drift_significant = if channels.iter().all(|c| c.clock.drift_significant.is_some()) {
        Some(
            channels
                .iter()
                .any(|c| c.clock.drift_significant == Some(true)),
        )
    } else {
        None
    };
    let clock_ppm_ok = if channels.iter().all(|c| c.clock.clock_ppm_ok.is_some()) {
        Some(channels.iter().all(|c| c.clock.clock_ppm_ok == Some(true)))
    } else {
        None
    };
    let summary = CalSummary {
        measure_id,
        pass,
        session_id: session_id.to_string(),
        mode: channels[0].summary.mode.as_str().to_string(),
        engine: channels[0].summary.engine.clone(),
        simulated,
        sample_rate: channels[0].summary.sample_rate,
        drive_level_dbfs: options.level_dbfs,
        capture_peak_dbfs: channels
            .iter()
            .map(|channel| channel.summary.capture_peak_dbfs)
            .fold(f64::NEG_INFINITY, f64::max),
        snr_db,
        capture_frames: channels
            .iter()
            .map(|channel| channel.summary.capture_frames)
            .sum(),
        timeline_frames: channels
            .iter()
            .map(|channel| channel.summary.timeline_frames)
            .sum(),
        valid_slots: channels
            .iter()
            .map(|channel| channel.summary.valid_slots)
            .sum(),
        total_slots: channels
            .iter()
            .map(|channel| channel.summary.total_slots)
            .sum(),
        clock_ppm,
        clock_ppm_uncertainty_ppm: clock_uncertainty,
        drift_significant,
        clock_ppm_ok,
        xruns: channels.iter().map(|channel| channel.summary.xruns).sum(),
        clipped_samples: channels
            .iter()
            .map(|channel| channel.summary.clipped_samples)
            .sum(),
    };
    let artifacts: Vec<ArtifactFile> = channels
        .iter()
        .flat_map(|channel| channel.artifacts.iter().cloned())
        .collect();
    let irs: Vec<IrArtifact> = channels
        .iter()
        .flat_map(|channel| channel.irs.iter().cloned())
        .collect();
    let last = LastMeasurement {
        summary: summary.clone(),
        layout: options.layout.clone(),
        channels: options.channels.clone(),
        irs: irs.iter().map(ir_artifact_to_ir).collect(),
        session_dir: session_dir.to_path_buf(),
    };
    let delta = previous.map(|previous| delta_json(previous, &summary));
    let result = json!({
        "ok": true,
        "schema": MEASUREMENT_SCHEMA,
        "measure_id": measure_id,
        "pass": pass,
        "session_id": session_id,
        "state": "done",
        "simulated": simulated,
        "engine": summary.engine,
        "mode": summary.mode,
        "layout": options.layout,
        "channels": options.channels,
        "level_dbfs": json_f64(options.level_dbfs),
        "sweep": sweep_json(),
        "quality": aggregate_quality_json(channels, &summary),
        "measurements": channels
            .iter()
            .map(|channel| json!({
                "channel": channel.channel,
                "session_id": channel.session_id,
                "artifacts_dir": channel.session_dir.display().to_string(),
                "quality": channel.quality,
                "measurement": channel.measurement,
                "wav": artifact_json_by_role(&channel.artifacts, "capture_wav"),
                "ir": channel.irs.iter().map(ir_json).collect::<Vec<Value>>(),
                "artifacts": artifacts_json(&channel.artifacts),
            }))
            .collect::<Vec<Value>>(),
        "artifacts": artifacts_json(&artifacts),
        "ir": irs.iter().map(ir_json).collect::<Vec<Value>>(),
        "delta_vs_previous": delta,
        "verdict": if simulated { "SIMULATED_MEASURED" } else { "MEASURED" },
        "reasons": {
            "per_channel_sessions": "each declared channel gets its own sweep/capture session plus its own T62 timebase and quality gates (single-output sessions keep the robust noise floor and onset detection valid); the aggregate uses the worst channel",
            "simulated": if simulated {
                json!("offline synthetic engine: these numbers verify the wiring/quality gates, they are not room data")
            } else {
                Value::Null
            },
        },
    });
    fs::create_dir_all(session_dir).map_err(|error| {
        CalFailure::new(
            error::ARTIFACT_IO,
            format!("create artifact dir {}: {error}", session_dir.display()),
        )
    })?;
    write_json_file(session_dir, "measurement.json", &result)
        .and_then(|()| write_json_file(session_dir, "summary.json", &summary.to_json()))
        .map_err(|error| CalFailure::new(error::ARTIFACT_IO, error))?;
    Ok(DonePayload {
        summary,
        last,
        result,
    })
}

/// 产物清单写盘（raw / wav / timeline / capture-report / summary）。
fn write_capture_artifacts(
    session_dir: &Path,
    channel: u32,
    output: &MeasurementOutput,
    capture_channels: u16,
    sample_rate: u32,
    artifacts: &mut Vec<ArtifactFile>,
) -> Result<(), String> {
    fs::create_dir_all(session_dir)
        .map_err(|error| format!("create artifact dir {}: {error}", session_dir.display()))?;
    artifacts.push(write_bytes(
        session_dir,
        "capture_raw",
        "capture.raw",
        &output.raw,
        Some(channel),
    )?);
    let wav = encode_wav_pcm24_interleaved(&output.raw, capture_channels, sample_rate);
    artifacts.push(write_bytes(
        session_dir,
        "capture_wav",
        "capture.wav",
        &wav,
        Some(channel),
    )?);
    artifacts.push(write_bytes(
        session_dir,
        "timeline",
        "timeline.json",
        output.timeline_json.as_bytes(),
        Some(channel),
    )?);
    artifacts.push(write_bytes(
        session_dir,
        "capture_report",
        "capture-report.json",
        output.capture_report_json.as_bytes(),
        Some(channel),
    )?);
    artifacts.push(write_bytes(
        session_dir,
        "summary",
        "summary.json",
        output.summary.to_json().as_bytes(),
        Some(channel),
    )?);
    Ok(())
}

#[derive(Clone)]
struct IrArtifact {
    label: String,
    slot: u32,
    playback_channel: u32,
    channel: u32,
    samples: usize,
    peak_index: usize,
    sample_rate_hz: u32,
    repeats_averaged: u32,
    path: PathBuf,
    sha256: String,
    artifact: ArtifactFile,
    /// 计算 design 用的 IR 样本（f64；不重复读盘）。
    ir: Vec<f64>,
}

fn write_ir_artifacts(
    session_dir: &Path,
    channel: u32,
    irs: &[DeconvolvedIr],
) -> Result<Vec<IrArtifact>, String> {
    let ir_dir = session_dir.join("ir");
    fs::create_dir_all(&ir_dir)
        .map_err(|error| format!("create IR dir {}: {error}", ir_dir.display()))?;
    let mut artifacts = Vec::with_capacity(irs.len());
    let mut index = Vec::with_capacity(irs.len());
    for ir in irs {
        let mut bytes = Vec::with_capacity(ir.ir.len() * 4);
        for sample in &ir.ir {
            bytes.extend_from_slice(&(*sample as f32).to_le_bytes());
        }
        let name = format!("{}.f32", sanitize_label(&ir.label));
        let artifact = write_bytes(&ir_dir, "ir", &name, &bytes, Some(channel))?;
        index.push(json!({
            "label": ir.label,
            "channel": channel,
            "slot": ir.slot,
            "playback_channel": ir.playback_channel,
            "samples": ir.ir.len(),
            "peak_index": ir.peak_index,
            "sample_rate_hz": ir.sample_rate_hz,
            "repeats_averaged": ir.repeats_averaged,
            "tail_s": json_f64(ir.tail_s),
            "artifact": artifact.path.display().to_string(),
            "sha256": artifact.sha256,
        }));
        artifacts.push(IrArtifact {
            label: ir.label.clone(),
            slot: ir.slot,
            playback_channel: ir.playback_channel,
            channel,
            samples: ir.ir.len(),
            peak_index: ir.peak_index,
            sample_rate_hz: ir.sample_rate_hz,
            repeats_averaged: ir.repeats_averaged,
            path: artifact.path.clone(),
            sha256: artifact.sha256.clone(),
            artifact,
            ir: ir.ir.clone(),
        });
    }
    write_json_file(&ir_dir, "ir-index.json", &Value::Array(index))?;
    Ok(artifacts)
}

fn ir_artifact_to_ir(artifact: &IrArtifact) -> DeconvolvedIr {
    DeconvolvedIr {
        label: artifact.label.clone(),
        slot: artifact.slot,
        playback_channel: artifact.playback_channel,
        ir: artifact.ir.clone(),
        sample_rate_hz: artifact.sample_rate_hz,
        peak_index: artifact.peak_index,
        repeats_averaged: artifact.repeats_averaged,
        tail_s: IrExtractConfig::default().tail_s,
        sweep_start_hz: LEGACY_SWEEP_START_HZ,
        sweep_end_hz: LEGACY_SWEEP_END_HZ,
        sweep_seconds: LEGACY_SWEEP_SECONDS,
    }
}

fn ir_json(ir: &IrArtifact) -> Value {
    json!({
        "label": ir.label,
        "channel": ir.channel,
        "slot": ir.slot,
        "playback_channel": ir.playback_channel,
        "samples": ir.samples,
        "peak_index": ir.peak_index,
        "sample_rate_hz": ir.sample_rate_hz,
        "repeats_averaged": ir.repeats_averaged,
        "path": ir.path.display().to_string(),
        "sha256": ir.sha256,
    })
}

fn artifact_json_by_role(artifacts: &[ArtifactFile], role: &str) -> Value {
    match artifacts.iter().find(|file| file.role == role) {
        Some(file) => file.to_json(),
        None => Value::Null,
    }
}

fn artifacts_json(artifacts: &[ArtifactFile]) -> Value {
    Value::Array(
        artifacts
            .iter()
            .map(|file| file.to_json())
            .collect::<Vec<Value>>(),
    )
}

/// 单通道质量指标 JSON（T62/T67 报告里的真实值；不可估计为 null）。
fn quality_json(
    summary: &khaen_calibration::stage_backend::MeasurementSummary,
    clock: &ClockSummary,
) -> Value {
    // 截断判据：采集覆盖 seed + 时间轴（成功路径已由 T67 E_CAPTURE_INCOMPLETE 把关，这里如实写出来）。
    let capture_complete =
        summary.capture_frames as f64 + summary.seed_render_frame >= summary.timeline_frames as f64;
    json!({
        "snr_db": json_opt_f64(summary.snr_db),
        "capture_complete": capture_complete,
        "truncated": !capture_complete,
        "capture_peak_dbfs": json_f64(summary.capture_peak_dbfs),
        "clipped_samples": summary.clipped_samples,
        "nonfinite_samples": summary.nonfinite_samples,
        "xruns": summary.xruns,
        "underruns": summary.underruns,
        "capture_frames": summary.capture_frames,
        "timeline_frames": summary.timeline_frames,
        "valid_slots": summary.valid_slots,
        "total_slots": summary.total_slots,
        "all_onsets_valid": summary.all_onsets_valid,
        "seed_render_frame": json_f64(summary.seed_render_frame),
        "noise_floor_dbfs": json_opt_f64(summary.noise_floor_dbfs),
        "clock_ppm": json_opt_f64(clock.clock_ppm),
        "clock_ppm_uncertainty_ppm": json_opt_f64(clock.clock_ppm_uncertainty_ppm),
        "drift_significant": clock.drift_significant,
        "clock_ppm_ok": clock.clock_ppm_ok,
        "notation": "clock_ppm/uncertainty come from the T62 capture timebase (UMIK independent USB clock; MOTU electrical loopback is a separate measurement)",
    })
}

/// 聚合质量 JSON（逐通道 + 最差通道口径；不平均掉问题通道）。
fn aggregate_quality_json(channels: &[ChannelMeasurement], summary: &CalSummary) -> Value {
    json!({
        "channels": channels.iter().map(|channel| json!({
            "channel": channel.channel,
            "snr_db": json_opt_f64(channel.summary.snr_db),
            "capture_peak_dbfs": json_f64(channel.summary.capture_peak_dbfs),
            "clipped_samples": channel.summary.clipped_samples,
            "xruns": channel.summary.xruns,
            "valid_slots": channel.summary.valid_slots,
            "total_slots": channel.summary.total_slots,
            "all_onsets_valid": channel.summary.all_onsets_valid,
            "clock_ppm": json_opt_f64(channel.clock.clock_ppm),
            "clock_ppm_uncertainty_ppm": json_opt_f64(channel.clock.clock_ppm_uncertainty_ppm),
            "drift_significant": channel.clock.drift_significant,
            "clock_ppm_ok": channel.clock.clock_ppm_ok,
        })).collect::<Vec<Value>>(),
        "min_snr_db": json_opt_f64(summary.snr_db),
        "max_capture_peak_dbfs": json_f64(summary.capture_peak_dbfs),
        "clipped_samples": summary.clipped_samples,
        "xruns": summary.xruns,
        "clock_ppm": json_opt_f64(summary.clock_ppm),
        "clock_ppm_uncertainty_ppm": json_opt_f64(summary.clock_ppm_uncertainty_ppm),
        "all_onsets_valid": channels.iter().all(|channel| channel.summary.all_onsets_valid),
        "notation": "aggregate SNR = worst channel (null if any channel could not estimate it); clock ppm = median over channels (null unless every channel estimated it); UMIK is an independent USB clock, so clock_ppm + uncertainty are required",
    })
}

/// 单通道的 T67 measurement 摘要 JSON（SNR 估计/seed/notes）。
fn measurement_json(
    summary: &khaen_calibration::stage_backend::MeasurementSummary,
    output: &MeasurementOutput,
    channel: u32,
) -> Value {
    json!({
        "channel": channel,
        "capture_peak_dbfs": json_f64(summary.capture_peak_dbfs),
        "noise_floor_dbfs": json_opt_f64(summary.noise_floor_dbfs),
        "snr_db": json_opt_f64(summary.snr_db),
        "snr_gate": serde_json::from_str::<Value>(&match &summary.snr {
            Some(report) => report.to_json(),
            None => "null".to_string(),
        }).unwrap_or(Value::Null),
        "seed_render_frame": json_f64(summary.seed_render_frame),
        "all_onsets_valid": summary.all_onsets_valid,
        "raw_relative_path": output.raw_relative_path,
        "notes": summary.notes,
    })
}

fn delta_json(previous: &CalSummary, current: &CalSummary) -> Value {
    json!({
        "previous_measure_id": previous.measure_id,
        "previous_pass": previous.pass,
        "snr_db_delta": match (previous.snr_db, current.snr_db) {
            (Some(a), Some(b)) => json_f64(b - a),
            _ => Value::Null,
        },
        "capture_peak_dbfs_delta": json_f64(current.capture_peak_dbfs - previous.capture_peak_dbfs),
        "clock_ppm_delta": match (previous.clock_ppm, current.clock_ppm) {
            (Some(a), Some(b)) => json_f64(b - a),
            _ => Value::Null,
        },
    })
}

struct ClockSummary {
    clock_ppm: Option<f64>,
    clock_ppm_uncertainty_ppm: Option<f64>,
    drift_significant: Option<bool>,
    clock_ppm_ok: Option<bool>,
}

fn clock_from_capture_report(report_json: &str) -> ClockSummary {
    let value: Value = serde_json::from_str(report_json).unwrap_or(Value::Null);
    let clock = value
        .get("timebase")
        .and_then(|timebase| timebase.get("clock"))
        .cloned()
        .unwrap_or(Value::Null);
    ClockSummary {
        clock_ppm: clock.get("clock_ppm").and_then(Value::as_f64),
        clock_ppm_uncertainty_ppm: clock
            .get("clock_ppm_uncertainty_ppm")
            .and_then(Value::as_f64),
        drift_significant: clock.get("drift_significant").and_then(Value::as_bool),
        clock_ppm_ok: clock.get("clock_ppm_ok").and_then(Value::as_bool),
    }
}

/// 测量 artifact schema。
pub const MEASUREMENT_SCHEMA: &str = "khaen-rt-host.cal-measurement/v1";

#[derive(Clone)]
struct ArtifactFile {
    role: &'static str,
    path: PathBuf,
    bytes: u64,
    sha256: String,
    /// 本 artifact 属于哪个测量通道（多通道逐 session 测量）。
    channel: Option<u32>,
}

impl ArtifactFile {
    fn to_json(&self) -> Value {
        json!({
            "role": self.role,
            "channel": self.channel,
            "path": self.path.display().to_string(),
            "bytes": self.bytes,
            "sha256": self.sha256,
        })
    }
}

fn write_bytes(
    dir: &Path,
    role: &'static str,
    name: &str,
    bytes: &[u8],
    channel: Option<u32>,
) -> Result<ArtifactFile, String> {
    let path = dir.join(name);
    fs::write(&path, bytes).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(ArtifactFile {
        role,
        path,
        bytes: bytes.len() as u64,
        sha256: khaen_host::sha256_hex(bytes),
        channel,
    })
}

fn write_json_file(dir: &Path, name: &str, value: &Value) -> Result<(), String> {
    let path = dir.join(name);
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

fn write_error_artifact(
    session_dir: &Path,
    failure: &CalFailure,
    artifacts: Option<&Vec<ArtifactFile>>,
) -> Result<(), String> {
    if let Err(error) = fs::create_dir_all(session_dir) {
        return Err(format!(
            "create artifact dir {}: {error}",
            session_dir.display()
        ));
    }
    let value = json!({
        "ok": false,
        "schema": "khaen-rt-host.cal-error/v1",
        "error": failure.to_json(),
        "artifacts_written": artifacts.map(|artifacts| artifacts_json(artifacts)).unwrap_or(Value::Null),
    });
    write_json_file(session_dir, "error.json", &value)
}

fn sanitize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for character in label.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "channel".to_string()
    } else {
        out
    }
}
