//! [NONRT] solver ingest（T48） ——WBS T48 写域。
//!
//! 本模块是 P1 低频 MIMO 设计链的**只读数据入口**：把新校准 daemon 已经落盘的
//! raw / source / diag / position 产物读成强类型、校验过的 [IngestBundle]，供 T49
//! （timebase/相位重建）、T50（目标）、T51–T54（solver/regularize/validate/artifact）
//! 与 T73/T6 证据链消费。本模块**不采音**（不打开任何音频设备、不 spawn 任何进程）、
//! **不写盘**、**不做 timebase 数学**（coarse/frac/ppm 反演归 T49）、**不做解卷积**。
//!
//! 权威输入（只读）：
//! - WBS T48 行与 §1/§6/§7（scratch/lead/planner/TSPI-RT-STACK-WBS.md）；
//! - DSP-HOST-DECISION §2.1–§2.3（新 daemon 保存 raw/source/slot/repeat/公共时轴；UMIK 独立 USB ADC 1ch；
//!   MOTU 可选电学 reference 是另一独立通道；缺有效时轴不能产可应用矩阵）；
//! - docs/MIMO-COEFF-PLAN.md §2.3（session/positions 目录与 position label/role/weight）与 §2.4a（相位语义）；
//! - rt-stack/spec/mimo-t6.md §2.1/§3.2/§3.4/FC-05/FC-06/FC-08（公共 render frame/slot、mic clock、重复）；
//! - rt-stack/spec/core-v1.md §3.1/§3.2（容量与采样率常量）与 §12.1/§12.2（本 crate 无 RT 区、[NONRT]）；
//! - rt-stack/spec/compat-matrix.md Y23/Y24（数值/单位校验、artifact 版本/hash/stale 精神）；
//! - calibration-spec/CONTRACT.md §2/§3（source 分析产物复用 measure-multi 的 schema/字段语义）。
//!
//! # 输入目录契约（tspi-modal-ingest/v1，本模块冻结）
//!
//! ~~~text
//! <session_dir>/
//!   session.json                 schema tspi-mimo-measurement/v1（必需）
//!   SHA256SUMS                   "<64 lowercase hex>  <relative path>"，逐行（必需；覆盖本模块读取的全部文件）
//!   positions/
//!     <position_dir>/
//!       position.json            schema tspi-mimo-position/v1（label/role/weight 必填）
//!       capture.raw              UMIK 独立 USB ADC 1ch 采集（原样，不重采样；必需）
//!       reference.raw            MOTU 可选电学 reference，另一独立通道（session.reference 声明时必需）
//!       <SPK>-ch<NN>.analysis.json   source：measure-multi 兼容分析产物（每 participant 必需）
//!       <SPK>-ch<NN>.diag.json       diag：公共 render frame/slot/mic-clock 时轴（每 participant 必需）
//! ~~~
//!
//! - <NN> 是零填充的设备通道号（speaker.out_index，0..21，与样例 FL-ch00.analysis.json 一致）。
//! - capture.raw 是 1ch、S24_3LE/S32_LE/F32_LE 的原始采集；文件大小必须严格等于
//!   frames × channels × bytes_per_sample，且所有扫频槽必须落在采集窗口内。
//! - session.json 声明布局/采样率/participants/speakers/mic/playback/capture/stimulus/室温/tool；
//!   participants 是显式低频参与角色集合（**不从 role 名、HP/LP 或频响推断**）。
//! - position.json 的 name/role（design|validation）/weight（>0 有限）必填；坐标/麦克风朝向可选。
//! - diag.json 的公共时轴字段（缺任一即**拒交付**，E_INGEST_TIMEBASE）：
//!   session_id/epoch_id/graph_id（64 hex，非全零）/speaker/channel/slot/capture_file/
//!   render_frame_index/capture_sample_index/nominal_start/actual_start/lag_samples/
//!   repeat_starts（长度 = repeats = stimulus.repeats）/clock（clock_ppm/
//!   clock_ppm_uncertainty_ppm/drift_significant/clock_ppm_ok/correction）/onset_invalid。
//!   clock_ppm_ok=false、onset_invalid=true、或显著漂移未声明校正 → 拒交付。
//! - 同一 position 的全部 diag 必须共享同一个 capture_file、同一个 session_id/epoch_id/graph_id，
//!   且 slot 覆盖 0..participants.len()、render_frame_index 与 capture_sample_index 随 slot 严格递增。
//!   全部 position 必须共享同一 session_id/epoch_id/graph_id（公共 render frame 基准；跨 position
//!   不复用 capture 属正常，时间轴重建由 T49 完成）。
//! - analysis.json：schema tspi-channel-measurement/v1 + meta schema khaen-calibration-meta/v1；
//!   必须与 session 的 fs/设备/通道/sweep 参数一致，channel == speaker.out_index，FR 频点有限且严格递增，
//!   freq_hz ∈ (0, fs/2]。onset_invalid=true / clock_ppm_ok=false / clock_ppm_significant=true 且
//!   drift_corrected!=true → 拒交付。
//! - SHA256SUMS 覆盖本模块读取的全部文件（session/position/diag/analysis/capture/reference）；
//!   缺失清单项 → E_INGEST_MISSING，字节不符 → E_INGEST_HASH。清单只允许相对路径（拒绝绝对路径/.. /反斜杠）。
//! - 未知字段只产生 warning（保留扩展性）；重复 JSON key、类型不符、越界、NaN/Inf（JSON 非法）、
//!   重复 label/路径/participant/slot、schema 不符 → 拒绝。
//!
//! # 错误码（稳定，供 T49+ 与证据链引用）
//!
//! | code | kind | 语义 |
//! | --- | --- | --- |
//! | E_INGEST_IO | Io | 目录/文件不可读（权限、非普通文件） |
//! | E_INGEST_JSON | Json | JSON 语法/编码/重复 key/非法数值 |
//! | E_INGEST_SCHEMA | Schema | schema 不匹配、字段类型/枚举/标签不一致 |
//! | E_INGEST_MISSING | Missing | 必需文件/字段/参与者测量缺失 |
//! | E_INGEST_DUPLICATE | Duplicate | 重复 label/名称/通道/清单路径 |
//! | E_INGEST_RANGE | Range | 数值越界/非整数/超出容量 |
//! | E_INGEST_TIMEBASE | Timebase | 公共时轴缺失或自相矛盾（render frame/slot/mic clock） |
//! | E_INGEST_HASH | Hash | SHA256SUMS 缺失项/哈希不符/非法 hex |
//! | E_INGEST_UNSUPPORTED | Unsupported | 不支持的采样率/格式/1ch 以外的 UMIK 采集/旧 producer |
//! | E_INGEST_AUDIO | Audio | raw 文件大小与声明格式不符、扫频槽越界 |
//!
//! # 写域与不变量
//!
//! - 只读：本文件不含任何写盘调用（单测静态守卫）。
//! - 不采音：本文件不引用任何进程/音频设备 API（单测静态守卫）。
//! - 仅 [NONRT] 控制面使用；可安全在 daemon 非 RT 线程调用，禁止在 RT 区调用。
//! - 本文件不注册公共 mod/CLI、不改 Cargo.toml；由 T06 integrator 决定公共注册。

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use khaen_core::rt::{
    MAX_INPUT_CHANNELS, MAX_LOW_BAND_PARTICIPANTS, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ,
};

/// session 清单 schema（本模块冻结）。
pub const SESSION_SCHEMA: &str = "tspi-mimo-measurement/v1";
/// position 清单 schema（本模块冻结）。
pub const POSITION_SCHEMA: &str = "tspi-mimo-position/v1";
/// 公共时轴 diag schema（本模块冻结）。
pub const DIAG_SCHEMA: &str = "tspi-mimo-diag/v1";
/// source 分析产物 schema（复用 khaen-calibrate measure-multi；只读兼容）。
pub const ANALYSIS_SCHEMA: &str = "tspi-channel-measurement/v1";
/// source 分析产物 meta schema（复用 khaen-calibrate measure-multi；只读兼容）。
pub const ANALYSIS_META_SCHEMA: &str = "khaen-calibration-meta/v1";
/// 输入文件 hash 清单文件名。
pub const SHA256SUMS_FILE: &str = "SHA256SUMS";
/// session 清单文件名。
pub const SESSION_FILE: &str = "session.json";
/// position 目录名。
pub const POSITIONS_DIR: &str = "positions";
/// position 清单文件名。
pub const POSITION_FILE: &str = "position.json";
/// UMIK 采集 raw 文件名（每个 position 一个）。
pub const CAPTURE_FILE: &str = "capture.raw";
/// MOTU 可选电学 reference raw 文件名（session.reference 声明时必需）。
pub const REFERENCE_FILE: &str = "reference.raw";

const MAX_SESSION_BYTES: u64 = 256 * 1024;
const MAX_POSITION_BYTES: u64 = 64 * 1024;
const MAX_DIAG_BYTES: u64 = 256 * 1024;
const MAX_ANALYSIS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SHA256SUMS_BYTES: u64 = 1024 * 1024;
const MAX_POSITIONS: usize = 32;
const MAX_MANIFEST_ENTRIES: usize = 2048;
const MAX_TOTAL_INPUT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_FR_POINTS: usize = 8192;
const MAX_JSON_DEPTH: usize = 64;
const MAX_WARNINGS: usize = 256;

/// ingest 错误种类（机器码见 [IngestErrorKind::code]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestErrorKind {
    /// 目录/文件不可读。
    Io,
    /// JSON 语法/编码/重复 key/非法数值。
    Json,
    /// schema/字段类型/枚举/标签不一致。
    Schema,
    /// 必需文件/字段/测量缺失。
    Missing,
    /// 重复 label/名称/通道/清单路径。
    Duplicate,
    /// 数值越界/非整数/超容量。
    Range,
    /// 公共时轴缺失或自相矛盾。
    Timebase,
    /// hash 清单缺失项/不符/非法 hex。
    Hash,
    /// 不支持的采样率/格式/采集形态/producer。
    Unsupported,
    /// raw 大小/扫频槽与声明格式矛盾。
    Audio,
}

impl IngestErrorKind {
    /// 稳定机器码（证据/报告引用）。
    pub const fn code(self) -> &'static str {
        match self {
            IngestErrorKind::Io => "E_INGEST_IO",
            IngestErrorKind::Json => "E_INGEST_JSON",
            IngestErrorKind::Schema => "E_INGEST_SCHEMA",
            IngestErrorKind::Missing => "E_INGEST_MISSING",
            IngestErrorKind::Duplicate => "E_INGEST_DUPLICATE",
            IngestErrorKind::Range => "E_INGEST_RANGE",
            IngestErrorKind::Timebase => "E_INGEST_TIMEBASE",
            IngestErrorKind::Hash => "E_INGEST_HASH",
            IngestErrorKind::Unsupported => "E_INGEST_UNSUPPORTED",
            IngestErrorKind::Audio => "E_INGEST_AUDIO",
        }
    }
}

/// ingest 错误：kind + 逻辑上下文（如 session.json.stimulus.repeats）+ 可选文件路径 + 细节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestError {
    kind: IngestErrorKind,
    context: String,
    path: Option<PathBuf>,
    detail: String,
}

impl IngestError {
    fn new(kind: IngestErrorKind, context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError {
            kind,
            context: context.into(),
            path: None,
            detail: detail.into(),
        }
    }

    fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn io(context: impl Into<String>, error: impl fmt::Display) -> Self {
        IngestError::new(IngestErrorKind::Io, context, error.to_string())
    }

    fn json(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Json, context, detail)
    }

    fn schema(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Schema, context, detail)
    }

    fn missing(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Missing, context, detail)
    }

    fn duplicate(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Duplicate, context, detail)
    }

    fn range(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Range, context, detail)
    }

    fn timebase(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Timebase, context, detail)
    }

    fn hash(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Hash, context, detail)
    }

    fn unsupported(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Unsupported, context, detail)
    }

    fn audio(context: impl Into<String>, detail: impl Into<String>) -> Self {
        IngestError::new(IngestErrorKind::Audio, context, detail)
    }

    /// 错误种类。
    pub fn kind(&self) -> IngestErrorKind {
        self.kind
    }

    /// 稳定机器码。
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// 逻辑上下文（字段路径或文件角色）。
    pub fn context(&self) -> &str {
        &self.context
    }

    /// 涉及的输入文件（若有）。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 人读细节。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.context)?;
        if let Some(path) = &self.path {
            write!(f, " [{}]", path.display())?;
        }
        write!(f, ": {}", self.detail)
    }
}

impl std::error::Error for IngestError {}

/// 非致命输入问题（不确定项只报告，不阻塞交付）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestWarning {
    /// 稳定 code：W_INGEST_UNKNOWN_FIELD / W_INGEST_LOW_REPEATS / W_INGEST_IGNORED_FILE。
    pub code: &'static str,
    /// 人读说明。
    pub message: String,
}

impl IngestWarning {
    const UNKNOWN_FIELD: &'static str = "W_INGEST_UNKNOWN_FIELD";
    const LOW_REPEATS: &'static str = "W_INGEST_LOW_REPEATS";
    const IGNORED_FILE: &'static str = "W_INGEST_IGNORED_FILE";
}

/// 测点角色（显式标签，不从名称/坐标推断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionRole {
    /// 设计点（参与拟合；不得用于 holdout 评估）。
    Design,
    /// 验证/holdout 点（不得参与调参）。
    Validation,
}

impl PositionRole {
    /// 机器名（design/validation）。
    pub const fn as_str(self) -> &'static str {
        match self {
            PositionRole::Design => "design",
            PositionRole::Validation => "validation",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "design" => Some(PositionRole::Design),
            "validation" => Some(PositionRole::Validation),
            _ => None,
        }
    }
}

impl fmt::Display for PositionRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 采样格式（只接受本栈部署口径的三种原始格式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// S24_3LE（3 字节 packed 24-bit，现有 MOTU/UMIK 采集默认）。
    S24_3LE,
    /// S32_LE（4 字节有符号 32-bit）。
    S32LE,
    /// F32_LE（4 字节 IEEE-754 binary32）。
    F32LE,
}

impl SampleFormat {
    /// 机器名（与 diag/清单字符串一致）。
    pub const fn as_str(self) -> &'static str {
        match self {
            SampleFormat::S24_3LE => "S24_3LE",
            SampleFormat::S32LE => "S32_LE",
            SampleFormat::F32LE => "F32_LE",
        }
    }

    /// 每样本字节数。
    pub const fn bytes_per_sample(self) -> u64 {
        match self {
            SampleFormat::S24_3LE => 3,
            SampleFormat::S32LE | SampleFormat::F32LE => 4,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "S24_3LE" => Some(SampleFormat::S24_3LE),
            "S32_LE" => Some(SampleFormat::S32LE),
            "F32_LE" => Some(SampleFormat::F32LE),
            _ => None,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 显著时钟漂移的校正声明（FC-06：只有 |ppm|>2σ 才算漂移并允许校正）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftCorrection {
    /// 未做/无需校正。
    None,
    /// 已按声明的 ppm 重采样校正。
    Resample,
}

impl DriftCorrection {
    /// 机器名（none/resample）。
    pub const fn as_str(self) -> &'static str {
        match self {
            DriftCorrection::None => "none",
            DriftCorrection::Resample => "resample",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(DriftCorrection::None),
            "resample" | "resample_sinc" => Some(DriftCorrection::Resample),
            _ => None,
        }
    }
}

impl fmt::Display for DriftCorrection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个 FR 点（freq_hz 单调递增；dB/deg 均有限）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrPoint {
    /// 频率（Hz，>0 且 ≤ fs/2）。
    pub freq_hz: f64,
    /// 幅度（dB）。
    pub db: f64,
    /// 相位（deg；语义由 analysis.phase_ref 声明，本模块不解释）。
    pub phase_deg: f64,
}

/// 扬声器/输出角色声明（participants 从本表显式引用）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerInfo {
    /// 角色 id（^[A-Z][A-Z0-9]*$，与校准/布局唯一 role id 约定一致）。
    pub name: String,
    /// 播放设备通道号（0-based，< MAX_OUTPUT_CHANNELS）。
    pub out_index: u32,
    /// 描述性类别（如 main/surround/subwoofer/height；不参与任何算法推断）。
    pub kind: String,
    /// 允许的低频带（Hz，可选；显式声明，不由 role 名推断）。
    pub allowed_band_hz: Option<(f64, f64)>,
}

