//! [NONRT] `khaen-file-runner/v1` session manifest：解析/校验/blob 源（自有格式）。
//!
//! session 是 runner 的唯一输入描述：音频布局/引擎版本/role 映射/A-G-P 系数文件与 manifest 引用/
//! 校准 IR 绑定/保护策略。它由 T26 host（或证据生成脚本）构造；runner 只读。
//!
//! # 最小形状（完整字段见 [`Session`]）
//!
//! ~~~text
//! {
//!   "schema": "khaen-file-runner/v1",
//!   "precision": "f32-strict",
//!   "audio": { "sample_rate_hz": 48000, "container_channels": 8, "input_channels": 2,
//!              "layout_id": "4.1.2", "input": { "file": "input.wav", "sha256": "<可选>" },
//!              "binding": { "profile_sha256": "<64hex>", "calibration_sha256": "<64hex>",
//!                           "target_sha256": null } },
//!   "engine": { "abi_version": 1, "plan_version": 1, "block_size": 128, "expected_workers": 4,
//!               "profile": "regular", "max_taps": 4096, "extended_taps_admitted": false,
//!               "graph_id": "<可选 64hex；提供则必须匹配重算>" },
//!   "roles": [ { "name": "FL", "index": 0, "src": 0, "scatter": 0 }, ... ],
//!   "participants": ["FL", "FR", "SL", "SR", "SUB1"],
//!   "d_samples": 0,
//!   "coefficients": { "a": [ { "src": 0, "file": "a0.wav", "manifest_file": "a0.fir.json" } ],
//!                     "g": [ { "row": 0, "col": 1, "file": "g01.wav", "manifest_file": "..." } ],
//!                     "p": [ { "role": "FL", "file": "pFL.wav", "manifest_file": "..." } ] },
//!   "ir": { ... `tspi-cal-ir-draft/v1` ... } | null,
//!   "protection": { "fallback": "identity_p_post", "triggers": ["graph_hash_mismatch","ir_stale"],
//!                   "mute_on_exec_fault": true },
//!   "output": { "format": "wav_f32" }
//! }
//! ~~~
//!
//! 系数槽可用 `manifest` 内联对象或 `manifest_file` 引用 blob；二者互斥，引用名由
//! [`SourceProvider`] 解析（测试用内存 map，host 用文件系统；禁止绝对路径/.. 逃逸）。
//!
//! 本模块只做**结构/范围/一致性**校验；系数 manifest 的数值校验在 [`crate::fir`]，图与执行在
//! [`crate::graph`]/[`crate::runner`]。未知顶层字段只允许 `x_` 前缀（其余拒收）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::fir::PROFILE_REGULAR;
use crate::ir::{self, IrCurrent, IrDraft, StaleReason};
use crate::json::Json;
use crate::sha256::is_hex64;
use crate::{
    RunnerError, RunnerResult, E_FILE_BLOB, E_FILE_CHANNELS, E_FILE_FIELD, E_FILE_GRAPH,
    E_FILE_PATH_TABLE, E_FILE_PRECISION, E_FILE_PROFILE, E_FILE_RATE, E_FILE_ROLES, E_FILE_SCHEMA,
    E_FILE_VERSION, FALLBACK_IDENTITY_P_POST, FALLBACK_MUTE, PRECISION_LOCK, SESSION_SCHEMA,
    TRIGGER_COMPILE_REJECT, TRIGGER_GRAPH_HASH_MISMATCH, TRIGGER_IR_ABSENT, TRIGGER_IR_STALE,
};

/// 运行时允许的采样率（core-v1 §3.1；与 core::rt::SAMPLE_RATES_HZ 同源）。
pub const RUNTIME_SAMPLE_RATES: [u32; 2] = [48_000, 96_000];
/// 运行时允许的 block 大小（core-v1 §3.2）。
pub const RUNTIME_BLOCK_SIZES: [u16; 2] = [64, 128];
/// 最大输出容器通道（core-v1 MAX_OUTPUT_CHANNELS=22）。
pub const MAX_CONTAINER_CHANNELS: u16 = 22;
/// 最大输入通道（core-v1 §3.1 / core::rt::MAX_INPUT_CHANNELS=20）。
pub const MAX_INPUT_CHANNELS: u16 = 20;
/// 最大输出角色（core-v1 / core::rt::MAX_OUTPUT_ROLES=16）。
pub const MAX_ROLES: usize = 16;
/// 低带 participants 上限（core-v1 MAX_LOW_BAND_PARTICIPANTS=8）。
pub const MAX_PARTICIPANTS: usize = 8;
/// 输出格式（v1 仅 f32 WAV）。
pub const OUTPUT_FORMAT_WAV_F32: &str = "wav_f32";