/// 麦克风声明（P1 必须是独立 USB ADC 单通道 UMIK）。
#[derive(Debug, Clone, PartialEq)]
pub struct MicInfo {
    /// 型号（如 UMIK-1）。
    pub model: String,
    /// 序列号。
    pub serial: String,
    /// miniDSP 校准文件字节 sha256（64 lowercase hex，非全零）。
    pub cal_sha256: String,
    /// 朝向声明（如 90deg/0deg；P1 多声道听音位默认 90°）。
    pub orientation: String,
    /// 时钟域；P1 ingest 只接受 usb（UMIK 是独立 USB ADC，不与 MOTU 同源）。
    pub clock_domain: String,
}

/// 播放侧声明。
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackInfo {
    /// ALSA 设备串（如 hw:motu,0；稳定 ID 语义由 host/daemon 保证）。
    pub device: String,
    /// 播放通道数（1..=MAX_OUTPUT_CHANNELS）。
    pub channels: u16,
    /// 输出样本格式。
    pub sample_format: SampleFormat,
}

/// UMIK 采集侧声明（P1 必须 1ch）。
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureInfo {
    /// ALSA 设备串（如 hw:umik1,0）。
    pub device: String,
    /// 采集通道数；P1 必须为 1。
    pub channels: u16,
    /// 采集样本格式。
    pub sample_format: SampleFormat,
    /// 扫频前静音保护（秒）。
    pub pre_seconds: f64,
    /// 扫频后静音保护（秒）。
    pub post_seconds: f64,
}

/// MOTU 可选电学 reference（T49 使用；必须是另一独立通道/设备，不是 UMIK）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceInfo {
    /// ALSA 设备串（必须与 capture.device 不同）。
    pub device: String,
    /// 通道数（1..=MAX_INPUT_CHANNELS）。
    pub channels: u16,
    /// 样本格式。
    pub sample_format: SampleFormat,
    /// 必须是独立通道（本栈只接受 true）。
    pub independent: bool,
}

/// 扫频/激励声明（design/off/on 各处应一致；FC-11）。
#[derive(Debug, Clone, PartialEq)]
pub struct StimulusInfo {
    /// 激励类型（log_sweep/linear_sweep/noise/ir）。
    pub kind: String,
    /// 起始频率（Hz）。
    pub start_hz: f64,
    /// 结束频率（Hz，≤ fs/2）。
    pub end_hz: f64,
    /// 单遍时长（秒）。
    pub seconds: f64,
    /// 数字电平（dBFS，≤0）。
    pub level_dbfs: f64,
    /// fade 时长（ms）。
    pub fade_ms: f64,
    /// 每槽重复次数（1..=8；<3 给 warning，质量门由 T60/T63/T73 判定）。
    pub repeats: u32,
    /// 遍间静音（秒）。
    pub repeat_gap_seconds: f64,
}

/// 工具/版本声明（状态/证据链追溯）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    /// 工具名（如 tspi-audio-daemon）。
    pub name: String,
    /// 工具版本。
    pub version: String,
}

/// session.json 解析结果（schema tspi-mimo-measurement/v1）。
#[derive(Debug, Clone, PartialEq)]
pub struct IngestSession {
    /// session 唯一 id（进入所有 diag/报告的公共时轴）。
    pub session_id: String,
    /// 目标布局 id（如 4.1.2；与 layout-presets id 一致，实际解析归 T57）。
    pub layout: String,
    /// 采样率（48k/96k，来自 core-v1 §3.2）。
    pub sample_rate_hz: u32,
    /// render block 帧数（可选；必须 ∈ {64,128}）。
    pub block_frames: Option<u16>,
    /// 低频 MIMO 显式参与角色（有序；≤8；不从角色名推断）。
    pub participants: Vec<String>,
    /// 全部扬声器/输出角色声明。
    pub speakers: Vec<SpeakerInfo>,
    /// UMIK 声明。
    pub mic: MicInfo,
    /// 播放侧声明。
    pub playback: PlaybackInfo,
    /// UMIK 采集侧声明。
    pub capture: CaptureInfo,
    /// MOTU 可选电学 reference。
    pub reference: Option<ReferenceInfo>,
    /// 激励声明。
    pub stimulus: StimulusInfo,
    /// 室温（摄氏度；FC-10）。
    pub room_temperature_c: f64,
    /// 工具/版本。
    pub tool: ToolInfo,
    /// 创建时刻（unix 秒，可选）。
    pub created_unix: Option<u64>,
    /// 人读 claim/备注（可选）。
    pub claim: Option<String>,
}

impl IngestSession {
    /// 按 participants 顺序返回参与角色声明（顺序即 dense 索引 0..M-1）。
    pub fn participant_speakers(&self) -> Vec<&SpeakerInfo> {
        self.participants
            .iter()
            .filter_map(|name| self.speakers.iter().find(|s| &s.name == name))
            .collect()
    }

    /// 参与角色对应的设备通道号（dense 索引 → DAC 槽位；4.1.2 例：0,1,2,3,7）。
    pub fn dense_to_out_index(&self) -> Vec<u32> {
        self.participant_speakers()
            .into_iter()
            .map(|s| s.out_index)
            .collect()
    }

    /// 某参与者声明的允许低频带（若有）。
    pub fn allowed_band_hz(&self, participant: &str) -> Option<(f64, f64)> {
        self.speakers
            .iter()
            .find(|s| s.name == participant)
            .and_then(|s| s.allowed_band_hz)
    }
}

/// position.json 解析结果（schema tspi-mimo-position/v1）。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionMeta {
    /// 测点标签（唯一；进入冻结清单/holdout 记录）。
    pub name: String,
    /// 显式角色（design/validation）。
    pub role: PositionRole,
    /// 设计/验证权重（有限、>0；归一化由 T52/T53 负责）。
    pub weight: f64,
    /// 粗略坐标（米，可选；候选 B 目标不要求）。
    pub coordinates_m: Option<[f64; 3]>,
    /// 麦克风朝向声明（可选）。
    pub mic_orientation: Option<String>,
    /// 创建时刻（unix 秒，可选）。
    pub created_unix: Option<u64>,
}

/// 一个已校验并绑定 hash 的 raw 采集文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFile {
    /// 相对 session 根的路径（/ 分隔）。
    pub relative_path: String,
    /// 文件字节 sha256（64 lowercase hex）。
    pub sha256: String,
    /// 文件字节数。
    pub bytes: u64,
    /// 帧数（= bytes / (channels×bytes_per_sample)）。
    pub frames: u64,
    /// 通道数。
    pub channels: u16,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 样本格式。
    pub sample_format: SampleFormat,
}

impl CaptureFile {
    /// 采集时长（秒）。
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / self.sample_rate_hz as f64
    }
}

/// 公共时轴 mic clock 段（FC-06 对齐 B2：ppm + uncertainty，2σ 判漂移）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicClock {
    /// 实测 UMIK 相对 MOTU/render 的 ppm（可为负）。
    pub clock_ppm: f64,
    /// ppm 不确定度（σ）。
    pub clock_ppm_uncertainty_ppm: f64,
    /// |ppm| > 2σ（只有此时允许/需要校正）。
    pub drift_significant: bool,
    /// 时轴估计是否有效（false → 拒交付）。
    pub clock_ppm_ok: bool,
    /// 校正声明。
    pub correction: DriftCorrection,
}

/// 一个参与者的公共时轴 diag（schema tspi-mimo-diag/v1）。
#[derive(Debug, Clone, PartialEq)]
pub struct TimebaseDiag {
    /// session id（必须等于 session.json）。
    pub session_id: String,
    /// host epoch id（公共 render frame 基准；>0）。
    pub epoch_id: u64,
    /// host graph id（64 lowercase hex，非全零）。
    pub graph_id: String,
    /// 角色 id（必须等于 participant）。
    pub speaker: String,
    /// 播放设备通道号（= speaker.out_index）。
    pub channel: u32,
    /// 本次扫频在会话中的槽位序号（0..participants.len()-1）。
    pub slot: u32,
    /// 采集文件名（本契约固定 capture.raw）。
    pub capture_file: String,
    /// 扫频起点对应的 host render frame 计数（公共 render frame）。
    pub render_frame_index: u64,
    /// 扫频起点在 UMIK capture 中的样本下标（公共 sample time）。
    pub capture_sample_index: u64,
    /// 名义槽起点（采集样本）。
    pub nominal_start: f64,
    /// 互相关实测起点（采集样本）。
    pub actual_start: f64,
    /// actual − nominal（采集样本）。
    pub lag_samples: f64,
    /// 每遍 sweep 起点（采集样本；长度 = repeats；严格递增）。
    pub repeat_starts: Vec<f64>,
    /// 重复次数（必须等于 stimulus.repeats）。
    pub repeats: u32,
    /// mic clock 段。
    pub clock: MicClock,
    /// 起点是否有效（true → 拒交付）。
    pub onset_invalid: bool,
}

/// source 分析产物的 meta（schema khaen-calibration-meta/v1，复用 measure-multi）。
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisMeta {
    /// 采样率。
    pub sample_rate_hz: u32,
    /// 播放通道数（meta.channels）。
    pub playback_channels: u16,
    /// sweep 起始频率。
    pub sweep_start_hz: f64,
    /// 该槽实际 sweep 结束频率（band-limit 时 ≤ stimulus.end_hz）。
    pub sweep_end_hz: f64,
    /// sweep 时长（秒）。
    pub sweep_seconds: f64,
    /// 电平（dBFS）。
    pub level_dbfs: f64,
    /// 前静音（秒）。
    pub pre_seconds: f64,
    /// 后静音（秒）。
    pub post_seconds: f64,
    /// 播放设备串。
    pub playback_device: String,
    /// 采集通道数。
    pub capture_channels: u16,
    /// 采集设备串。
    pub capture_device: String,
    /// 创建时刻（unix 秒）。
    pub created_unix: u64,
}

/// analysis 内的可选测点声明（老工具 measure-multi 的 meta.position）。
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisPosition {
    /// 测点名。
    pub name: String,
    /// 可选权重。
    pub weight: Option<f64>,
}

/// 单个 participant 的 source 分析结果（schema tspi-channel-measurement/v1）。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceAnalysis {
    /// 播放设备通道号（= speaker.out_index）。
    pub channel: u32,
    /// 实测传播延迟（样本，含尘埃项；T49 才做公共时轴/相位重建）。
    pub delay_samples: f64,
    /// 实测传播延迟（ms）。
    pub delay_ms: f64,
    /// 粗延迟（整数样本）。
    pub delay_coarse_samples: i64,
    /// 实测极性（-1/0/1；0=unknown，T62 质量门决定是否可交付）。
    pub polarity: i32,
    /// 峰值电平（dB）。
    pub peak_db: f64,
    /// 1 kHz 相位（deg）。
    pub phase_1k_deg: f64,
    /// 全带 SNR（dB）。
    pub snr_db: f64,
    /// 带限 SNR（dB，可选）。
    pub snr_band_db: Option<f64>,
    /// 带限频率范围（Hz，可选）。
    pub snr_band_hz: Option<(f64, f64)>,
    /// 相位参考语义串（必须非空；T49 解释）。
    pub phase_ref: String,
    /// 实测 ppm（可选；diag 为准）。
    pub clock_ppm: Option<f64>,
    /// 实测 ppm 是否有效（false → 拒交付）。
    pub clock_ppm_ok: Option<bool>,
    /// ppm 不确定度（可选）。
    pub clock_ppm_uncertainty_ppm: Option<f64>,
    /// |ppm|>2σ（true 且 drift_corrected!=true → 拒交付）。
    pub clock_ppm_significant: Option<bool>,
    /// 是否已做漂移校正。
    pub drift_corrected: Option<bool>,
    /// 起点是否无效（true → 拒交付）。
    pub onset_invalid: Option<bool>,
    /// 分析文件角色（reference 的产物不得作为 participant source）。
    pub measurement_role: Option<String>,
    /// 测点声明（若存在必须与 position.json 一致）。
    pub position: Option<AnalysisPosition>,
    /// 多遍次数（若存在必须等于 stimulus.repeats）。
    pub repeats_count: Option<u32>,
    /// 全带 FR 点（严格递增频率）。
    pub fr: Vec<FrPoint>,
    /// 模态用 1/48 oct FR 点（可选；存在时校验与 fr 同规则）。
    pub fr_modal: Vec<FrPoint>,
    /// meta。
    pub meta: AnalysisMeta,
}

/// 一个 participant 的完整 measurement（source + diag + 文件 hash）。
#[derive(Debug, Clone, PartialEq)]
pub struct IngestMeasurement {
    /// 角色 id。
    pub speaker: String,
    /// 播放设备通道号。
    pub out_index: u32,
    /// 槽位序号。
    pub slot: u32,
    /// analysis 文件 relative path。
    pub analysis_relative_path: String,
    /// analysis 文件 sha256。
    pub analysis_sha256: String,
    /// diag 文件 relative path。
    pub diag_relative_path: String,
    /// diag 文件 sha256。
    pub diag_sha256: String,
    /// source 分析。
    pub analysis: SourceAnalysis,
    /// 公共时轴 diag。
    pub diag: TimebaseDiag,
}

/// 一个 position（测点）的只读视图。
#[derive(Debug, Clone, PartialEq)]
pub struct IngestPosition {
    /// position 目录名（相对 positions/）。
    pub dir: String,
    /// 显式 label/role/weight。
    pub meta: PositionMeta,
    /// position.json sha256。
    pub meta_sha256: String,
    /// UMIK capture.raw（已校验）。
    pub capture: CaptureFile,
    /// MOTU 电学 reference（session.reference 声明时存在）。
    pub reference: Option<CaptureFile>,
    /// 各 participant measurement（按 slot 升序）。
    pub measurements: Vec<IngestMeasurement>,
}

impl IngestPosition {
    /// 该 position 的 participant 名（按 slot 顺序）。
    pub fn participant_names(&self) -> Vec<&str> {
        self.measurements
            .iter()
            .map(|m| m.speaker.as_str())
            .collect()
    }

    /// 找某 participant 的 measurement。
    pub fn measurement(&self, speaker: &str) -> Option<&IngestMeasurement> {
        self.measurements.iter().find(|m| m.speaker == speaker)
    }

    /// 设计点判定。
    pub fn is_design(&self) -> bool {
        self.meta.role == PositionRole::Design
    }

    /// 验证/holdout 点判定。
    pub fn is_validation(&self) -> bool {
        self.meta.role == PositionRole::Validation
    }
}

/// 输入 bundle 中的一个已校验文件记录（sha256 链）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestFileRecord {
    /// 相对 session 根路径（/ 分隔）。
    pub relative_path: String,
    /// 字节数。
    pub bytes: u64,
    /// 字节 sha256（64 lowercase hex）。
    pub sha256: String,
}

/// 一次成功 ingest 的完整只读结果。
#[derive(Debug, Clone, PartialEq)]
pub struct IngestBundle {
    /// session 根目录（调用方给出的路径）。
    pub root: PathBuf,
    /// session 声明。
    pub session: IngestSession,
    /// session.json sha256。
    pub session_sha256: String,
    /// 全部 position（按目录名字典序）。
    pub positions: Vec<IngestPosition>,
    /// 全部已校验输入文件（按路径字典序；含 SHA256SUMS 中未被 ingest 消费的附加文件）。
    pub files: Vec<IngestFileRecord>,
    /// 非致命 warning。
    pub warnings: Vec<IngestWarning>,
}

impl IngestBundle {
    /// 设计点（拟合用）迭代器。
    pub fn design_positions(&self) -> impl Iterator<Item = &IngestPosition> {
        self.positions.iter().filter(|p| p.is_design())
    }

    /// 验证/holdout 点迭代器。
    pub fn validation_positions(&self) -> impl Iterator<Item = &IngestPosition> {
        self.positions.iter().filter(|p| p.is_validation())
    }

    /// 设计点数。
    pub fn design_count(&self) -> usize {
        self.positions.iter().filter(|p| p.is_design()).count()
    }

    /// 验证点数。
    pub fn validation_count(&self) -> usize {
        self.positions.iter().filter(|p| p.is_validation()).count()
    }

    /// 设计点 measurement 数（design × participants）。
    pub fn design_measurement_count(&self) -> usize {
        self.design_positions().map(|p| p.measurements.len()).sum()
    }

    /// 找 position（按 label）。
    pub fn position(&self, name: &str) -> Option<&IngestPosition> {
        self.positions.iter().find(|p| p.meta.name == name)
    }

    /// 找 measurement（position label + participant）。
    pub fn measurement(&self, position: &str, speaker: &str) -> Option<&IngestMeasurement> {
        self.position(position).and_then(|p| p.measurement(speaker))
    }

    /// 会话声明的 dense → DAC 槽位表（4.1.2 例：[0, 1, 2, 3, 7]）。
    pub fn dense_to_out_index(&self) -> Vec<u32> {
        self.session.dense_to_out_index()
    }
}

// ===========================================================================
// JSON（内部最小严格解析器；无第三方依赖）
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

struct JsonParser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> JsonParser<'a> {
    fn parse(text: &'a str) -> Result<Json, IngestError> {
        let mut parser = JsonParser {
            src: text.as_bytes(),
            pos: 0,
            depth: 0,
        };
        if parser.src.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return Err(IngestError::json("json", "UTF-8 BOM is not allowed"));
        }
        parser.skip_ws();
        let value = parser.value()?;
        parser.skip_ws();
        if parser.pos != parser.src.len() {
            return Err(parser.error(format!(
                "trailing bytes after JSON value at offset {}",
                parser.pos
            )));
        }
        Ok(value)
    }

    fn error(&self, message: impl Into<String>) -> IngestError {
        IngestError::json("json", format!("{} (offset {})", message.into(), self.pos))
    }

    fn skip_ws(&mut self) {
        while let Some(byte) = self.src.get(self.pos) {
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8) -> Result<(), IngestError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(format!("expected '{}'", byte as char)))
        }
    }

    fn value(&mut self) -> Result<Json, IngestError> {
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(self.error(format!("nesting deeper than {MAX_JSON_DEPTH}")));
        }
        self.skip_ws();
        let value = match self.peek() {
            Some(b'{') => self.object()?,
            Some(b'[') => self.array()?,
            Some(b'"') => Json::String(self.string()?),
            Some(b't') => {
                self.literal(b"true")?;
                Json::Bool(true)
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Json::Bool(false)
            }
            Some(b'n') => {
                self.literal(b"null")?;
                Json::Null
            }
            Some(b'-') | Some(b'0'..=b'9') => self.number()?,
            Some(byte) => {
                return Err(self.error(format!("unexpected byte 0x{byte:02x} at JSON value start")))
            }
            None => return Err(self.error("unexpected end of JSON input")),
        };
        self.depth -= 1;
        Ok(value)
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), IngestError> {
        if self.src.get(self.pos..self.pos + word.len()) == Some(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(self.error("invalid JSON literal"))
        }
    }

    fn object(&mut self) -> Result<Json, IngestError> {
        self.expect(b'{')?;
        self.skip_ws();
        let mut entries: Vec<(String, Json)> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("object key must be a string"));
            }
            let key = self.string()?;
            if !seen.insert(key.clone()) {
                return Err(self.error(format!("duplicate object key {key:?}")));
            }
            self.skip_ws();
            self.expect(b':')?;
            let value = self.value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("expected ',' or '}' in object")),
            }
        }
        Ok(Json::Object(entries))
    }

    fn array(&mut self) -> Result<Json, IngestError> {
        self.expect(b'[')?;
        self.skip_ws();
        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("expected ',' or ']' in array")),
            }
        }
        Ok(Json::Array(values))
    }

    fn string(&mut self) -> Result<String, IngestError> {
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| self.error("unterminated string"))?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    self.pos += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated escape sequence"))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let code = self.hex4()?;
                            let scalar = if (0xD800..=0xDBFF).contains(&code) {
                                if !self.src[self.pos..].starts_with(b"\\u") {
                                    return Err(self.error("high surrogate without low surrogate"));
                                }
                                self.pos += 2;
                                let low = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(self.error("invalid low surrogate"));
                                }
                                0x10000 + (((code as u32 - 0xD800) << 10) | (low as u32 - 0xDC00))
                            } else if (0xDC00..=0xDFFF).contains(&code) {
                                return Err(self.error("unexpected low surrogate"));
                            } else {
                                code as u32
                            };
                            let ch = char::from_u32(scalar)
                                .ok_or_else(|| self.error("invalid unicode scalar"))?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => {
                            return Err(self
                                .error(format!("invalid escape sequence \\\\{}", other as char)))
                        }
                    }
                }
                0x00..=0x1F => {
                    return Err(self.error("raw control character in string"));
                }
                _ => {
                    out.push(byte);
                    self.pos += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.error("string is not valid UTF-8"))
    }

    fn hex4(&mut self) -> Result<u16, IngestError> {
        let bytes = self
            .src
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("truncated \\u escape"))?;
        let mut value: u16 = 0;
        for &byte in bytes {
            let nibble = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error("invalid hex digit in \\u escape")),
            };
            value = (value << 4) | nibble as u16;
        }
        self.pos += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, IngestError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("leading zero in number"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("fraction requires at least one digit"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("exponent requires at least one digit"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let slice = &self.src[start..self.pos];
        let text =
            std::str::from_utf8(slice).map_err(|_| self.error("number is not valid ASCII"))?;
        let value: f64 = text
            .parse()
            .map_err(|_| self.error("number does not parse as f64"))?;
        if !value.is_finite() {
            return Err(self.error("number overflows f64 (NaN/Inf are not valid JSON)"));
        }
        Ok(Json::Number(value))
    }
}

fn as_object<'a>(value: &'a Json, context: &str) -> Result<&'a [(String, Json)], IngestError> {
    match value {
        Json::Object(entries) => Ok(entries),
        _ => Err(IngestError::schema(context, "expected a JSON object")),
    }
}

fn as_array<'a>(value: &'a Json, context: &str) -> Result<&'a [Json], IngestError> {
    match value {
        Json::Array(values) => Ok(values),
        _ => Err(IngestError::schema(context, "expected a JSON array")),
    }
}

fn as_string<'a>(value: &'a Json, context: &str) -> Result<&'a str, IngestError> {
    match value {
        Json::String(text) => Ok(text),
        _ => Err(IngestError::schema(context, "expected a JSON string")),
    }
}

fn as_f64(value: &Json, context: &str) -> Result<f64, IngestError> {
    match value {
        Json::Number(number) => Ok(*number),
        _ => Err(IngestError::schema(context, "expected a JSON number")),
    }
}

fn as_u64(value: &Json, context: &str) -> Result<u64, IngestError> {
    let number = as_f64(value, context)?;
    if number < 0.0 || number.fract() != 0.0 || number > 9_007_199_254_740_992.0 {
        return Err(IngestError::range(
            context,
            "expected an unsigned integer <= 2^53",
        ));
    }
    Ok(number as u64)
}

fn as_i64(value: &Json, context: &str) -> Result<i64, IngestError> {
    let number = as_f64(value, context)?;
    if number.fract() != 0.0 || number.abs() > 9_007_199_254_740_992.0 {
        return Err(IngestError::range(context, "expected an integer <= 2^53"));
    }
    Ok(number as i64)
}

fn as_bool(value: &Json, context: &str) -> Result<bool, IngestError> {
    match value {
        Json::Bool(flag) => Ok(*flag),
        _ => Err(IngestError::schema(context, "expected a JSON boolean")),
    }
}

fn opt<'a>(entries: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn req<'a>(
    entries: &'a [(String, Json)],
    key: &str,
    context: &str,
) -> Result<&'a Json, IngestError> {
    opt(entries, key)
        .ok_or_else(|| IngestError::missing(format!("{context}.{key}"), "required field is absent"))
}

fn req_str<'a>(
    entries: &'a [(String, Json)],
    key: &str,
    context: &str,
) -> Result<&'a str, IngestError> {
    let value = req(entries, key, context)?;
    as_string(value, &format!("{context}.{key}"))
}

fn req_string(
    entries: &[(String, Json)],
    key: &str,
    context: &str,
    max_len: usize,
) -> Result<String, IngestError> {
    let text = req_str(entries, key, context)?;
    if text.is_empty() {
        return Err(IngestError::schema(
            format!("{context}.{key}"),
            "must not be empty",
        ));
    }
    if text.chars().count() > max_len {
        return Err(IngestError::range(
            format!("{context}.{key}"),
            format!("string longer than {max_len} chars"),
        ));
    }
    Ok(text.to_string())
}

fn opt_string(
    entries: &[(String, Json)],
    key: &str,
    context: &str,
    max_len: usize,
) -> Result<Option<String>, IngestError> {
    match opt(entries, key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => {
            let text = as_string(value, &format!("{context}.{key}"))?;
            if text.is_empty() {
                return Err(IngestError::schema(
                    format!("{context}.{key}"),
                    "must not be empty when present",
                ));
            }
            if text.chars().count() > max_len {
                return Err(IngestError::range(
                    format!("{context}.{key}"),
                    format!("string longer than {max_len} chars"),
                ));
            }
            Ok(Some(text.to_string()))
        }
    }
}

fn req_f64(entries: &[(String, Json)], key: &str, context: &str) -> Result<f64, IngestError> {
    let value = req(entries, key, context)?;
    as_f64(value, &format!("{context}.{key}"))
}

fn req_u64(entries: &[(String, Json)], key: &str, context: &str) -> Result<u64, IngestError> {
    let value = req(entries, key, context)?;
    as_u64(value, &format!("{context}.{key}"))
}

fn req_i64(entries: &[(String, Json)], key: &str, context: &str) -> Result<i64, IngestError> {
    let value = req(entries, key, context)?;
    as_i64(value, &format!("{context}.{key}"))
}

fn req_bool(entries: &[(String, Json)], key: &str, context: &str) -> Result<bool, IngestError> {
    let value = req(entries, key, context)?;
    as_bool(value, &format!("{context}.{key}"))
}

fn req_array<'a>(
    entries: &'a [(String, Json)],
    key: &str,
    context: &str,
) -> Result<&'a [Json], IngestError> {
    let value = req(entries, key, context)?;
    as_array(value, &format!("{context}.{key}"))
}

fn opt_f64(
    entries: &[(String, Json)],
    key: &str,
    context: &str,
) -> Result<Option<f64>, IngestError> {
    match opt(entries, key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => Ok(Some(as_f64(value, &format!("{context}.{key}"))?)),
    }
}

fn opt_u64(
    entries: &[(String, Json)],
    key: &str,
    context: &str,
) -> Result<Option<u64>, IngestError> {
    match opt(entries, key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => Ok(Some(as_u64(value, &format!("{context}.{key}"))?)),
    }
}

fn opt_bool(
    entries: &[(String, Json)],
    key: &str,
    context: &str,
) -> Result<Option<bool>, IngestError> {
    match opt(entries, key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => Ok(Some(as_bool(value, &format!("{context}.{key}"))?)),
    }
}

fn push_warning(warnings: &mut Vec<IngestWarning>, code: &'static str, message: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(IngestWarning { code, message });
    }
}

fn warn_unknown_fields(
    entries: &[(String, Json)],
    context: &str,
    known: &[&str],
    warnings: &mut Vec<IngestWarning>,
) {
    for (key, _) in entries {
        if !known.contains(&key.as_str()) {
            push_warning(
                warnings,
                IngestWarning::UNKNOWN_FIELD,
                format!("{context}.{key}: unknown field ignored"),
            );
        }
    }
}

fn fail_schema(found: &str, expected: &str, context: &str) -> IngestError {
    IngestError::schema(
        context,
        format!("schema mismatch: expected {expected:?}, found {found:?}"),
    )
}