/// blob 源抽象：按 session 内相对引用提供文件字节（WAV/manifest）。
///
/// 实现者必须拒绝绝对路径与 `..` 逃逸（[`FsSource`] 已实现）；测试用 [`MapSource`]。
pub trait SourceProvider {
    /// 读取引用字节；缺失/逃逸 -> `E_FILE_BLOB`。
    fn load(&self, reference: &str) -> RunnerResult<Vec<u8>>;
}

/// 内存 blob 源（测试/证据生成；引用键与 session 内文件名一一对应）。
#[derive(Clone, Debug, Default)]
pub struct MapSource {
    files: BTreeMap<String, Vec<u8>>,
}

impl MapSource {
    /// 空源。
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入一个引用（重复键覆盖）。
    pub fn insert(&mut self, reference: impl Into<String>, bytes: Vec<u8>) {
        self.files.insert(reference.into(), bytes);
    }

    /// 构造者风格插入。
    pub fn with(mut self, reference: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.insert(reference, bytes);
        self
    }

    /// 引用数量。
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl SourceProvider for MapSource {
    fn load(&self, reference: &str) -> RunnerResult<Vec<u8>> {
        self.files.get(reference).cloned().ok_or_else(|| {
            RunnerError::new(
                E_FILE_BLOB,
                format!("blob '{}' not found in memory source", reference),
            )
        })
    }
}

/// 文件系统 blob 源（host/证据；根目录 + 相对引用；拒绝绝对路径与 `..` 逃逸）。
#[derive(Clone, Debug)]
pub struct FsSource {
    root: PathBuf,
}

impl FsSource {
    /// 以 root 为 blob 根。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SourceProvider for FsSource {
    fn load(&self, reference: &str) -> RunnerResult<Vec<u8>> {
        if reference.is_empty() {
            return Err(RunnerError::new(E_FILE_BLOB, "empty blob reference"));
        }
        let relative = Path::new(reference);
        if relative.is_absolute() {
            return Err(RunnerError::new(
                E_FILE_BLOB,
                format!("absolute blob reference '{}' is not allowed", reference),
            ));
        }
        for component in relative.components() {
            match component {
                Component::Normal(_) => {}
                _ => {
                    return Err(RunnerError::new(
                        E_FILE_BLOB,
                        format!("blob reference '{}' escapes root", reference),
                    ))
                }
            }
        }
        let path = self.root.join(relative);
        std::fs::read(&path).map_err(|err| {
            RunnerError::new(
                E_FILE_BLOB,
                format!("failed to read blob '{}': {}", reference, err),
            )
        })
    }
}

/// 音频/布局输入声明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSpec {
    /// 采样率（48k/96k）。
    pub sample_rate_hz: u32,
    /// 播放容器通道数（C）。
    pub container_channels: u16,
    /// 输入源通道数（N）。
    pub input_channels: u16,
    /// layout id（如 4.1.2）。
    pub layout_id: String,
    /// 输入 WAV blob 引用。
    pub input_file: String,
    /// 输入文件字节 SHA-256（可选；提供则必须匹配）。
    pub input_sha256: Option<String>,
    /// 渲染 profile hash（可选；IR stale 判定用）。
    pub profile_sha256: Option<String>,
    /// 源校准文件 hash（可选；IR stale 判定用）。
    pub calibration_sha256: Option<String>,
    /// 目标曲线 hash（可选；None 且提供 binding 时表示“当前无曲线”）。
    pub target_sha256: Option<String>,
    /// 是否提供 binding（true 时 profile/target/calibration 参与 stale 判定）。
    pub binding_present: bool,
}

/// 引擎/版本声明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineSpec {
    /// ABI 版本（必须 =1）。
    pub abi_version: u16,
    /// plan 提交计数（>=1；不参与图 hash）。
    pub plan_version: u32,
    /// block 大小（64/128）。
    pub block_size: u16,
    /// 静态 DAG worker 数（1/2/4；1 仅测试/benchmark）。
    pub expected_workers: u8,
    /// 执行 profile（regular/extended）。
    pub profile: String,
    /// 每 route taps 上限。
    pub max_taps: u32,
    /// extended profile 是否已显式准入。
    pub extended_taps_admitted: bool,
    /// 期望图 hash（可选；提供则必须匹配）。
    pub graph_id: Option<String>,
}

/// role -> 源/容器映射（A_map 与 scatter 的声明输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleMap {
    /// role id（唯一、非空）。
    pub name: String,
    /// role 序号（必须 = 数组下标，0..O-1）。
    pub index: u16,
    /// 选中的源通道（None = 整行零填，不复制分发）。
    pub src: Option<u16>,
    /// 播放容器通道（单射）。
    pub scatter: u16,
}

/// 系数槽类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoefficientKind {
    /// 输入侧 A：源 src 的 FIR。
    Input {
        /// 源通道索引。
        src: u16,
    },
    /// 低带矩阵 G：participants dense 位置 (row, col)。
    Matrix {
        /// dense 行号（0..M-1）。
        row: u16,
        /// dense 列号（0..M-1）。
        col: u16,
    },
    /// 输出侧 P：role 的 FIR。
    Output {
        /// role 序号（输出角色）。
        role: u16,
    },
}