fn is_lower_hex_64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_role_id(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn is_id_token(text: &str, max_len: usize) -> bool {
    !text.is_empty()
        && text.chars().count() <= max_len
        && text
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn f64_in_range(value: f64, min: f64, max: f64, context: &str) -> Result<f64, IngestError> {
    if !value.is_finite() {
        return Err(IngestError::range(context, "value must be finite"));
    }
    if value < min || value > max {
        return Err(IngestError::range(
            context,
            format!("value {value} outside [{min}, {max}]"),
        ));
    }
    Ok(value)
}

fn approx_eq(left: f64, right: f64, tolerance: f64) -> bool {
    (left - right).abs() <= tolerance
}

/// 本模块的静态守卫入口（供单测/证据链复核；不读取任何音频）。
#[doc(hidden)]
pub fn ingest_source_text_for_guards() -> &'static str {
    include_str!("ingest.rs")
}

// ===========================================================================
// SHA-256（内部实现；无第三方依赖，用于输入 hash 链校验与 KAT 自证）
// ===========================================================================

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    total_bytes: u64,
}

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            buffer: [0u8; 64],
            buffered: 0,
            total_bytes: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total_bytes = self.total_bytes.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let needed = 64 - self.buffered;
            let take = needed.min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.process_block(&block);
                self.buffered = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.process_block(block);
            data = rest;
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_bytes.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            for byte in &mut self.buffer[self.buffered..] {
                *byte = 0;
            }
            let block = self.buffer;
            self.process_block(&block);
            self.buffered = 0;
        }
        for byte in &mut self.buffer[self.buffered..56] {
            *byte = 0;
        }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.process_block(&block);
        let mut out = [0u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn process_block(&mut self, block: &[u8]) {
        debug_assert_eq!(block.len(), 64);
        let mut w = [0u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 计算 SHA-256 并返回小写 hex（64 字符）。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// 流式计算文件 SHA-256（小写 hex），不把整个文件读入内存。
pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<String> {
    sha256_file_with_len(path).map(|(hex, _)| hex)
}

fn sha256_file_with_len(path: impl AsRef<Path>) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.wrapping_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((hex_encode(&hasher.finalize()), total))
}

// ===========================================================================
// 文件 / hash 清单
// ===========================================================================

#[derive(Debug, Clone)]
struct VerifiedFile {
    bytes: u64,
    sha256: String,
}

struct IngestReader {
    root: PathBuf,
    sums: BTreeMap<String, String>,
    verified: BTreeMap<String, VerifiedFile>,
}

impl IngestReader {
    fn new(root: &Path) -> Result<Self, IngestError> {
        let sums_path = root.join(SHA256SUMS_FILE);
        let metadata = match fs::symlink_metadata(&sums_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(
                    IngestError::missing(SHA256SUMS_FILE, "manifest file is absent")
                        .with_path(&sums_path),
                );
            }
            Err(error) => return Err(IngestError::io(SHA256SUMS_FILE, error).with_path(&sums_path)),
        };
        if metadata.file_type().is_symlink() {
            return Err(IngestError::unsupported(
                SHA256SUMS_FILE,
                "symlinked SHA256SUMS is not allowed",
            )
            .with_path(&sums_path));
        }
        if !metadata.is_file() {
            return Err(
                IngestError::missing(SHA256SUMS_FILE, "not a regular file").with_path(&sums_path)
            );
        }
        if metadata.len() > MAX_SHA256SUMS_BYTES {
            return Err(IngestError::range(
                SHA256SUMS_FILE,
                format!("larger than {MAX_SHA256SUMS_BYTES} bytes"),
            )
            .with_path(&sums_path));
        }
        let bytes = fs::read(&sums_path)
            .map_err(|error| IngestError::io(SHA256SUMS_FILE, error).with_path(&sums_path))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            IngestError::json(SHA256SUMS_FILE, "manifest is not valid UTF-8").with_path(&sums_path)
        })?;
        let sums = parse_sha256sums(text)?;
        let mut reader = IngestReader {
            root: root.to_path_buf(),
            sums,
            verified: BTreeMap::new(),
        };
        reader.verify_all()?;
        Ok(reader)
    }

    fn resolve(&self, relative_path: &str) -> Result<PathBuf, IngestError> {
        let safe = safe_relative_path(relative_path)?;
        Ok(self.root.join(safe))
    }

    fn verify_all(&mut self) -> Result<(), IngestError> {
        if self.sums.len() > MAX_MANIFEST_ENTRIES {
            return Err(IngestError::range(
                SHA256SUMS_FILE,
                format!("more than {MAX_MANIFEST_ENTRIES} entries"),
            ));
        }
        let mut total_bytes: u64 = 0;
        let names: Vec<String> = self.sums.keys().cloned().collect();
        for relative_path in names {
            let expected = self.sums.get(&relative_path).cloned().unwrap_or_default();
            let path = self.resolve(&relative_path)?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                IngestError::missing(
                    SHA256SUMS_FILE,
                    format!("listed file {relative_path:?} is not accessible: {error}"),
                )
                .with_path(&path)
            })?;
            if metadata.file_type().is_symlink() {
                return Err(IngestError::unsupported(
                    "ingest.input",
                    format!("listed path {relative_path:?} is a symlink (not allowed)"),
                )
                .with_path(&path));
            }
            if !metadata.is_file() {
                return Err(IngestError::missing(
                    "ingest.input",
                    format!("listed path {relative_path:?} is not a regular file"),
                )
                .with_path(&path));
            }
            total_bytes = total_bytes.saturating_add(metadata.len());
            if total_bytes > MAX_TOTAL_INPUT_BYTES {
                return Err(IngestError::range(
                    "ingest.input",
                    format!("input bundle larger than {MAX_TOTAL_INPUT_BYTES} bytes"),
                ));
            }
            let (actual, bytes) = sha256_file_with_len(&path)
                .map_err(|error| IngestError::io(relative_path.clone(), error).with_path(&path))?;
            if actual != expected {
                return Err(IngestError::hash(
                    relative_path.clone(),
                    format!("sha256 mismatch: manifest {expected}, actual {actual}"),
                )
                .with_path(&path));
            }
            self.verified.insert(
                relative_path,
                VerifiedFile {
                    bytes,
                    sha256: actual,
                },
            );
        }
        Ok(())
    }

    fn verified(&self, relative_path: &str) -> Result<&VerifiedFile, IngestError> {
        self.verified.get(relative_path).ok_or_else(|| {
            IngestError::missing(
                relative_path.to_string(),
                format!("file not covered by {SHA256SUMS_FILE}"),
            )
        })
    }

    fn read_json(
        &self,
        relative_path: &str,
        max_bytes: u64,
        context: &str,
    ) -> Result<(Json, String), IngestError> {
        let verified = self.verified(relative_path)?;
        if verified.bytes > max_bytes {
            return Err(IngestError::range(
                context,
                format!("{relative_path:?} larger than {max_bytes} bytes"),
            ));
        }
        let path = self.resolve(relative_path)?;
        let bytes =
            fs::read(&path).map_err(|error| IngestError::io(context, error).with_path(&path))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            IngestError::json(context, format!("{relative_path:?} is not valid UTF-8"))
        })?;
        let parsed = JsonParser::parse(text).map_err(|error| {
            IngestError::new(
                error.kind(),
                format!("{context} ({relative_path})"),
                error.detail(),
            )
            .with_path(&path)
        })?;
        let actual = sha256_hex(&bytes);
        if actual != verified.sha256 {
            return Err(IngestError::hash(
                context,
                format!("{relative_path:?} changed after manifest verification"),
            )
            .with_path(&path));
        }
        Ok((parsed, actual))
    }

    fn capture(
        &self,
        relative_path: &str,
        channels: u16,
        sample_rate_hz: u32,
        sample_format: SampleFormat,
        context: &str,
    ) -> Result<CaptureFile, IngestError> {
        let verified = self.verified(relative_path)?.clone();
        if channels == 0 {
            return Err(IngestError::schema(
                context,
                "channel count must be positive",
            ));
        }
        let frame_bytes = channels as u64 * sample_format.bytes_per_sample();
        if frame_bytes == 0 || verified.bytes % frame_bytes != 0 {
            return Err(IngestError::audio(
                context,
                format!(
                    "{relative_path:?} byte length {} is not a multiple of {} ({}ch x {} bytes)",
                    verified.bytes,
                    frame_bytes,
                    channels,
                    sample_format.bytes_per_sample()
                ),
            ));
        }
        let frames = verified.bytes / frame_bytes;
        if frames == 0 {
            return Err(IngestError::audio(
                context,
                format!("{relative_path:?} contains zero frames"),
            ));
        }
        Ok(CaptureFile {
            relative_path: relative_path.to_string(),
            sha256: verified.sha256,
            bytes: verified.bytes,
            frames,
            channels,
            sample_rate_hz,
            sample_format,
        })
    }
}

fn safe_relative_path(relative_path: &str) -> Result<PathBuf, IngestError> {
    if relative_path.is_empty() {
        return Err(IngestError::schema(
            "path",
            "relative path must not be empty",
        ));
    }
    if relative_path.len() > 512 {
        return Err(IngestError::range(
            "path",
            "relative path longer than 512 bytes",
        ));
    }
    if relative_path.contains('\\') {
        return Err(IngestError::unsupported(
            "path",
            format!("backslashes are not allowed in {relative_path:?}"),
        ));
    }
    if relative_path.starts_with('/') {
        return Err(IngestError::unsupported(
            "path",
            format!("absolute path {relative_path:?} is not allowed"),
        ));
    }
    let candidate = Path::new(relative_path);
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            _ => {
                return Err(IngestError::unsupported(
                    "path",
                    format!("path {relative_path:?} must be relative with no dot segments"),
                ))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(IngestError::schema(
            "path",
            format!("path {relative_path:?} normalizes to empty"),
        ));
    }
    Ok(normalized)
}

fn parse_sha256sums(text: &str) -> Result<BTreeMap<String, String>, IngestError> {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    for (line_number, raw_line) in text.lines().enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let split_at = line
            .find(|ch: char| ch == ' ' || ch == '\t' || ch == '*')
            .ok_or_else(|| {
                IngestError::hash(
                    SHA256SUMS_FILE,
                    format!("line {} is missing the path separator", line_number + 1),
                )
            })?;
        let hash = &line[..split_at];
        if !is_lower_hex_64(hash) {
            return Err(IngestError::hash(
                SHA256SUMS_FILE,
                format!(
                    "line {} has an invalid sha256 (expected 64 lowercase hex), found {:?}",
                    line_number + 1,
                    hash
                ),
            ));
        }
        let mut rest = &line[split_at..];
        rest = rest.trim_start_matches(|ch: char| ch == ' ' || ch == '\t');
        rest = rest.strip_prefix('*').unwrap_or(rest);
        let path = rest.trim_end();
        if path.is_empty() {
            return Err(IngestError::hash(
                SHA256SUMS_FILE,
                format!("line {} is missing a file path", line_number + 1),
            ));
        }
        safe_relative_path(path)?;
        if let Some(previous) = entries.insert(path.to_string(), hash.to_string()) {
            return Err(IngestError::duplicate(
                SHA256SUMS_FILE,
                format!(
                    "path {path:?} listed twice (line {}; previous hash {previous})",
                    line_number + 1
                ),
            ));
        }
        if entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(IngestError::range(
                SHA256SUMS_FILE,
                format!("more than {MAX_MANIFEST_ENTRIES} entries"),
            ));
        }
    }
    if entries.is_empty() {
        return Err(IngestError::missing(
            SHA256SUMS_FILE,
            "manifest contains no file entries",
        ));
    }
    Ok(entries)
}

// ===========================================================================
// session / position 清单解析
// ===========================================================================

fn expect_schema(
    entries: &[(String, Json)],
    expected: &str,
    context: &str,
) -> Result<(), IngestError> {
    let found = req_str(entries, "schema", context)?;
    if found != expected {
        return Err(fail_schema(found, expected, &format!("{context}.schema")));
    }
    Ok(())
}

fn parse_session(
    json: &Json,
    warnings: &mut Vec<IngestWarning>,
) -> Result<IngestSession, IngestError> {
    let context = SESSION_FILE;
    let entries = as_object(json, context)?;
    warn_unknown_fields(
        entries,
        context,
        &[
            "schema",
            "session_id",
            "layout",
            "sample_rate_hz",
            "block_frames",
            "participants",
            "speakers",
            "mic",
            "playback",
            "capture",
            "reference",
            "stimulus",
            "room_temperature_c",
            "tool",
            "created_unix",
            "claim",
        ],
        warnings,
    );
    expect_schema(entries, SESSION_SCHEMA, context)?;
    let session_id = req_string(entries, "session_id", context, 128)?;
    if !is_id_token(&session_id, 128) {
        return Err(IngestError::schema(
            format!("{context}.session_id"),
            format!("{session_id:?} is not a safe id token"),
        ));
    }
    let layout = req_string(entries, "layout", context, 64)?;
    if !is_id_token(&layout, 64) {
        return Err(IngestError::schema(
            format!("{context}.layout"),
            format!("{layout:?} is not a layout id token"),
        ));
    }
    let sample_rate_raw = req_u64(entries, "sample_rate_hz", context)?;
    if sample_rate_raw > u32::MAX as u64 {
        return Err(IngestError::range(
            format!("{context}.sample_rate_hz"),
            "does not fit in u32",
        ));
    }
    let sample_rate_hz = sample_rate_raw as u32;
    if !SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
        return Err(IngestError::unsupported(
            format!("{context}.sample_rate_hz"),
            format!("{sample_rate_hz} Hz is not a supported rate (48k/96k)"),
        ));
    }
    let block_frames = match opt_u64(entries, "block_frames", context)? {
        None => None,
        Some(value) => {
            if value > u16::MAX as u64 || !khaen_core::rt::BLOCK_SIZES.contains(&(value as u16))
            {
                return Err(IngestError::unsupported(
                    format!("{context}.block_frames"),
                    format!("{value} is not a supported block size (64/128)"),
                ));
            }
            Some(value as u16)
        }
    };

    let participant_values = req_array(entries, "participants", context)?;
    if participant_values.is_empty() {
        return Err(IngestError::missing(
            format!("{context}.participants"),
            "at least one participant is required",
        ));
    }
    if participant_values.len() > MAX_LOW_BAND_PARTICIPANTS {
        return Err(IngestError::range(
            format!("{context}.participants"),
            format!("more than MAX_LOW_BAND_PARTICIPANTS={MAX_LOW_BAND_PARTICIPANTS} participants"),
        ));
    }
    let mut participants: Vec<String> = Vec::new();
    let mut participant_set: BTreeSet<String> = BTreeSet::new();
    for (index, value) in participant_values.iter().enumerate() {
        let name = as_string(value, &format!("{context}.participants[{index}]"))?;
        if !is_role_id(name) {
            return Err(IngestError::schema(
                format!("{context}.participants[{index}]"),
                format!("{name:?} is not an uppercase role id"),
            ));
        }
        if !participant_set.insert(name.to_string()) {
            return Err(IngestError::duplicate(
                format!("{context}.participants"),
                format!("duplicate participant {name:?}"),
            ));
        }
        participants.push(name.to_string());
    }

    let speaker_values = req_array(entries, "speakers", context)?;
    if speaker_values.is_empty() {
        return Err(IngestError::missing(
            format!("{context}.speakers"),
            "at least one speaker is required",
        ));
    }
    if speaker_values.len() > MAX_OUTPUT_CHANNELS {
        return Err(IngestError::range(
            format!("{context}.speakers"),
            format!("more than MAX_OUTPUT_CHANNELS={MAX_OUTPUT_CHANNELS} speakers"),
        ));
    }
    let mut speakers: Vec<SpeakerInfo> = Vec::new();
    let mut speaker_names: BTreeSet<String> = BTreeSet::new();
    let mut out_indices: BTreeSet<u32> = BTreeSet::new();
    for (index, value) in speaker_values.iter().enumerate() {
        let item_context = format!("{context}.speakers[{index}]");
        let item = as_object(value, &item_context)?;
        warn_unknown_fields(
            item,
            &item_context,
            &["name", "out_index", "kind", "allowed_band_hz"],
            warnings,
        );
        let name = req_string(item, "name", &item_context, 64)?;
        if !is_role_id(&name) {
            return Err(IngestError::schema(
                format!("{item_context}.name"),
                format!("{name:?} is not an uppercase role id"),
            ));
        }
        if !speaker_names.insert(name.clone()) {
            return Err(IngestError::duplicate(
                format!("{context}.speakers"),
                format!("duplicate speaker {name:?}"),
            ));
        }
        let out_index_raw = req_u64(item, "out_index", &item_context)?;
        if out_index_raw >= MAX_OUTPUT_CHANNELS as u64 {
            return Err(IngestError::range(
                format!("{item_context}.out_index"),
                format!("{out_index_raw} >= MAX_OUTPUT_CHANNELS={MAX_OUTPUT_CHANNELS}"),
            ));
        }
        let out_index = out_index_raw as u32;
        if !out_indices.insert(out_index) {
            return Err(IngestError::duplicate(
                format!("{context}.speakers"),
                format!("duplicate out_index {out_index}"),
            ));
        }
        let kind = req_string(item, "kind", &item_context, 32)?;
        let allowed_band_hz = match opt(item, "allowed_band_hz") {
            None | Some(Json::Null) => None,
            Some(value) => {
                let band = as_array(value, &format!("{item_context}.allowed_band_hz"))?;
                if band.len() != 2 {
                    return Err(IngestError::schema(
                        format!("{item_context}.allowed_band_hz"),
                        "expected [start_hz, end_hz]",
                    ));
                }
                let start = as_f64(&band[0], &format!("{item_context}.allowed_band_hz[0]"))?;
                let end = as_f64(&band[1], &format!("{item_context}.allowed_band_hz[1]"))?;
                if !(0.0 < start && start < end && end <= sample_rate_hz as f64 / 2.0) {
                    return Err(IngestError::range(
                        format!("{item_context}.allowed_band_hz"),
                        format!("invalid band [{start}, {end}] for fs={sample_rate_hz}"),
                    ));
                }
                Some((start, end))
            }
        };
        speakers.push(SpeakerInfo {
            name,
            out_index,
            kind,
            allowed_band_hz,
        });
    }
    for participant in &participants {
        if !speaker_names.contains(participant) {
            return Err(IngestError::schema(
                format!("{context}.participants"),
                format!("participant {participant:?} is not declared in speakers"),
            ));
        }
    }

    let mic_entries = as_object(req(entries, "mic", context)?, &format!("{context}.mic"))?;
    warn_unknown_fields(
        mic_entries,
        &format!("{context}.mic"),
        &[
            "model",
            "serial",
            "cal_sha256",
            "orientation",
            "clock_domain",
        ],
        warnings,
    );
    let mic = MicInfo {
        model: req_string(mic_entries, "model", &format!("{context}.mic"), 64)?,
        serial: req_string(mic_entries, "serial", &format!("{context}.mic"), 64)?,
        cal_sha256: req_string(mic_entries, "cal_sha256", &format!("{context}.mic"), 64)?,
        orientation: req_string(mic_entries, "orientation", &format!("{context}.mic"), 64)?,
        clock_domain: req_string(mic_entries, "clock_domain", &format!("{context}.mic"), 16)?,
    };
    if !is_lower_hex_64(&mic.cal_sha256) || mic.cal_sha256.bytes().all(|byte| byte == b'0') {
        return Err(IngestError::hash(
            format!("{context}.mic.cal_sha256"),
            "expected 64 lowercase hex chars of a real calibration file hash",
        ));
    }
    if mic.clock_domain != "usb" {
        return Err(IngestError::unsupported(
            format!("{context}.mic.clock_domain"),
            format!(
                "UMIK must be an independent USB ADC (clock_domain usb), found {:?}",
                mic.clock_domain
            ),
        ));
    }

    let playback_entries = as_object(
        req(entries, "playback", context)?,
        &format!("{context}.playback"),
    )?;
    warn_unknown_fields(
        playback_entries,
        &format!("{context}.playback"),
        &["device", "channels", "sample_format"],
        warnings,
    );
    let playback = PlaybackInfo {
        device: req_string(
            playback_entries,
            "device",
            &format!("{context}.playback"),
            128,
        )?,
        channels: parse_channels(
            req_u64(playback_entries, "channels", &format!("{context}.playback"))?,
            MAX_OUTPUT_CHANNELS,
            &format!("{context}.playback.channels"),
        )?,
        sample_format: parse_sample_format(
            req_str(
                playback_entries,
                "sample_format",
                &format!("{context}.playback"),
            )?,
            &format!("{context}.playback.sample_format"),
        )?,
    };

    let capture_entries = as_object(
        req(entries, "capture", context)?,
        &format!("{context}.capture"),
    )?;
    warn_unknown_fields(
        capture_entries,
        &format!("{context}.capture"),
        &[
            "device",
            "channels",
            "sample_format",
            "pre_seconds",
            "post_seconds",
        ],
        warnings,
    );
    let capture = CaptureInfo {
        device: req_string(
            capture_entries,
            "device",
            &format!("{context}.capture"),
            128,
        )?,
        channels: parse_channels(
            req_u64(capture_entries, "channels", &format!("{context}.capture"))?,
            MAX_INPUT_CHANNELS,
            &format!("{context}.capture.channels"),
        )?,
        sample_format: parse_sample_format(
            req_str(
                capture_entries,
                "sample_format",
                &format!("{context}.capture"),
            )?,
            &format!("{context}.capture.sample_format"),
        )?,
        pre_seconds: f64_in_range(
            req_f64(
                capture_entries,
                "pre_seconds",
                &format!("{context}.capture"),
            )?,
            0.0,
            30.0,
            &format!("{context}.capture.pre_seconds"),
        )?,
        post_seconds: f64_in_range(
            req_f64(
                capture_entries,
                "post_seconds",
                &format!("{context}.capture"),
            )?,
            0.0,
            30.0,
            &format!("{context}.capture.post_seconds"),
        )?,
    };
    if capture.channels != 1 {
        return Err(IngestError::unsupported(
            format!("{context}.capture.channels"),
            format!(
                "UMIK ingest requires exactly 1 capture channel (found {})",
                capture.channels
            ),
        ));
    }

    let reference = match opt(entries, "reference") {
        None | Some(Json::Null) => None,
        Some(value) => {
            let reference_entries = as_object(value, &format!("{context}.reference"))?;
            warn_unknown_fields(
                reference_entries,
                &format!("{context}.reference"),
                &["device", "channels", "sample_format", "independent"],
                warnings,
            );
            let device = req_string(
                reference_entries,
                "device",
                &format!("{context}.reference"),
                128,
            )?;
            if device == capture.device {
                return Err(IngestError::unsupported(
                    format!("{context}.reference.device"),
                    "reference must be an independent channel/device, not the UMIK capture device",
                ));
            }
            let independent = req_bool(
                reference_entries,
                "independent",
                &format!("{context}.reference"),
            )?;
            if !independent {
                return Err(IngestError::unsupported(
                    format!("{context}.reference.independent"),
                    "reference must declare independent=true",
                ));
            }
            Some(ReferenceInfo {
                device,
                channels: parse_channels(
                    req_u64(
                        reference_entries,
                        "channels",
                        &format!("{context}.reference"),
                    )?,
                    MAX_INPUT_CHANNELS,
                    &format!("{context}.reference.channels"),
                )?,
                sample_format: parse_sample_format(
                    req_str(
                        reference_entries,
                        "sample_format",
                        &format!("{context}.reference"),
                    )?,
                    &format!("{context}.reference.sample_format"),
                )?,
                independent,
            })
        }
    };

    let stimulus_entries = as_object(
        req(entries, "stimulus", context)?,
        &format!("{context}.stimulus"),
    )?;
    warn_unknown_fields(
        stimulus_entries,
        &format!("{context}.stimulus"),
        &[
            "kind",
            "start_hz",
            "end_hz",
            "seconds",
            "level_dbfs",
            "fade_ms",
            "repeats",
            "repeat_gap_seconds",
        ],
        warnings,
    );
    let kind = req_string(stimulus_entries, "kind", &format!("{context}.stimulus"), 32)?;
    if !matches!(kind.as_str(), "log_sweep" | "linear_sweep" | "noise" | "ir") {
        return Err(IngestError::unsupported(
            format!("{context}.stimulus.kind"),
            format!("unsupported stimulus kind {kind:?}"),
        ));
    }
    let start_hz = f64_in_range(
        req_f64(stimulus_entries, "start_hz", &format!("{context}.stimulus"))?,
        f64::MIN_POSITIVE,
        sample_rate_hz as f64 / 2.0,
        &format!("{context}.stimulus.start_hz"),
    )?;
    let end_hz = f64_in_range(
        req_f64(stimulus_entries, "end_hz", &format!("{context}.stimulus"))?,
        start_hz,
        sample_rate_hz as f64 / 2.0,
        &format!("{context}.stimulus.end_hz"),
    )?;
    if end_hz <= start_hz {
        return Err(IngestError::range(
            format!("{context}.stimulus.end_hz"),
            format!("end_hz {end_hz} must be greater than start_hz {start_hz}"),
        ));
    }
    let seconds = f64_in_range(
        req_f64(stimulus_entries, "seconds", &format!("{context}.stimulus"))?,
        0.001,
        600.0,
        &format!("{context}.stimulus.seconds"),
    )?;
    let level_dbfs = f64_in_range(
        req_f64(
            stimulus_entries,
            "level_dbfs",
            &format!("{context}.stimulus"),
        )?,
        -120.0,
        0.0,
        &format!("{context}.stimulus.level_dbfs"),
    )?;
    let fade_ms = f64_in_range(
        req_f64(stimulus_entries, "fade_ms", &format!("{context}.stimulus"))?,
        0.0,
        1000.0,
        &format!("{context}.stimulus.fade_ms"),
    )?;
    let repeats_raw = req_u64(stimulus_entries, "repeats", &format!("{context}.stimulus"))?;
    if repeats_raw == 0 || repeats_raw > 8 {
        return Err(IngestError::range(
            format!("{context}.stimulus.repeats"),
            format!("repeats {repeats_raw} outside [1, 8]"),
        ));
    }
    let repeats = repeats_raw as u32;
    if repeats < 3 {
        push_warning(
            warnings,
            IngestWarning::LOW_REPEATS,
            format!(
                "{context}.stimulus.repeats={repeats}: T6/quality policy requires >=3 design/off repeats"
            ),
        );
    }
    let repeat_gap_seconds = match opt_f64(
        stimulus_entries,
        "repeat_gap_seconds",
        &format!("{context}.stimulus"),
    )? {
        None => 0.2,
        Some(value) => f64_in_range(
            value,
            0.0,
            10.0,
            &format!("{context}.stimulus.repeat_gap_seconds"),
        )?,
    };
    let stimulus = StimulusInfo {
        kind,
        start_hz,
        end_hz,
        seconds,
        level_dbfs,
        fade_ms,
        repeats,
        repeat_gap_seconds,
    };

    let room_temperature_c = f64_in_range(
        req_f64(entries, "room_temperature_c", context)?,
        -10.0,
        60.0,
        &format!("{context}.room_temperature_c"),
    )?;
    let tool_entries = as_object(req(entries, "tool", context)?, &format!("{context}.tool"))?;
    warn_unknown_fields(
        tool_entries,
        &format!("{context}.tool"),
        &["name", "version"],
        warnings,
    );
    let tool = ToolInfo {
        name: req_string(tool_entries, "name", &format!("{context}.tool"), 64)?,
        version: req_string(tool_entries, "version", &format!("{context}.tool"), 64)?,
    };
    let created_unix = opt_u64(entries, "created_unix", context)?;
    let claim = opt_string(entries, "claim", context, 500)?;

    Ok(IngestSession {
        session_id,
        layout,
        sample_rate_hz,
        block_frames,
        participants,
        speakers,
        mic,
        playback,
        capture,
        reference,
        stimulus,
        room_temperature_c,
        tool,
        created_unix,
        claim,
    })
}

fn parse_channels(value: u64, max: usize, context: &str) -> Result<u16, IngestError> {
    if value == 0 || value > max as u64 || value > u16::MAX as u64 {
        return Err(IngestError::range(
            context,
            format!("channel count {value} outside [1, {max}]"),
        ));
    }
    Ok(value as u16)
}

fn parse_sample_format(value: &str, context: &str) -> Result<SampleFormat, IngestError> {
    SampleFormat::parse(value).ok_or_else(|| {
        IngestError::unsupported(
            context,
            format!("unsupported sample format {value:?} (S24_3LE/S32_LE/F32_LE)"),
        )
    })
}

fn parse_position(
    json: &Json,
    warnings: &mut Vec<IngestWarning>,
) -> Result<PositionMeta, IngestError> {
    let context = POSITION_FILE;
    let entries = as_object(json, context)?;
    warn_unknown_fields(
        entries,
        context,
        &[
            "schema",
            "name",
            "role",
            "weight",
            "coordinates_m",
            "mic_orientation",
            "created_unix",
        ],
        warnings,
    );
    expect_schema(entries, POSITION_SCHEMA, context)?;
    let name = req_string(entries, "name", context, 64)?;
    if !is_id_token(&name, 64) {
        return Err(IngestError::schema(
            format!("{context}.name"),
            format!("{name:?} is not a safe label token"),
        ));
    }
    let role_text = req_str(entries, "role", context)?;
    let role = PositionRole::parse(role_text).ok_or_else(|| {
        IngestError::schema(
            format!("{context}.role"),
            format!("unsupported role {role_text:?} (design|validation)"),
        )
    })?;
    let weight = f64_in_range(
        req_f64(entries, "weight", context)?,
        0.0,
        1_000_000.0,
        &format!("{context}.weight"),
    )?;
    if weight <= 0.0 {
        return Err(IngestError::range(
            format!("{context}.weight"),
            "weight must be > 0",
        ));
    }
    let coordinates_m = match opt(entries, "coordinates_m") {
        None | Some(Json::Null) => None,
        Some(value) => {
            let values = as_array(value, &format!("{context}.coordinates_m"))?;
            if values.len() != 3 {
                return Err(IngestError::schema(
                    format!("{context}.coordinates_m"),
                    "expected [x, y, z] in metres",
                ));
            }
            let mut out = [0.0_f64; 3];
            for (index, value) in values.iter().enumerate() {
                out[index] = f64_in_range(
                    as_f64(value, &format!("{context}.coordinates_m[{index}]"))?,
                    -100.0,
                    100.0,
                    &format!("{context}.coordinates_m[{index}]"),
                )?;
            }
            Some(out)
        }
    };
    let mic_orientation = opt_string(entries, "mic_orientation", context, 64)?;
    let created_unix = opt_u64(entries, "created_unix", context)?;
    Ok(PositionMeta {
        name,
        role,
        weight,
        coordinates_m,
        mic_orientation,
        created_unix,
    })
}

// ===========================================================================
// diag（公共时轴）解析
// ===========================================================================