/// 一个系数文件槽（file + manifest 引用）。
#[derive(Clone, Debug)]
pub struct CoeffSlot {
    /// 槽类型。
    pub kind: CoefficientKind,
    /// 系数文件引用（WAV/raw）。
    pub file: String,
    /// manifest 引用（blob key；与 inline 互斥）。
    pub manifest_file: Option<String>,
    /// 内联 manifest 对象（与 manifest_file 互斥）。
    pub manifest_inline: Option<Json>,
}

/// 保护策略（binding 失效/执行故障的安全行为）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectionPolicy {
    /// `identity_p_post`（G=I 保 P_post）或 `mute`（全零）。
    pub fallback: String,
    /// 触发保护的原因集合（graph_hash_mismatch/ir_stale/compile_reject/ir_absent）。
    pub triggers: Vec<String>,
    /// 执行故障时是否安全静音（默认 true）。
    pub mute_on_exec_fault: bool,
}

impl Default for ProtectionPolicy {
    fn default() -> Self {
        Self {
            fallback: FALLBACK_IDENTITY_P_POST.to_string(),
            triggers: vec![
                TRIGGER_GRAPH_HASH_MISMATCH.to_string(),
                TRIGGER_IR_STALE.to_string(),
                TRIGGER_COMPILE_REJECT.to_string(),
            ],
            mute_on_exec_fault: true,
        }
    }
}

/// 输出声明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSpec {
    /// 格式（v1 仅 `wav_f32`）。
    pub format: String,
}

/// 解析后的 session manifest。
#[derive(Clone, Debug)]
pub struct Session {
    /// schema（= [`SESSION_SCHEMA`]）。
    pub schema: String,
    /// 精度锁（= [`PRECISION_LOCK`]）。
    pub precision: String,
    /// 音频/布局。
    pub audio: AudioSpec,
    /// 引擎/版本。
    pub engine: EngineSpec,
    /// role 表（按 index 升序 = 数组顺序）。
    pub roles: Vec<RoleMap>,
    /// 低带 participants（role id）。
    pub participants: Vec<String>,
    /// 公共 baseline 延迟 D（样本）。
    pub d_samples: u32,
    /// 系数槽（a/g/p 合并）。
    pub coefficients: Vec<CoeffSlot>,
    /// 校准 IR（可缺省）。
    pub ir: Option<IrDraft>,
    /// 保护策略。
    pub protection: ProtectionPolicy,
    /// 输出声明（可缺省）。
    pub output: Option<OutputSpec>,
    /// created_unix（可选；不参与图版本）。
    pub created_unix: Option<u64>,
    /// x_ 扩展字段名（warning）。
    pub extensions: Vec<String>,
    /// 解析期 warning。
    pub warnings: Vec<String>,
}

const SESSION_KNOWN_FIELDS: [&str; 12] = [
    "schema",
    "precision",
    "created_unix",
    "audio",
    "engine",
    "roles",
    "participants",
    "d_samples",
    "coefficients",
    "ir",
    "protection",
    "output",
];

const PROTECTION_TRIGGERS: [&str; 4] = [
    TRIGGER_GRAPH_HASH_MISMATCH,
    TRIGGER_IR_STALE,
    TRIGGER_COMPILE_REJECT,
    TRIGGER_IR_ABSENT,
];

/// 解析 session manifest（结构/范围/一致性；不加载 blob）。
pub fn parse_session(text: &str) -> RunnerResult<Session> {
    let value = Json::parse(text)?;
    parse_session_json(&value)
}