fn parse_diag(
    json: &Json,
    context: &str,
    session: &IngestSession,
    speaker: &SpeakerInfo,
) -> Result<TimebaseDiag, IngestError> {
    let entries = as_object(json, context)?;
    let mut warnings = Vec::new();
    warn_unknown_fields(
        entries,
        context,
        &[
            "schema",
            "session_id",
            "epoch_id",
            "graph_id",
            "speaker",
            "channel",
            "slot",
            "capture_file",
            "render_frame_index",
            "capture_sample_index",
            "nominal_start",
            "actual_start",
            "lag_samples",
            "repeat_starts",
            "repeats",
            "clock",
            "onset_invalid",
        ],
        &mut warnings,
    );
    expect_schema(entries, DIAG_SCHEMA, context)?;
    for key in [
        "session_id",
        "epoch_id",
        "graph_id",
        "speaker",
        "channel",
        "slot",
        "capture_file",
        "render_frame_index",
        "capture_sample_index",
        "nominal_start",
        "actual_start",
        "lag_samples",
        "repeat_starts",
        "repeats",
        "clock",
        "onset_invalid",
    ] {
        if opt(entries, key).is_none() {
            return Err(IngestError::timebase(
                format!("{context}.{key}"),
                "required public time-axis field is absent",
            ));
        }
    }
    let session_id = req_string(entries, "session_id", context, 128)?;
    if session_id != session.session_id {
        return Err(IngestError::timebase(
            format!("{context}.session_id"),
            format!(
                "diag session_id {session_id:?} does not match session.json session_id {:?}",
                session.session_id
            ),
        ));
    }
    let epoch_id = req_u64(entries, "epoch_id", context)?;
    if epoch_id == 0 {
        return Err(IngestError::timebase(
            format!("{context}.epoch_id"),
            "epoch_id must be > 0",
        ));
    }
    let graph_id = req_string(entries, "graph_id", context, 64)?;
    if !is_lower_hex_64(&graph_id) || graph_id.bytes().all(|byte| byte == b'0') {
        return Err(IngestError::schema(
            format!("{context}.graph_id"),
            "expected 64 lowercase hex chars of a real graph id (not all zero)",
        ));
    }
    let diag_speaker = req_string(entries, "speaker", context, 64)?;
    if diag_speaker != speaker.name {
        return Err(IngestError::schema(
            format!("{context}.speaker"),
            format!(
                "diag speaker {diag_speaker:?} does not match expected participant {:?}",
                speaker.name
            ),
        ));
    }
    let channel = req_u64(entries, "channel", context)?;
    if channel != speaker.out_index as u64 {
        return Err(IngestError::schema(
            format!("{context}.channel"),
            format!(
                "diag channel {channel} does not match speaker out_index {}",
                speaker.out_index
            ),
        ));
    }
    let slot = req_u64(entries, "slot", context)?;
    if slot >= session.participants.len() as u64 {
        return Err(IngestError::timebase(
            format!("{context}.slot"),
            format!(
                "slot {slot} outside 0..{} (participants)",
                session.participants.len()
            ),
        ));
    }
    let capture_file = req_string(entries, "capture_file", context, 256)?;
    if capture_file != CAPTURE_FILE {
        return Err(IngestError::unsupported(
            format!("{context}.capture_file"),
            format!(
                "ingest contract expects capture file {CAPTURE_FILE:?}, found {capture_file:?}"
            ),
        ));
    }
    let render_frame_index = req_u64(entries, "render_frame_index", context)?;
    if render_frame_index == 0 {
        return Err(IngestError::timebase(
            format!("{context}.render_frame_index"),
            "render_frame_index must be > 0",
        ));
    }
    let capture_sample_index = req_u64(entries, "capture_sample_index", context)?;
    let nominal_start = req_f64(entries, "nominal_start", context)?;
    let actual_start = req_f64(entries, "actual_start", context)?;
    let lag_samples = req_f64(entries, "lag_samples", context)?;
    if !nominal_start.is_finite() || nominal_start < 0.0 {
        return Err(IngestError::timebase(
            format!("{context}.nominal_start"),
            format!("invalid nominal_start {nominal_start}"),
        ));
    }
    if !actual_start.is_finite() || actual_start < 0.0 {
        return Err(IngestError::timebase(
            format!("{context}.actual_start"),
            format!("invalid actual_start {actual_start}"),
        ));
    }
    if !lag_samples.is_finite() || lag_samples.abs() > session.sample_rate_hz as f64 {
        return Err(IngestError::timebase(
            format!("{context}.lag_samples"),
            format!("implausible lag_samples {lag_samples}"),
        ));
    }
    if !approx_eq(lag_samples, actual_start - nominal_start, 0.01) {
        return Err(IngestError::timebase(
            format!("{context}.lag_samples"),
            format!(
                "lag_samples {lag_samples} does not match actual_start - nominal_start ({})",
                actual_start - nominal_start
            ),
        ));
    }
    if (capture_sample_index as f64 - actual_start).abs() > 1.0 {
        return Err(IngestError::timebase(
            format!("{context}.capture_sample_index"),
            format!(
                "capture_sample_index {capture_sample_index} is not consistent with actual_start {actual_start}"
            ),
        ));
    }

    let repeat_values = req_array(entries, "repeat_starts", context)?;
    if repeat_values.is_empty() {
        return Err(IngestError::timebase(
            format!("{context}.repeat_starts"),
            "repeat_starts must not be empty",
        ));
    }
    let mut repeat_starts = Vec::with_capacity(repeat_values.len());
    for (index, value) in repeat_values.iter().enumerate() {
        let start = as_f64(value, &format!("{context}.repeat_starts[{index}]"))?;
        if !start.is_finite() || start < 0.0 {
            return Err(IngestError::timebase(
                format!("{context}.repeat_starts[{index}]"),
                format!("invalid repeat start {start}"),
            ));
        }
        if index > 0 && start <= repeat_starts[index - 1] {
            return Err(IngestError::timebase(
                format!("{context}.repeat_starts"),
                "repeat starts must be strictly increasing",
            ));
        }
        repeat_starts.push(start);
    }
    if (repeat_starts[0] - actual_start).abs() > 1.0 {
        return Err(IngestError::timebase(
            format!("{context}.repeat_starts[0]"),
            "first repeat start must equal actual_start (+-1 sample)",
        ));
    }
    let repeats_raw = req_u64(entries, "repeats", context)?;
    if repeats_raw != session.stimulus.repeats as u64 {
        return Err(IngestError::timebase(
            format!("{context}.repeats"),
            format!(
                "diag repeats {repeats_raw} does not match stimulus.repeats {}",
                session.stimulus.repeats
            ),
        ));
    }
    if repeat_starts.len() != repeats_raw as usize {
        return Err(IngestError::timebase(
            format!("{context}.repeat_starts"),
            format!(
                "repeat_starts length {} does not match repeats {repeats_raw}",
                repeat_starts.len()
            ),
        ));
    }

    let clock_entries = as_object(req(entries, "clock", context)?, &format!("{context}.clock"))?;
    warn_unknown_fields(
        clock_entries,
        &format!("{context}.clock"),
        &[
            "clock_ppm",
            "clock_ppm_uncertainty_ppm",
            "drift_significant",
            "clock_ppm_ok",
            "correction",
        ],
        &mut warnings,
    );
    let clock_ppm = req_f64(clock_entries, "clock_ppm", &format!("{context}.clock"))?;
    let uncertainty = req_f64(
        clock_entries,
        "clock_ppm_uncertainty_ppm",
        &format!("{context}.clock"),
    )?;
    if !clock_ppm.is_finite() || clock_ppm.abs() > 1000.0 {
        return Err(IngestError::timebase(
            format!("{context}.clock.clock_ppm"),
            format!("implausible clock_ppm {clock_ppm}"),
        ));
    }
    if !uncertainty.is_finite() || uncertainty < 0.0 || uncertainty > 1000.0 {
        return Err(IngestError::timebase(
            format!("{context}.clock.clock_ppm_uncertainty_ppm"),
            format!("invalid uncertainty {uncertainty}"),
        ));
    }
    let drift_significant = req_bool(
        clock_entries,
        "drift_significant",
        &format!("{context}.clock"),
    )?;
    let clock_ppm_ok = req_bool(clock_entries, "clock_ppm_ok", &format!("{context}.clock"))?;
    if !clock_ppm_ok {
        return Err(IngestError::timebase(
            format!("{context}.clock.clock_ppm_ok"),
            "mic clock estimate declared invalid",
        ));
    }
    let correction_text = req_str(clock_entries, "correction", &format!("{context}.clock"))?;
    let correction = DriftCorrection::parse(correction_text).ok_or_else(|| {
        IngestError::schema(
            format!("{context}.clock.correction"),
            format!("unsupported drift correction {correction_text:?} (none|resample)"),
        )
    })?;
    let computed_significant = clock_ppm.abs() > 2.0 * uncertainty;
    if computed_significant != drift_significant {
        return Err(IngestError::timebase(
            format!("{context}.clock.drift_significant"),
            format!(
                "drift_significant={drift_significant} contradicts |clock_ppm|={} vs 2*uncertainty={}",
                clock_ppm.abs(),
                (2.0 * uncertainty).abs()
            ),
        ));
    }
    if drift_significant && correction == DriftCorrection::None {
        return Err(IngestError::timebase(
            format!("{context}.clock.correction"),
            "significant drift is declared but correction is none",
        ));
    }
    let onset_invalid = req_bool(entries, "onset_invalid", context)?;
    if onset_invalid {
        return Err(IngestError::timebase(
            format!("{context}.onset_invalid"),
            "measurement declares an invalid sweep onset",
        ));
    }

    Ok(TimebaseDiag {
        session_id,
        epoch_id,
        graph_id,
        speaker: diag_speaker,
        channel: channel as u32,
        slot: slot as u32,
        capture_file,
        render_frame_index,
        capture_sample_index,
        nominal_start,
        actual_start,
        lag_samples,
        repeat_starts,
        repeats: repeats_raw as u32,
        clock: MicClock {
            clock_ppm,
            clock_ppm_uncertainty_ppm: uncertainty,
            drift_significant,
            clock_ppm_ok,
            correction,
        },
        onset_invalid,
    })
}

// ===========================================================================
// analysis（source）解析
// ===========================================================================

fn parse_fr(
    values: &[Json],
    context: &str,
    sample_rate_hz: u32,
) -> Result<Vec<FrPoint>, IngestError> {
    if values.len() > MAX_FR_POINTS {
        return Err(IngestError::range(
            context,
            format!("more than {MAX_FR_POINTS} FR points"),
        ));
    }
    let mut points = Vec::with_capacity(values.len());
    let mut previous_freq = 0.0_f64;
    let nyquist = sample_rate_hz as f64 / 2.0;
    for (index, value) in values.iter().enumerate() {
        let item_context = format!("{context}[{index}]");
        let item = as_object(value, &item_context)?;
        let mut ignored = Vec::new();
        warn_unknown_fields(
            item,
            &item_context,
            &["freq_hz", "db", "phase_deg"],
            &mut ignored,
        );
        let freq_hz = as_f64(
            req(&item, "freq_hz", &item_context)?,
            &format!("{item_context}.freq_hz"),
        )?;
        let db = as_f64(
            req(&item, "db", &item_context)?,
            &format!("{item_context}.db"),
        )?;
        let phase_deg = as_f64(
            req(&item, "phase_deg", &item_context)?,
            &format!("{item_context}.phase_deg"),
        )?;
        if !(freq_hz.is_finite() && freq_hz > 0.0 && freq_hz <= nyquist) {
            return Err(IngestError::range(
                format!("{item_context}.freq_hz"),
                format!("freq_hz {freq_hz} outside (0, {nyquist}]"),
            ));
        }
        if !db.is_finite() || !phase_deg.is_finite() {
            return Err(IngestError::range(
                item_context,
                "FR db/phase must be finite",
            ));
        }
        if index > 0 && freq_hz <= previous_freq {
            return Err(IngestError::schema(
                format!("{item_context}.freq_hz"),
                format!("frequencies must be strictly increasing ({previous_freq} -> {freq_hz})"),
            ));
        }
        previous_freq = freq_hz;
        points.push(FrPoint {
            freq_hz,
            db,
            phase_deg,
        });
    }
    Ok(points)
}