/// 解析已解析的 session JSON。
pub fn parse_session_json(value: &Json) -> RunnerResult<Session> {
    let object = value
        .as_object()
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "session must be a JSON object"))?;
    let mut warnings = Vec::new();
    let mut extensions = Vec::new();
    for key in object.keys() {
        if !SESSION_KNOWN_FIELDS.contains(&key.as_str()) {
            if key.starts_with("x_") {
                extensions.push(key.clone());
            } else {
                return Err(RunnerError::new(
                    E_FILE_SCHEMA,
                    format!("unknown session field '{}' (x_ prefix required)", key),
                ));
            }
        }
    }
    let schema = value.require_str("schema")?;
    if schema != SESSION_SCHEMA {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!(
                "session schema must be '{}' (got '{}')",
                SESSION_SCHEMA, schema
            ),
        ));
    }
    let precision = value.require_str("precision")?;
    if precision != PRECISION_LOCK {
        return Err(RunnerError::new(
            E_FILE_PRECISION,
            format!(
                "precision must be '{}' (locked; got '{}')",
                PRECISION_LOCK, precision
            ),
        ));
    }
    let created_unix = match value.get("created_unix") {
        Some(entry) => Some(entry.as_u64().ok_or_else(|| {
            RunnerError::new(E_FILE_SCHEMA, "created_unix must be a non-negative integer")
        })?),
        None => None,
    };

    // audio
    let audio_object = value.require_object("audio")?;
    let sample_rate_hz = require_object_u64(audio_object, "sample_rate_hz", "audio")?;
    if !RUNTIME_SAMPLE_RATES.contains(&(sample_rate_hz as u32)) {
        return Err(RunnerError::new(
            E_FILE_RATE,
            format!(
                "audio.sample_rate_hz {} not a runtime rate (48000/96000)",
                sample_rate_hz
            ),
        ));
    }
    let container_channels = require_object_u64(audio_object, "container_channels", "audio")?;
    if container_channels == 0 || container_channels > MAX_CONTAINER_CHANNELS as u64 {
        return Err(RunnerError::new(
            E_FILE_CHANNELS,
            format!(
                "audio.container_channels {} outside 1..={}",
                container_channels, MAX_CONTAINER_CHANNELS
            ),
        ));
    }
    let input_channels = require_object_u64(audio_object, "input_channels", "audio")?;
    if input_channels == 0 || input_channels > MAX_INPUT_CHANNELS as u64 {
        return Err(RunnerError::new(
            E_FILE_CHANNELS,
            format!(
                "audio.input_channels {} outside 1..={}",
                input_channels, MAX_INPUT_CHANNELS
            ),
        ));
    }
    let layout_id = require_object_string(audio_object, "layout_id", "audio")?;
    if layout_id.is_empty() {
        return Err(RunnerError::new(E_FILE_SCHEMA, "audio.layout_id is empty"));
    }
    let input_object = audio_object
        .get("input")
        .and_then(Json::as_object)
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "audio.input must be an object"))?;
    let input_file = require_object_string(input_object, "file", "audio.input")?;
    if input_file.is_empty() {
        return Err(RunnerError::new(E_FILE_SCHEMA, "audio.input.file is empty"));
    }
    let input_sha256 = match input_object.get("sha256") {
        Some(entry) => {
            let hash = entry.as_str().ok_or_else(|| {
                RunnerError::new(E_FILE_SCHEMA, "audio.input.sha256 must be a string")
            })?;
            if !is_hex64(hash) {
                return Err(RunnerError::new(
                    E_FILE_GRAPH,
                    "audio.input.sha256 must be 64 hex chars",
                ));
            }
            Some(hash.to_string())
        }
        None => None,
    };
    let mut binding_present = false;
    let mut profile_sha256 = None;
    let mut calibration_sha256 = None;
    let mut target_sha256 = None;
    if let Some(binding) = audio_object.get("binding") {
        let binding_object = binding
            .as_object()
            .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "audio.binding must be an object"))?;
        binding_present = true;
        profile_sha256 = Some(require_hash(
            binding_object,
            "profile_sha256",
            "audio.binding",
        )?);
        calibration_sha256 = Some(require_hash(
            binding_object,
            "calibration_sha256",
            "audio.binding",
        )?);
        target_sha256 = match binding_object.get("target_sha256") {
            None | Some(Json::Null) => None,
            Some(entry) => {
                let hash = entry.as_str().ok_or_else(|| {
                    RunnerError::new(
                        E_FILE_SCHEMA,
                        "audio.binding.target_sha256 must be string or null",
                    )
                })?;
                if !is_hex64(hash) {
                    return Err(RunnerError::new(
                        E_FILE_GRAPH,
                        "audio.binding.target_sha256 must be 64 hex chars",
                    ));
                }
                Some(hash.to_string())
            }
        };
    }

    // engine
    let engine_object = value.require_object("engine")?;
    let abi_version = require_object_u64(engine_object, "abi_version", "engine")?;
    if abi_version != 1 {
        return Err(RunnerError::new(
            E_FILE_VERSION,
            format!("engine.abi_version must be 1 (got {})", abi_version),
        ));
    }
    let plan_version = require_object_u64(engine_object, "plan_version", "engine")?;
    if plan_version == 0 || plan_version > u32::MAX as u64 {
        return Err(RunnerError::new(
            E_FILE_VERSION,
            "engine.plan_version must be >= 1 and fit u32",
        ));
    }
    let block_size = require_object_u64(engine_object, "block_size", "engine")?;
    if !RUNTIME_BLOCK_SIZES.contains(&(block_size as u16)) {
        return Err(RunnerError::new(
            E_FILE_VERSION,
            format!(
                "engine.block_size {} not a runtime block (64/128)",
                block_size
            ),
        ));
    }
    let expected_workers = require_object_u64(engine_object, "expected_workers", "engine")?;
    if ![1u64, 2, 4].contains(&expected_workers) {
        return Err(RunnerError::new(
            E_FILE_VERSION,
            format!("engine.expected_workers {} must be 1/2/4", expected_workers),
        ));
    }
    if expected_workers == 1 {
        warnings.push(
            "expected_workers=1 is test/benchmark only (no production admission)".to_string(),
        );
    }
    let profile = require_object_string(engine_object, "profile", "engine")?;
    if profile != PROFILE_REGULAR && profile != crate::fir::PROFILE_EXTENDED {
        return Err(RunnerError::new(
            E_FILE_PROFILE,
            format!(
                "engine.profile '{}' unsupported (regular/extended)",
                profile
            ),
        ));
    }
    let max_taps = match engine_object.get("max_taps") {
        Some(entry) => entry
            .as_u64()
            .ok_or_else(|| RunnerError::new(E_FILE_PROFILE, "engine.max_taps must be a u32"))?,
        None => crate::ROUTE_TAPS_REGULAR_MAX as u64,
    };
    if max_taps == 0 || max_taps > crate::ROUTE_TAPS_EXTENDED_MAX as u64 {
        return Err(RunnerError::new(
            E_FILE_PROFILE,
            format!("engine.max_taps {} outside 1..=16384", max_taps),
        ));
    }
    let extended_taps_admitted = match engine_object.get("extended_taps_admitted") {
        Some(entry) => entry.as_bool().ok_or_else(|| {
            RunnerError::new(E_FILE_SCHEMA, "engine.extended_taps_admitted must be bool")
        })?,
        None => false,
    };
    if max_taps > crate::ROUTE_TAPS_REGULAR_MAX as u64 && !extended_taps_admitted {
        return Err(RunnerError::new(
            E_FILE_PROFILE,
            "engine.max_taps > 4096 requires extended_taps_admitted=true",
        ));
    }
    if profile == PROFILE_REGULAR && max_taps > crate::ROUTE_TAPS_REGULAR_MAX as u64 {
        return Err(RunnerError::new(
            E_FILE_PROFILE,
            "engine.profile=regular cannot have max_taps > 4096",
        ));
    }
    let graph_id = match engine_object.get("graph_id") {
        Some(entry) => {
            let hash = entry.as_str().ok_or_else(|| {
                RunnerError::new(E_FILE_GRAPH, "engine.graph_id must be a string")
            })?;
            if !is_hex64(hash) {
                return Err(RunnerError::new(
                    E_FILE_GRAPH,
                    "engine.graph_id must be 64 hex chars",
                ));
            }
            Some(hash.to_string())
        }
        None => None,
    };

    // roles
    let role_entries = value.require_array("roles")?;
    if role_entries.is_empty() || role_entries.len() > MAX_ROLES {
        return Err(RunnerError::new(
            E_FILE_ROLES,
            format!(
                "roles count {} outside 1..={}",
                role_entries.len(),
                MAX_ROLES
            ),
        ));
    }
    let mut roles: Vec<RoleMap> = Vec::with_capacity(role_entries.len());
    let mut role_names: BTreeSet<String> = BTreeSet::new();
    let mut scatters: BTreeSet<u16> = BTreeSet::new();
    let mut used_sources: BTreeSet<u16> = BTreeSet::new();
    for (position, entry) in role_entries.iter().enumerate() {
        let role_object = entry
            .as_object()
            .ok_or_else(|| RunnerError::new(E_FILE_ROLES, "roles entries must be objects"))?;
        let name = require_object_string(role_object, "name", "roles[]")?;
        if name.is_empty() || !role_names.insert(name.clone()) {
            return Err(RunnerError::new(
                E_FILE_ROLES,
                format!("duplicate/empty role name '{}'", name),
            ));
        }
        let index = require_object_u64(role_object, "index", "roles[]")?;
        if index != position as u64 {
            return Err(RunnerError::new(
                E_FILE_ROLES,
                format!(
                    "role '{}' index {} != position {} (role table must be dense)",
                    name, index, position
                ),
            ));
        }
        let scatter = require_object_u64(role_object, "scatter", "roles[]")?;
        if scatter >= container_channels {
            return Err(RunnerError::new(
                E_FILE_ROLES,
                format!(
                    "role '{}' scatter {} outside container channels {}",
                    name, scatter, container_channels
                ),
            ));
        }
        if !scatters.insert(scatter as u16) {
            return Err(RunnerError::new(
                E_FILE_ROLES,
                format!(
                    "role '{}' scatter {} duplicated (must be injective)",
                    name, scatter
                ),
            ));
        }
        let src = match role_object.get("src") {
            None | Some(Json::Null) => None,
            Some(entry) => {
                let src = entry.as_u64().ok_or_else(|| {
                    RunnerError::new(
                        E_FILE_ROLES,
                        "roles[].src must be a non-negative integer or null",
                    )
                })?;
                if src >= input_channels {
                    return Err(RunnerError::new(
                        E_FILE_ROLES,
                        format!(
                            "role '{}' src {} outside input_channels {}",
                            name, src, input_channels
                        ),
                    ));
                }
                if !used_sources.insert(src as u16) {
                    return Err(RunnerError::new(
                        E_FILE_ROLES,
                        format!(
                            "source {} selected by multiple roles (A_map must be one-to-one)",
                            src
                        ),
                    ));
                }
                Some(src as u16)
            }
        };
        roles.push(RoleMap {
            name,
            index: index as u16,
            src,
            scatter: scatter as u16,
        });
    }

    // participants
    let participant_entries = value.require_array("participants")?;
    if participant_entries.is_empty() || participant_entries.len() > MAX_PARTICIPANTS {
        return Err(RunnerError::new(
            E_FILE_ROLES,
            format!(
                "participants count {} outside 1..={}",
                participant_entries.len(),
                MAX_PARTICIPANTS
            ),
        ));
    }
    let mut participants: Vec<String> = Vec::with_capacity(participant_entries.len());
    for entry in participant_entries {
        let name = entry.as_str().ok_or_else(|| {
            RunnerError::new(E_FILE_ROLES, "participants entries must be strings")
        })?;
        if !role_names.contains(name) {
            return Err(RunnerError::new(
                E_FILE_ROLES,
                format!("participant '{}' not in roles", name),
            ));
        }
        if participants.iter().any(|existing| existing == name) {
            return Err(RunnerError::new(
                E_FILE_ROLES,
                format!("duplicate participant '{}'", name),
            ));
        }
        participants.push(name.to_string());
    }
    if participants.len() > roles.len() {
        return Err(RunnerError::new(
            E_FILE_ROLES,
            "participants count exceeds roles count",
        ));
    }
    let d_samples = match value.get("d_samples") {
        Some(entry) => entry
            .as_u64()
            .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "d_samples must be a u32"))?,
        None => 0,
    };
    if d_samples > u32::MAX as u64 {
        return Err(RunnerError::new(E_FILE_SCHEMA, "d_samples overflows u32"));
    }

    // coefficients
    let coefficient_object = value.require_object("coefficients")?;
    let mut coefficients: Vec<CoeffSlot> = Vec::new();
    let a_entries = coefficient_object
        .get("a")
        .and_then(Json::as_array)
        .ok_or_else(|| RunnerError::new(E_FILE_PATH_TABLE, "missing coefficients.a array"))?;
    let mut a_sources: BTreeSet<u16> = BTreeSet::new();
    for entry in a_entries {
        let slot_object = entry.as_object().ok_or_else(|| {
            RunnerError::new(E_FILE_PATH_TABLE, "coefficients.a entries must be objects")
        })?;
        let src = require_object_u64(slot_object, "src", "coefficients.a[]")?;
        if src >= input_channels {
            return Err(RunnerError::new(
                E_FILE_PATH_TABLE,
                format!(
                    "coefficients.a src {} outside input_channels {}",
                    src, input_channels
                ),
            ));
        }
        if !a_sources.insert(src as u16) {
            return Err(RunnerError::new(
                E_FILE_PATH_TABLE,
                format!("duplicate A slot for src {}", src),
            ));
        }
        coefficients.push(parse_coeff_slot(
            slot_object,
            CoefficientKind::Input { src: src as u16 },
        )?);
    }
    if a_sources.len() != input_channels as usize {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            format!(
                "coefficients.a covers {} of {} sources (all sources need A FIR)",
                a_sources.len(),
                input_channels
            ),
        ));
    }
    let g_entries = match coefficient_object.get("g") {
        Some(entry) => entry
            .as_array()
            .ok_or_else(|| RunnerError::new(E_FILE_PATH_TABLE, "coefficients.g must be an array"))?
            .to_vec(),
        None => Vec::new(),
    };
    let mut g_cells: BTreeSet<(u16, u16)> = BTreeSet::new();
    let m = participants.len();
    for entry in &g_entries {
        let slot_object = entry.as_object().ok_or_else(|| {
            RunnerError::new(E_FILE_PATH_TABLE, "coefficients.g entries must be objects")
        })?;
        let row = require_object_u64(slot_object, "row", "coefficients.g[]")?;
        let col = require_object_u64(slot_object, "col", "coefficients.g[]")?;
        if row >= m as u64 || col >= m as u64 {
            return Err(RunnerError::new(
                E_FILE_PATH_TABLE,
                format!(
                    "coefficients.g cell ({},{}) outside participants {}",
                    row, col, m
                ),
            ));
        }
        if !g_cells.insert((row as u16, col as u16)) {
            return Err(RunnerError::new(
                E_FILE_PATH_TABLE,
                format!("duplicate G cell ({},{})", row, col),
            ));
        }
        coefficients.push(parse_coeff_slot(
            slot_object,
            CoefficientKind::Matrix {
                row: row as u16,
                col: col as u16,
            },
        )?);
    }
    let p_entries = coefficient_object
        .get("p")
        .and_then(Json::as_array)
        .ok_or_else(|| RunnerError::new(E_FILE_PATH_TABLE, "missing coefficients.p array"))?;
    let mut p_roles: BTreeSet<u16> = BTreeSet::new();
    for entry in p_entries {
        let slot_object = entry.as_object().ok_or_else(|| {
            RunnerError::new(E_FILE_PATH_TABLE, "coefficients.p entries must be objects")
        })?;
        let role_name = require_object_string(slot_object, "role", "coefficients.p[]")?;
        let role_index = roles
            .iter()
            .position(|role| role.name == role_name)
            .ok_or_else(|| {
                RunnerError::new(
                    E_FILE_PATH_TABLE,
                    format!("coefficients.p role '{}' not in roles", role_name),
                )
            })? as u16;
        if !p_roles.insert(role_index) {
            return Err(RunnerError::new(
                E_FILE_PATH_TABLE,
                format!("duplicate P slot for role '{}'", role_name),
            ));
        }
        coefficients.push(parse_coeff_slot(
            slot_object,
            CoefficientKind::Output { role: role_index },
        )?);
    }
    if p_roles.len() != roles.len() {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            format!(
                "coefficients.p covers {} of {} roles (all roles need P FIR)",
                p_roles.len(),
                roles.len()
            ),
        ));
    }

    // ir
    let ir = match value.get("ir") {
        None | Some(Json::Null) => None,
        Some(entry) => Some(ir::parse_ir(entry)?),
    };

    // protection
    let protection = match value.get("protection") {
        Some(entry) => {
            let protection_object = entry
                .as_object()
                .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "protection must be an object"))?;
            let fallback = match protection_object.get("fallback") {
                Some(entry) => entry
                    .as_str()
                    .ok_or_else(|| {
                        RunnerError::new(E_FILE_SCHEMA, "protection.fallback must be a string")
                    })?
                    .to_string(),
                None => FALLBACK_IDENTITY_P_POST.to_string(),
            };
            if fallback != FALLBACK_IDENTITY_P_POST && fallback != FALLBACK_MUTE {
                return Err(RunnerError::new(
                    E_FILE_SCHEMA,
                    format!(
                        "protection.fallback '{}' unsupported (identity_p_post/mute)",
                        fallback
                    ),
                ));
            }
            let mut triggers: Vec<String> = Vec::new();
            if let Some(entries) = protection_object.get("triggers") {
                let array = entries.as_array().ok_or_else(|| {
                    RunnerError::new(E_FILE_SCHEMA, "protection.triggers must be an array")
                })?;
                for trigger in array {
                    let text = trigger.as_str().ok_or_else(|| {
                        RunnerError::new(
                            E_FILE_SCHEMA,
                            "protection.triggers entries must be strings",
                        )
                    })?;
                    if !PROTECTION_TRIGGERS.contains(&text) {
                        return Err(RunnerError::new(
                            E_FILE_SCHEMA,
                            format!("protection trigger '{}' unknown", text),
                        ));
                    }
                    if !triggers.iter().any(|existing| existing == text) {
                        triggers.push(text.to_string());
                    }
                }
            } else {
                triggers = ProtectionPolicy::default().triggers;
            }
            let mute_on_exec_fault = match protection_object.get("mute_on_exec_fault") {
                Some(entry) => entry.as_bool().ok_or_else(|| {
                    RunnerError::new(E_FILE_SCHEMA, "protection.mute_on_exec_fault must be bool")
                })?,
                None => true,
            };
            ProtectionPolicy {
                fallback,
                triggers,
                mute_on_exec_fault,
            }
        }
        None => ProtectionPolicy::default(),
    };

    // output
    let output = match value.get("output") {
        Some(entry) => {
            let output_object = entry
                .as_object()
                .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "output must be an object"))?;
            let format = require_object_string(output_object, "format", "output")?;
            if format != OUTPUT_FORMAT_WAV_F32 {
                return Err(RunnerError::new(
                    E_FILE_SCHEMA,
                    format!("output.format '{}' unsupported (wav_f32)", format),
                ));
            }
            Some(OutputSpec { format })
        }
        None => None,
    };

    Ok(Session {
        schema: schema.to_string(),
        precision: precision.to_string(),
        audio: AudioSpec {
            sample_rate_hz: sample_rate_hz as u32,
            container_channels: container_channels as u16,
            input_channels: input_channels as u16,
            layout_id,
            input_file,
            input_sha256,
            profile_sha256,
            calibration_sha256,
            target_sha256,
            binding_present,
        },
        engine: EngineSpec {
            abi_version: abi_version as u16,
            plan_version: plan_version as u32,
            block_size: block_size as u16,
            expected_workers: expected_workers as u8,
            profile,
            max_taps: max_taps as u32,
            extended_taps_admitted,
            graph_id,
        },
        roles,
        participants,
        d_samples: d_samples as u32,
        coefficients,
        ir,
        protection,
        output,
        created_unix,
        extensions,
        warnings,
    })
}

impl Session {
    /// role 名 -> 数组位置（输出角色序号）。
    pub fn role_position(&self, name: &str) -> Option<u16> {
        self.roles
            .iter()
            .position(|role| role.name == name)
            .map(|position| position as u16)
    }

    /// 当前 IR 上下文（仅当 session 提供 audio.binding 时可用于六原因判定）。
    pub fn ir_current(&self) -> Option<IrCurrent> {
        if !self.audio.binding_present {
            return None;
        }
        Some(IrCurrent {
            fs_hz: self.audio.sample_rate_hz,
            layout_id: Some(self.audio.layout_id.clone()),
            role_indices: self
                .roles
                .iter()
                .map(|role| (role.name.clone(), role.scatter as u32))
                .collect(),
            profile_sha256: self.audio.profile_sha256.clone().unwrap_or_default(),
            target_sha256: self.audio.target_sha256.clone(),
            calibration_sha256: self.audio.calibration_sha256.clone().unwrap_or_default(),
        })
    }

    /// stale 原因：总是比较 fs/layout/roles；提供 binding 时再比较 profile/target/calibration。
    pub fn ir_stale_reasons(&self) -> Vec<StaleReason> {
        let Some(ir) = self.ir.as_ref() else {
            return Vec::new();
        };
        let mut reasons = Vec::new();
        if self.audio.sample_rate_hz != ir.source_sample_rate {
            reasons.push(StaleReason::Fs);
        }
        if Some(self.audio.layout_id.as_str()) != ir.layout.as_deref() {
            reasons.push(StaleReason::Layout);
        }
        let current_roles: Vec<(String, u32)> = self
            .roles
            .iter()
            .map(|role| (role.name.clone(), role.scatter as u32))
            .collect();
        if current_roles != ir.role_indices() {
            reasons.push(StaleReason::Roles);
        }
        if self.audio.binding_present {
            if self.audio.profile_sha256.as_deref() != Some(ir.profile.profile_sha256.as_str()) {
                reasons.push(StaleReason::Profile);
            }
            if self.audio.target_sha256.as_deref() != ir.curve_target_sha256.as_deref() {
                reasons.push(StaleReason::Target);
            }
            if self.audio.calibration_sha256.as_deref() != Some(ir.source_sha256.as_str()) {
                reasons.push(StaleReason::Calibration);
            }
        }
        reasons
    }
}