fn parse_analysis(
    json: &Json,
    session: &IngestSession,
    position: &PositionMeta,
    speaker: &SpeakerInfo,
    warnings: &mut Vec<IngestWarning>,
) -> Result<SourceAnalysis, IngestError> {
    let context = "analysis";
    let entries = as_object(json, context)?;
    warn_unknown_fields(
        entries,
        context,
        &[
            "schema",
            "channel",
            "delay_samples",
            "delay_ms",
            "delay_coarse_samples",
            "polarity",
            "peak_db",
            "phase_1k_deg",
            "snr_db",
            "phase_ref",
            "fr",
            "fr_modal",
            "meta",
            "repeats",
            "clock_ppm",
            "clock_ppm_ok",
            "clock_ppm_uncertainty_ppm",
            "clock_ppm_significant",
            "drift_corrected",
            "snr_band_db",
            "snr_band_hz",
            "onset_invalid",
            "polarity_confidence",
            "polarity_estimators",
            "polarity_robust",
            "polarity_listening",
            "polarity_source",
            "sub_polarity_basis",
            "reference",
            "measurement_role",
            "position",
            "reference_invalid",
            "reference_invalid_reason",
        ],
        warnings,
    );
    expect_schema(entries, ANALYSIS_SCHEMA, context)?;
    let channel = req_u64(entries, "channel", context)?;
    if channel != speaker.out_index as u64 {
        return Err(IngestError::schema(
            format!("{context}.channel"),
            format!(
                "analysis channel {channel} does not match speaker {:?} out_index {}",
                speaker.name, speaker.out_index
            ),
        ));
    }
    let delay_samples = f64_in_range(
        req_f64(entries, "delay_samples", context)?,
        -1_000_000.0,
        1_000_000.0,
        &format!("{context}.delay_samples"),
    )?;
    let delay_ms = f64_in_range(
        req_f64(entries, "delay_ms", context)?,
        -1_000.0,
        1_000.0,
        &format!("{context}.delay_ms"),
    )?;
    let delay_coarse_samples = req_i64(entries, "delay_coarse_samples", context)?;
    let polarity = req_i64(entries, "polarity", context)?;
    if !matches!(polarity, -1 | 0 | 1) {
        return Err(IngestError::range(
            format!("{context}.polarity"),
            format!("polarity {polarity} must be -1, 0 or 1"),
        ));
    }
    let peak_db = f64_in_range(
        req_f64(entries, "peak_db", context)?,
        -300.0,
        100.0,
        &format!("{context}.peak_db"),
    )?;
    let phase_1k_deg = req_f64(entries, "phase_1k_deg", context)?;
    let snr_db = f64_in_range(
        req_f64(entries, "snr_db", context)?,
        -300.0,
        300.0,
        &format!("{context}.snr_db"),
    )?;
    let phase_ref = req_string(entries, "phase_ref", context, 256)?;
    let clock_ppm = opt_f64(entries, "clock_ppm", context)?;
    if let Some(value) = clock_ppm {
        f64_in_range(value, -1000.0, 1000.0, &format!("{context}.clock_ppm"))?;
    }
    let clock_ppm_ok = opt_bool(entries, "clock_ppm_ok", context)?;
    if clock_ppm_ok == Some(false) {
        return Err(IngestError::timebase(
            format!("{context}.clock_ppm_ok"),
            "measurement declares an invalid clock estimate",
        ));
    }
    let clock_ppm_uncertainty_ppm = opt_f64(entries, "clock_ppm_uncertainty_ppm", context)?;
    if let Some(value) = clock_ppm_uncertainty_ppm {
        f64_in_range(
            value,
            0.0,
            1000.0,
            &format!("{context}.clock_ppm_uncertainty_ppm"),
        )?;
    }
    let clock_ppm_significant = opt_bool(entries, "clock_ppm_significant", context)?;
    let drift_corrected = opt_bool(entries, "drift_corrected", context)?;
    if clock_ppm_significant == Some(true) && drift_corrected != Some(true) {
        return Err(IngestError::timebase(
            format!("{context}.clock_ppm_significant"),
            "significant drift is declared but drift_corrected is not true",
        ));
    }
    let onset_invalid = opt_bool(entries, "onset_invalid", context)?;
    if onset_invalid == Some(true) {
        return Err(IngestError::timebase(
            format!("{context}.onset_invalid"),
            "measurement declares an invalid sweep onset",
        ));
    }
    let measurement_role = opt_string(entries, "measurement_role", context, 32)?;
    if measurement_role.as_deref() == Some("reference") {
        return Err(IngestError::unsupported(
            format!("{context}.measurement_role"),
            "reference-mic analysis must not be used as a participant source",
        ));
    }
    let repeats_count = match opt(entries, "repeats") {
        None | Some(Json::Null) => None,
        Some(value) => {
            let repeats_entries = as_object(value, &format!("{context}.repeats"))?;
            let count = req_u64(repeats_entries, "count", &format!("{context}.repeats"))?;
            if count == 0 || count > 8 {
                return Err(IngestError::range(
                    format!("{context}.repeats.count"),
                    format!("repeat count {count} outside [1, 8]"),
                ));
            }
            if count != session.stimulus.repeats as u64 {
                return Err(IngestError::schema(
                    format!("{context}.repeats.count"),
                    format!(
                        "analysis repeat count {count} does not match stimulus.repeats {}",
                        session.stimulus.repeats
                    ),
                ));
            }
            Some(count as u32)
        }
    };
    let snr_band_db = opt_f64(entries, "snr_band_db", context)?;
    if let Some(value) = snr_band_db {
        f64_in_range(value, -300.0, 300.0, &format!("{context}.snr_band_db"))?;
    }
    let snr_band_hz = match opt(entries, "snr_band_hz") {
        None | Some(Json::Null) => None,
        Some(value) => {
            let band = as_array(value, &format!("{context}.snr_band_hz"))?;
            if band.len() != 2 {
                return Err(IngestError::schema(
                    format!("{context}.snr_band_hz"),
                    "expected [start_hz, end_hz]",
                ));
            }
            let start = as_f64(&band[0], &format!("{context}.snr_band_hz[0]"))?;
            let end = as_f64(&band[1], &format!("{context}.snr_band_hz[1]"))?;
            if !(0.0 < start && start < end && end <= session.sample_rate_hz as f64 / 2.0) {
                return Err(IngestError::range(
                    format!("{context}.snr_band_hz"),
                    format!("invalid SNR band [{start}, {end}]"),
                ));
            }
            Some((start, end))
        }
    };
    let analysis_position = match opt(entries, "position") {
        None | Some(Json::Null) => None,
        Some(value) => {
            let position_entries = as_object(value, &format!("{context}.position"))?;
            warn_unknown_fields(
                position_entries,
                &format!("{context}.position"),
                &["name", "weight"],
                warnings,
            );
            Some(AnalysisPosition {
                name: req_string(position_entries, "name", &format!("{context}.position"), 64)?,
                weight: opt_f64(position_entries, "weight", &format!("{context}.position"))?,
            })
        }
    };
    if let Some(found) = &analysis_position {
        if found.name != position.name {
            return Err(IngestError::schema(
                format!("{context}.position.name"),
                format!(
                    "analysis position {:?} does not match position.json name {:?}",
                    found.name, position.name
                ),
            ));
        }
        if let Some(weight) = found.weight {
            if !approx_eq(weight, position.weight, 1e-9) {
                return Err(IngestError::schema(
                    format!("{context}.position.weight"),
                    format!(
                        "analysis position weight {weight} does not match position.json weight {}",
                        position.weight
                    ),
                ));
            }
        }
    }

    let fr = parse_fr(
        req_array(entries, "fr", context)?,
        context,
        session.sample_rate_hz,
    )?;
    if fr.len() < 2 {
        return Err(IngestError::schema(
            format!("{context}.fr"),
            "at least 2 FR points are required",
        ));
    }
    let fr_modal = match opt(entries, "fr_modal") {
        None | Some(Json::Null) => Vec::new(),
        Some(value) => {
            let values = as_array(value, &format!("{context}.fr_modal"))?;
            if values.is_empty() {
                Vec::new()
            } else {
                let points = parse_fr(
                    values,
                    &format!("{context}.fr_modal"),
                    session.sample_rate_hz,
                )?;
                if points.len() < 2 {
                    return Err(IngestError::schema(
                        format!("{context}.fr_modal"),
                        "non-empty fr_modal needs at least 2 points",
                    ));
                }
                points
            }
        }
    };

    let meta_entries = as_object(req(entries, "meta", context)?, &format!("{context}.meta"))?;
    warn_unknown_fields(
        meta_entries,
        &format!("{context}.meta"),
        &[
            "schema",
            "sample_rate",
            "channels",
            "sweep_start_hz",
            "sweep_end_hz",
            "sweep_seconds",
            "level_db",
            "pre_seconds",
            "post_seconds",
            "playback_device",
            "playback_channels",
            "capture_device",
            "capture_channels",
            "created_unix",
        ],
        warnings,
    );
    expect_schema(
        meta_entries,
        ANALYSIS_META_SCHEMA,
        &format!("{context}.meta"),
    )?;
    let meta_sample_rate = req_u64(meta_entries, "sample_rate", &format!("{context}.meta"))?;
    if meta_sample_rate != session.sample_rate_hz as u64 {
        return Err(IngestError::schema(
            format!("{context}.meta.sample_rate"),
            format!(
                "analysis sample_rate {meta_sample_rate} does not match session sample_rate_hz {}",
                session.sample_rate_hz
            ),
        ));
    }
    let meta = AnalysisMeta {
        sample_rate_hz: meta_sample_rate as u32,
        playback_channels: parse_channels(
            req_u64(meta_entries, "channels", &format!("{context}.meta"))?,
            MAX_OUTPUT_CHANNELS,
            &format!("{context}.meta.channels"),
        )?,
        sweep_start_hz: req_f64(meta_entries, "sweep_start_hz", &format!("{context}.meta"))?,
        sweep_end_hz: req_f64(meta_entries, "sweep_end_hz", &format!("{context}.meta"))?,
        sweep_seconds: req_f64(meta_entries, "sweep_seconds", &format!("{context}.meta"))?,
        level_dbfs: req_f64(meta_entries, "level_db", &format!("{context}.meta"))?,
        pre_seconds: req_f64(meta_entries, "pre_seconds", &format!("{context}.meta"))?,
        post_seconds: req_f64(meta_entries, "post_seconds", &format!("{context}.meta"))?,
        playback_device: req_string(
            meta_entries,
            "playback_device",
            &format!("{context}.meta"),
            128,
        )?,
        capture_channels: parse_channels(
            req_u64(meta_entries, "capture_channels", &format!("{context}.meta"))?,
            MAX_INPUT_CHANNELS,
            &format!("{context}.meta.capture_channels"),
        )?,
        capture_device: req_string(
            meta_entries,
            "capture_device",
            &format!("{context}.meta"),
            128,
        )?,
        created_unix: req_u64(meta_entries, "created_unix", &format!("{context}.meta"))?,
    };
    if meta.playback_channels != session.playback.channels {
        return Err(IngestError::schema(
            format!("{context}.meta.channels"),
            format!(
                "analysis playback channels {} do not match session playback.channels {}",
                meta.playback_channels, session.playback.channels
            ),
        ));
    }
    if meta.capture_channels != session.capture.channels {
        return Err(IngestError::schema(
            format!("{context}.meta.capture_channels"),
            format!(
                "analysis capture channels {} do not match session capture.channels {}",
                meta.capture_channels, session.capture.channels
            ),
        ));
    }
    if meta.playback_device != session.playback.device {
        return Err(IngestError::schema(
            format!("{context}.meta.playback_device"),
            format!(
                "analysis playback device {:?} does not match session {:?}",
                meta.playback_device, session.playback.device
            ),
        ));
    }
    if meta.capture_device != session.capture.device {
        return Err(IngestError::schema(
            format!("{context}.meta.capture_device"),
            format!(
                "analysis capture device {:?} does not match session {:?}",
                meta.capture_device, session.capture.device
            ),
        ));
    }
    if !approx_eq(meta.sweep_start_hz, session.stimulus.start_hz, 1e-6) {
        return Err(IngestError::schema(
            format!("{context}.meta.sweep_start_hz"),
            format!(
                "analysis sweep start {} does not match session start {}",
                meta.sweep_start_hz, session.stimulus.start_hz
            ),
        ));
    }
    if meta.sweep_end_hz <= meta.sweep_start_hz
        || meta.sweep_end_hz > session.stimulus.end_hz + 1e-6
        || meta.sweep_end_hz > session.sample_rate_hz as f64 / 2.0 + 1e-6
    {
        return Err(IngestError::schema(
            format!("{context}.meta.sweep_end_hz"),
            format!(
                "analysis sweep end {} outside ({}, session end {}]",
                meta.sweep_end_hz, meta.sweep_start_hz, session.stimulus.end_hz
            ),
        ));
    }
    if !approx_eq(meta.sweep_seconds, session.stimulus.seconds, 1e-6) {
        return Err(IngestError::schema(
            format!("{context}.meta.sweep_seconds"),
            format!(
                "analysis sweep seconds {} do not match session {}",
                meta.sweep_seconds, session.stimulus.seconds
            ),
        ));
    }
    if !approx_eq(meta.level_dbfs, session.stimulus.level_dbfs, 1e-6) {
        return Err(IngestError::schema(
            format!("{context}.meta.level_db"),
            format!(
                "analysis level {} does not match session level {}",
                meta.level_dbfs, session.stimulus.level_dbfs
            ),
        ));
    }
    if !approx_eq(meta.pre_seconds, session.capture.pre_seconds, 1e-6)
        || !approx_eq(meta.post_seconds, session.capture.post_seconds, 1e-6)
    {
        return Err(IngestError::schema(
            format!("{context}.meta.pre_seconds"),
            "analysis pre/post guard do not match session capture guard",
        ));
    }
    if meta.created_unix == 0 {
        return Err(IngestError::range(
            format!("{context}.meta.created_unix"),
            "created_unix must be > 0",
        ));
    }

    Ok(SourceAnalysis {
        channel: channel as u32,
        delay_samples,
        delay_ms,
        delay_coarse_samples,
        polarity: polarity as i32,
        peak_db,
        phase_1k_deg,
        snr_db,
        snr_band_db,
        snr_band_hz,
        phase_ref,
        clock_ppm,
        clock_ppm_ok,
        clock_ppm_uncertainty_ppm,
        clock_ppm_significant,
        drift_corrected,
        onset_invalid,
        measurement_role,
        position: analysis_position,
        repeats_count,
        fr,
        fr_modal,
        meta,
    })
}

// ===========================================================================
// raw 窗口 / 跨文件一致性
// ===========================================================================

fn validate_timebase_window(
    diag: &TimebaseDiag,
    capture: &CaptureFile,
    session: &IngestSession,
    context: &str,
) -> Result<(), IngestError> {
    let frames = capture.frames as f64;
    let fs = session.sample_rate_hz as f64;
    if diag.capture_file != CAPTURE_FILE {
        return Err(IngestError::unsupported(
            format!("{context}.capture_file"),
            "ingest contract expects capture.raw",
        ));
    }
    if (diag.capture_sample_index as f64) >= frames {
        return Err(IngestError::timebase(
            format!("{context}.capture_sample_index"),
            format!(
                "capture sample index {} outside capture length {frames} frames",
                diag.capture_sample_index
            ),
        ));
    }
    if diag.actual_start > frames + 2.0 {
        return Err(IngestError::timebase(
            format!("{context}.actual_start"),
            format!(
                "actual_start {} beyond capture length {frames}",
                diag.actual_start
            ),
        ));
    }
    let sweep_frames = session.stimulus.seconds * fs;
    let gap_frames = session.stimulus.repeat_gap_seconds * fs;
    let pre_frames = session.capture.pre_seconds * fs;
    let post_frames = session.capture.post_seconds * fs;
    if diag.repeat_starts[0] < pre_frames - 2.0 {
        return Err(IngestError::timebase(
            format!("{context}.repeat_starts[0]"),
            format!(
                "first sweep start {} precedes declared pre-guard {} frames",
                diag.repeat_starts[0], pre_frames
            ),
        ));
    }
    let last_start = *diag.repeat_starts.last().unwrap_or(&diag.actual_start);
    let last_end = last_start + sweep_frames;
    if last_end > frames - post_frames + 2.0 {
        return Err(IngestError::timebase(
            format!("{context}.repeat_starts"),
            format!(
                "last sweep end {last_end} exceeds capture {frames} minus post-guard {post_frames}"
            ),
        ));
    }
    let expected_interval = sweep_frames + gap_frames;
    let tolerance = 0.05 * expected_interval + 2.0;
    for window in diag.repeat_starts.windows(2) {
        let delta = window[1] - window[0];
        if (delta - expected_interval).abs() > tolerance {
            return Err(IngestError::timebase(
                format!("{context}.repeat_starts"),
                format!(
                    "repeat interval {delta} frames deviates from expected {expected_interval} by more than {tolerance}"
                ),
            ));
        }
    }
    Ok(())
}

fn warn_ignored_position_files(
    reader: &IngestReader,
    position_dir: &str,
    expected: &BTreeSet<String>,
    warnings: &mut Vec<IngestWarning>,
) -> Result<(), IngestError> {
    let dir = reader.root.join(POSITIONS_DIR).join(position_dir);
    let entries = fs::read_dir(&dir)
        .map_err(|error| IngestError::io(format!("{POSITIONS_DIR}/{position_dir}"), error))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| IngestError::io(format!("{POSITIONS_DIR}/{position_dir}"), error))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".analysis.json") || name.ends_with(".diag.json")) {
            continue;
        }
        let relative = format!("{POSITIONS_DIR}/{position_dir}/{name}");
        if !expected.contains(&relative) {
            push_warning(
                warnings,
                IngestWarning::IGNORED_FILE,
                format!("{relative}: not part of participants; ignored"),
            );
        }
    }
    Ok(())
}