fn parse_coeff_slot(
    slot_object: &BTreeMap<String, Json>,
    kind: CoefficientKind,
) -> RunnerResult<CoeffSlot> {
    let file = require_object_string(slot_object, "file", "coefficients[]")?;
    if file.is_empty() {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            "coefficients[].file is empty",
        ));
    }
    let manifest_file = match slot_object.get("manifest_file") {
        Some(entry) => Some(
            entry
                .as_str()
                .ok_or_else(|| RunnerError::new(E_FILE_FIELD, "manifest_file must be a string"))?
                .to_string(),
        ),
        None => None,
    };
    let manifest_inline = slot_object.get("manifest").cloned();
    if manifest_file.is_some() && manifest_inline.is_some() {
        return Err(RunnerError::new(
            E_FILE_FIELD,
            "coefficient slot must not have both manifest and manifest_file",
        ));
    }
    if manifest_file.is_none() && manifest_inline.is_none() {
        return Err(RunnerError::new(
            E_FILE_FIELD,
            "coefficient slot requires manifest or manifest_file",
        ));
    }
    Ok(CoeffSlot {
        kind,
        file,
        manifest_file,
        manifest_inline,
    })
}

fn require_object_string(
    object: &BTreeMap<String, Json>,
    key: &str,
    context: &str,
) -> RunnerResult<String> {
    let entry = object.get(key).ok_or_else(|| {
        RunnerError::new(
            E_FILE_FIELD,
            format!("missing required field '{}.{}'", context, key),
        )
    })?;
    match entry.as_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(RunnerError::new(
            E_FILE_FIELD,
            format!("field '{}.{}' must be a string", context, key),
        )),
    }
}

fn require_object_u64(
    object: &BTreeMap<String, Json>,
    key: &str,
    context: &str,
) -> RunnerResult<u64> {
    let entry = object.get(key).ok_or_else(|| {
        RunnerError::new(
            E_FILE_FIELD,
            format!("missing required field '{}.{}'", context, key),
        )
    })?;
    entry.as_u64().ok_or_else(|| {
        RunnerError::new(
            E_FILE_FIELD,
            format!("field '{}.{}' must be a non-negative integer", context, key),
        )
    })
}

fn require_hash(object: &BTreeMap<String, Json>, key: &str, context: &str) -> RunnerResult<String> {
    let text = require_object_string(object, key, context)?;
    if !is_hex64(&text) {
        return Err(RunnerError::new(
            E_FILE_GRAPH,
            format!("field '{}.{}' must be 64 hex chars", context, key),
        ));
    }
    Ok(text)
}