fn read_positions(
    reader: &IngestReader,
    session: &IngestSession,
    warnings: &mut Vec<IngestWarning>,
) -> Result<Vec<IngestPosition>, IngestError> {
    let positions_root = reader.root.join(POSITIONS_DIR);
    let metadata = fs::symlink_metadata(&positions_root).map_err(|error| {
        IngestError::missing(
            POSITIONS_DIR,
            format!("positions directory unavailable: {error}"),
        )
        .with_path(&positions_root)
    })?;
    if !metadata.is_dir() {
        return Err(
            IngestError::missing(POSITIONS_DIR, "positions must be a directory")
                .with_path(&positions_root),
        );
    }
    let mut dir_entries: Vec<String> = Vec::new();
    let read_dir = fs::read_dir(&positions_root)
        .map_err(|error| IngestError::io(POSITIONS_DIR, error).with_path(&positions_root))?;
    for entry in read_dir {
        let entry = entry
            .map_err(|error| IngestError::io(POSITIONS_DIR, error).with_path(&positions_root))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry
            .file_type()
            .map_err(|error| IngestError::io(format!("{POSITIONS_DIR}/{name}"), error))?;
        if file_type.is_symlink() {
            return Err(IngestError::unsupported(
                format!("{POSITIONS_DIR}/{name}"),
                "symlinked position directories are not allowed",
            ));
        }
        if !file_type.is_dir() {
            push_warning(
                warnings,
                IngestWarning::IGNORED_FILE,
                format!("{POSITIONS_DIR}/{name}: not a directory; ignored"),
            );
            continue;
        }
        if !is_id_token(&name, 128) {
            return Err(IngestError::schema(
                format!("{POSITIONS_DIR}/{name}"),
                "position directory name is not a safe label token",
            ));
        }
        dir_entries.push(name);
    }
    if dir_entries.is_empty() {
        return Err(IngestError::missing(
            POSITIONS_DIR,
            "no position directories found",
        ));
    }
    if dir_entries.len() > MAX_POSITIONS {
        return Err(IngestError::range(
            POSITIONS_DIR,
            format!("more than {MAX_POSITIONS} positions"),
        ));
    }
    dir_entries.sort();

    let mut positions: Vec<IngestPosition> = Vec::new();
    let mut labels: BTreeSet<String> = BTreeSet::new();
    let mut common_timebase: Option<(String, u64, String)> = None;
    for dir_name in dir_entries {
        let position_rel = format!("{POSITIONS_DIR}/{dir_name}/{POSITION_FILE}");
        let (position_json, meta_sha) =
            reader.read_json(&position_rel, MAX_POSITION_BYTES, &position_rel)?;
        let meta = parse_position(&position_json, warnings)?;
        if !labels.insert(meta.name.clone()) {
            return Err(IngestError::duplicate(
                POSITIONS_DIR,
                format!("duplicate position label {:?}", meta.name),
            ));
        }
        let capture_rel = format!("{POSITIONS_DIR}/{dir_name}/{CAPTURE_FILE}");
        let capture = reader.capture(
            &capture_rel,
            session.capture.channels,
            session.sample_rate_hz,
            session.capture.sample_format,
            &capture_rel,
        )?;
        let reference = match &session.reference {
            None => None,
            Some(info) => {
                let reference_rel = format!("{POSITIONS_DIR}/{dir_name}/{REFERENCE_FILE}");
                let reference = reader.capture(
                    &reference_rel,
                    info.channels,
                    session.sample_rate_hz,
                    info.sample_format,
                    &reference_rel,
                )?;
                let frame_gap = (reference.frames as i128 - capture.frames as i128).abs();
                if frame_gap > session.sample_rate_hz as i128 {
                    return Err(IngestError::audio(
                        &reference_rel,
                        format!(
                            "reference length {} frames differs from UMIK capture {} frames by more than 1 s",
                            reference.frames, capture.frames
                        ),
                    ));
                }
                Some(reference)
            }
        };

        let mut expected_files: BTreeSet<String> = BTreeSet::new();
        expected_files.insert(capture_rel);
        if reference.is_some() {
            expected_files.insert(format!("{POSITIONS_DIR}/{dir_name}/{REFERENCE_FILE}"));
        }
        let mut measurements: Vec<IngestMeasurement> = Vec::new();
        for speaker_name in &session.participants {
            let speaker = session
                .speakers
                .iter()
                .find(|candidate| &candidate.name == speaker_name)
                .ok_or_else(|| {
                    IngestError::schema(
                        format!("{SESSION_FILE}.participants"),
                        format!("participant {speaker_name:?} missing from speakers"),
                    )
                })?;
            let base = format!("{speaker_name}-ch{:02}", speaker.out_index);
            let analysis_rel = format!("{POSITIONS_DIR}/{dir_name}/{base}.analysis.json");
            let diag_rel = format!("{POSITIONS_DIR}/{dir_name}/{base}.diag.json");
            expected_files.insert(analysis_rel.clone());
            expected_files.insert(diag_rel.clone());
            let (analysis_json, analysis_sha) =
                reader.read_json(&analysis_rel, MAX_ANALYSIS_BYTES, &analysis_rel)?;
            let analysis = parse_analysis(&analysis_json, session, &meta, speaker, warnings)?;
            let (diag_json, diag_sha) = reader.read_json(&diag_rel, MAX_DIAG_BYTES, &diag_rel)?;
            let diag = parse_diag(&diag_json, &diag_rel, session, speaker)?;
            validate_timebase_window(&diag, &capture, session, &diag_rel)?;
            let timebase = (
                diag.session_id.clone(),
                diag.epoch_id,
                diag.graph_id.clone(),
            );
            match &common_timebase {
                None => common_timebase = Some(timebase),
                Some(expected) => {
                    if expected != &timebase {
                        return Err(IngestError::timebase(
                            &diag_rel,
                            format!(
                                "public timebase mismatch across positions: expected {expected:?}, found {timebase:?}"
                            ),
                        ));
                    }
                }
            }
            measurements.push(IngestMeasurement {
                speaker: speaker_name.clone(),
                out_index: speaker.out_index,
                slot: diag.slot,
                analysis_relative_path: analysis_rel,
                analysis_sha256: analysis_sha,
                diag_relative_path: diag_rel,
                diag_sha256: diag_sha,
                analysis,
                diag,
            });
        }
        measurements.sort_by_key(|measurement| measurement.slot);
        for (index, measurement) in measurements.iter().enumerate() {
            if measurement.slot != index as u32 {
                return Err(IngestError::timebase(
                    &measurement.diag_relative_path,
                    format!(
                        "slot coverage must be contiguous 0..{}; found slot {} at index {index}",
                        measurements.len() - 1,
                        measurement.slot
                    ),
                ));
            }
        }
        for pair in measurements.windows(2) {
            if pair[1].diag.render_frame_index <= pair[0].diag.render_frame_index {
                return Err(IngestError::timebase(
                    &pair[1].diag_relative_path,
                    "render_frame_index must strictly increase with slot order",
                ));
            }
            if pair[1].diag.capture_sample_index <= pair[0].diag.capture_sample_index {
                return Err(IngestError::timebase(
                    &pair[1].diag_relative_path,
                    "capture_sample_index must strictly increase with slot order",
                ));
            }
        }
        warn_ignored_position_files(reader, &dir_name, &expected_files, warnings)?;
        positions.push(IngestPosition {
            dir: dir_name,
            meta,
            meta_sha256: meta_sha,
            capture,
            reference,
            measurements,
        });
    }
    Ok(positions)
}

/// 只读导入一个 session 目录（schema/时轴/hash 全部校验；不采音、不写盘）。
///
/// 成功返回强类型 [IngestBundle]；失败返回稳定 [IngestError]（code 见 [IngestErrorKind::code]）。
/// 所有输入文件必须被 [SHA256SUMS_FILE] 覆盖且字节 hash 一致；session.json、
/// positions/*/position.json、positions/*/capture.raw、每 participant 的 analysis/diag 均必需。
/// 缺公共时轴（render frame/slot/mic clock）→ E_INGEST_TIMEBASE。
pub fn read_bundle(root: impl AsRef<Path>) -> Result<IngestBundle, IngestError> {
    let root = root.as_ref();
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| IngestError::io("ingest.root", error).with_path(root))?;
    if metadata.file_type().is_symlink() {
        return Err(IngestError::unsupported(
            "ingest.root",
            "symlinked session root is not allowed",
        )
        .with_path(root));
    }
    if !metadata.is_dir() {
        return Err(
            IngestError::missing("ingest.root", "session root is not a directory").with_path(root),
        );
    }
    let reader = IngestReader::new(root)?;
    let mut warnings: Vec<IngestWarning> = Vec::new();
    let (session_json, session_sha) =
        reader.read_json(SESSION_FILE, MAX_SESSION_BYTES, SESSION_FILE)?;
    let session = parse_session(&session_json, &mut warnings)?;
    let positions = read_positions(&reader, &session, &mut warnings)?;
    if positions.is_empty() {
        return Err(IngestError::missing(POSITIONS_DIR, "no positions ingested"));
    }
    if !positions.iter().any(|position| position.is_design()) {
        return Err(IngestError::missing(
            "positions",
            "at least one design position is required for solver ingest",
        ));
    }
    let files: Vec<IngestFileRecord> = reader
        .verified
        .iter()
        .map(|(relative_path, file)| IngestFileRecord {
            relative_path: relative_path.clone(),
            bytes: file.bytes,
            sha256: file.sha256.clone(),
        })
        .collect();
    Ok(IngestBundle {
        root: root.to_path_buf(),
        session,
        session_sha256: session_sha,
        positions,
        files,
        warnings,
    })
}

// ===========================================================================
// 单测（primitives/JSON/hash/静态守卫；文件系统场景在 tests/T48/main.rs）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Json, IngestError> {
        JsonParser::parse(text)
    }

    fn parse_err(text: &str) -> IngestError {
        match parse(text) {
            Ok(value) => panic!("expected a JSON error, got {value:?}"),
            Err(error) => error,
        }
    }

    #[test]
    fn ingest_sha256_known_answers() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        let sample: Vec<u8> = (0..=200u8).collect();
        let mut chunked = Sha256::new();
        for chunk in sample.chunks(7) {
            chunked.update(chunk);
        }
        assert_eq!(hex_encode(&chunked.finalize()), sha256_hex(&sample));
        assert_eq!(hex_encode(&Sha256::new().finalize()), sha256_hex(b""));
    }

    #[test]
    fn ingest_json_parser_accepts_valid_json() {
        let value =
            parse(r#"{"a":1,"b":[true,false,null,-2.5e3],"c":"x\ny"}"#).expect("valid json");
        let entries = as_object(&value, "root").expect("object");
        assert_eq!(entries.len(), 3);
        let array = as_array(opt(entries, "b").expect("b"), "root.b").expect("array");
        assert_eq!(array.len(), 4);
        let number = as_f64(&array[3], "root.b[3]").expect("number");
        assert_eq!(number, -2500.0);
        assert_eq!(as_string(opt(entries, "c").unwrap(), "c").unwrap(), "x\ny");
    }

    #[test]
    fn ingest_json_parser_rejects_malformed_json() {
        assert_eq!(parse_err("").kind(), IngestErrorKind::Json);
        assert_eq!(parse_err("{").kind(), IngestErrorKind::Json);
        assert_eq!(
            parse_err(r#"{"a":1} trailing"#).kind(),
            IngestErrorKind::Json
        );
        assert_eq!(parse_err(r#"{"a":01}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a":NaN}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a":Infinity}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a":1e999}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a":1,}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a" 1}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#""unterminated"#).kind(), IngestErrorKind::Json);
        assert_eq!(
            parse_err("{\"a\":\"raw\tcontrol\"}").kind(),
            IngestErrorKind::Json
        );
        assert_eq!(parse_err(r#"{"a":}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err("{\"a\":\"\\q\"}").kind(), IngestErrorKind::Json);
    }

    #[test]
    fn ingest_json_parser_rejects_duplicate_keys() {
        let error = parse_err(r#"{"a":1,"a":2}"#);
        assert_eq!(error.kind(), IngestErrorKind::Json);
        assert!(error.detail().contains("duplicate object key"));
    }

    #[test]
    fn ingest_json_parser_handles_unicode_escapes() {
        let value = parse(r#"{"name":"\u4e2d\u6587","emoji":"\ud83d\ude00"}"#).expect("json");
        let entries = as_object(&value, "root").expect("object");
        assert_eq!(
            as_string(opt(entries, "name").unwrap(), "name").unwrap(),
            "中文"
        );
        assert_eq!(
            as_string(opt(entries, "emoji").unwrap(), "emoji").unwrap(),
            "😀"
        );
        assert_eq!(parse_err(r#"{"a":"\ud83d"}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a":"\ude00"}"#).kind(), IngestErrorKind::Json);
        assert_eq!(parse_err(r#"{"a":"\u12"}"#).kind(), IngestErrorKind::Json);
    }

    #[test]
    fn ingest_json_depth_is_bounded() {
        let deeply_nested = format!(
            "{}{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        assert_eq!(parse_err(&deeply_nested).kind(), IngestErrorKind::Json);
    }

    #[test]
    fn ingest_path_safety_rules() {
        assert!(safe_relative_path("session.json").is_ok());
        assert!(safe_relative_path("positions/p01-mlp/capture.raw").is_ok());
        assert_eq!(
            safe_relative_path("../secret").unwrap_err().kind(),
            IngestErrorKind::Unsupported
        );
        assert_eq!(
            safe_relative_path("/abs/path").unwrap_err().kind(),
            IngestErrorKind::Unsupported
        );
        assert_eq!(
            safe_relative_path("a\\b").unwrap_err().kind(),
            IngestErrorKind::Unsupported
        );
        assert_eq!(
            safe_relative_path("").unwrap_err().kind(),
            IngestErrorKind::Schema
        );
    }

    #[test]
    fn ingest_role_and_format_parsing() {
        assert_eq!(PositionRole::parse("design"), Some(PositionRole::Design));
        assert_eq!(
            PositionRole::parse("validation"),
            Some(PositionRole::Validation)
        );
        assert_eq!(PositionRole::parse("Design"), None);
        assert_eq!(SampleFormat::parse("S24_3LE"), Some(SampleFormat::S24_3LE));
        assert_eq!(SampleFormat::parse("s24_3le"), None);
        assert_eq!(SampleFormat::S32LE.bytes_per_sample(), 4);
        assert_eq!(SampleFormat::S24_3LE.as_str(), "S24_3LE");
        assert_eq!(
            DriftCorrection::parse("resample_sinc"),
            Some(DriftCorrection::Resample)
        );
        assert!(is_role_id("SUB1"));
        assert!(!is_role_id("sub1"));
        assert!(!is_role_id("FL_"));
        let hex = "a".repeat(64);
        assert!(is_lower_hex_64(&hex));
        let upper = "A".repeat(64);
        assert!(!is_lower_hex_64(&upper));
        assert!(!is_lower_hex_64("abc"));
    }

    #[test]
    fn ingest_f64_range_and_approx_rules() {
        assert_eq!(f64_in_range(1.0, 0.0, 2.0, "x").unwrap(), 1.0);
        assert_eq!(
            f64_in_range(-0.1, 0.0, 2.0, "x").unwrap_err().kind(),
            IngestErrorKind::Range
        );
        assert_eq!(
            f64_in_range(f64::NAN, 0.0, 2.0, "x").unwrap_err().kind(),
            IngestErrorKind::Range
        );
        assert!(approx_eq(1.0, 1.0 + 1e-9, 1e-6));
        assert!(!approx_eq(1.0, 1.1, 1e-6));
    }

    #[test]
    fn ingest_error_codes_are_stable() {
        let cases = [
            (IngestErrorKind::Io, "E_INGEST_IO"),
            (IngestErrorKind::Json, "E_INGEST_JSON"),
            (IngestErrorKind::Schema, "E_INGEST_SCHEMA"),
            (IngestErrorKind::Missing, "E_INGEST_MISSING"),
            (IngestErrorKind::Duplicate, "E_INGEST_DUPLICATE"),
            (IngestErrorKind::Range, "E_INGEST_RANGE"),
            (IngestErrorKind::Timebase, "E_INGEST_TIMEBASE"),
            (IngestErrorKind::Hash, "E_INGEST_HASH"),
            (IngestErrorKind::Unsupported, "E_INGEST_UNSUPPORTED"),
            (IngestErrorKind::Audio, "E_INGEST_AUDIO"),
        ];
        for (kind, code) in cases {
            assert_eq!(kind.code(), code);
        }
    }

    #[test]
    fn ingest_source_is_read_only_and_does_not_capture_audio() {
        let source = ingest_source_text_for_guards();
        let forbidden = [
            ["fs", "::", "write"].concat(),
            ["File", "::", "create"].concat(),
            ["Open", "Options"].concat(),
            ["remove", "_file"].concat(),
            ["std", "::", "process"].concat(),
            ["Command", "::", "new"].concat(),
            ["are", "cord"].concat(),
            ["a", "play"].concat(),
            ["cp", "al"].concat(),
            ["snd", "_pcm"].concat(),
        ];
        for token in forbidden {
            assert!(
                !source.contains(&token),
                "forbidden token found in ingest.rs: {token}"
            );
        }
        assert!(source.contains("[NONRT]"));
    }
}
