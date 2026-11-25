//! [NONRT] `tspi-cal-ir-draft/v1` 校准 IR 草案：解析/绑定/stale/PostEncoding（对齐 T13 adapter）。
//!
//! WBS T14 的“IR”输入：runner 不重新解析 YAML 校准文件（那是 T13 的职责），而是消费 T13 calibration
//! adapter 产出的内部 IR 草案（`tspi-cal-ir-draft/v1`），把它的 version/hash/role/fs/taps/placement/
//! P/phase/D 绑定到本次 file 图：
//!
//! - **版本/绑定**：`ir_version` 必须 =1；`ir_hash` 必须 64 hex（进入图版本 hash）；
//!   `source.sample_rate` 必须等于本次 fs；`layout` 与 `profile` 绑定进入 stale 判定；
//! - **role 绑定**：IR role 的 name/index（物理通道）必须与 session role/scatter 一一对应；
//! - **P/phase/D**：`post[].p_taps` 与 `encoding` 二选一（禁止双叠）：
//!   `coeff_embedded` = 系数文件已含 delay/polarity（p_taps = D+1；runner 不再加），
//!   `role_metadata` = P FIR 为 [1]（p_taps = 1），runner 在输出侧 P 上应用整数 delay 与 polarity
//!   （delay_q16 必须为 0：file runner 精度锁只支持整数 sample 延迟，分数时延必须先折入系数/离线重设计）；
//! - **stale 六原因**（与 T13 `StaleReason` 同字面值）：STALE_FS/STALE_LAYOUT/STALE_ROLES/
//!   STALE_PROFILE/STALE_TARGET/STALE_CALIBRATION；任一命中即触发保护（或按策略拒收）。
//!
//! IR 不重复应用校准：runner 只把 P 阶段作为唯一应用点，PEQ/gain/TargetFir 属于 T13 IR 的
//! placement/T12 编译产物，本模块只**记录**（报告）不二次计算；curve 只允许一处（P 基线），
//! G 只允许 modal residual（无 curve）。

use std::collections::BTreeSet;

use crate::json::Json;
use crate::sha256::is_hex64;
use crate::{RunnerError, RunnerResult, E_FILE_DELAY, E_FILE_IR, E_FILE_TAPS};

/// IR 草案 schema（T13）。
pub const IR_SCHEMA: &str = "tspi-cal-ir-draft/v1";
/// IR 草案版本（T13）。
pub const IR_VERSION: u32 = 1;
/// IR 规范化 hash 算法标识（T13）。
pub const IR_HASH_ALGO: &str = "tspi-cal-ir-canonical-v1";
/// P/phase/D 编码：已折入 P FIR 系数（role 字段 delay=0/polarity=+1）。
pub const POST_ENCODING_COEFF_EMBEDDED: &str = "coeff_embedded";
/// P/phase/D 编码：放 role 元数据（P FIR = [1]；runner 应用整数 delay/polarity）。
pub const POST_ENCODING_ROLE_METADATA: &str = "role_metadata";
/// max role 数（T13 上限 16；core-v1 role 上限 16）。
pub const IR_MAX_ROLES: usize = 16;

/// P/phase/D 编码方式（二选一，禁止重复计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostEncoding {
    /// 已折入 P FIR（p_taps = D+1）。
    CoeffEmbedded,
    /// role 元数据（P FIR = [1]；D/polarity 由 runner 应用）。
    RoleMetadata,
}

impl PostEncoding {
    /// 从字面值解析（未知 -> None）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            POST_ENCODING_COEFF_EMBEDDED => Some(PostEncoding::CoeffEmbedded),
            POST_ENCODING_ROLE_METADATA => Some(PostEncoding::RoleMetadata),
            _ => None,
        }
    }

    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            PostEncoding::CoeffEmbedded => POST_ENCODING_COEFF_EMBEDDED,
            PostEncoding::RoleMetadata => POST_ENCODING_ROLE_METADATA,
        }
    }
}

/// polarity（语义极性；应用一次）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// 正相。
    Normal,
    /// 反相（×−1）。
    Invert,
}

impl Polarity {
    /// 从字面值解析（normal/invert；兼容 +1/-1）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "normal" | "+1" => Some(Polarity::Normal),
            "invert" | "-1" => Some(Polarity::Invert),
            _ => None,
        }
    }

    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Polarity::Normal => "normal",
            Polarity::Invert => "invert",
        }
    }

    /// 乘子（+1/-1）。
    pub const fn multiplier(self) -> i8 {
        match self {
            Polarity::Normal => 1,
            Polarity::Invert => -1,
        }
    }
}

/// IR role 绑定项（name + 物理通道 index）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrRole {
    /// role id。
    pub name: String,
    /// 物理播放通道号。
    pub index: u32,
}

/// IR profile 绑定（stereo mask / 目标 profile / P 基线 hash）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrProfile {
    /// profile id。
    pub profile_id: String,
    /// 渲染 profile 文档 hash（64 hex）。
    pub profile_sha256: String,
    /// 是否 stereo_strict。
    pub stereo_strict: bool,
    /// stereo mask role id。
    pub stereo_mask: Vec<String>,
    /// 目标 profile 标识。
    pub target_profile: String,
}

/// IR 的 P_post 单角色（placement/PEQ/gain 只记录，不二次应用）。
#[derive(Clone, Debug, PartialEq)]
pub struct IrPostRole {
    /// role id。
    pub name: String,
    /// 物理通道号。
    pub index: u32,
    /// 实际总延迟（samples；两种编码下都等于滤波/role 的整数 D）。
    pub delay_samples: u32,
    /// 次样本延迟（校准整数 D 恒 0）。
    pub delay_q16: u32,
    /// 语义极性（应用一次）。
    pub polarity: Polarity,
    /// 合并后 role gain（dB；Gain 节点一次应用，不折入 P FIR）。
    pub gain_db: f64,
    /// 校准 PEQ 段数（只记录；应用点不在 runner）。
    pub peq_count: usize,
    /// 本角色出现的阶段（按规范链序子集；只记录）。
    pub placement: Vec<String>,
    /// 本角色 P FIR taps（CoeffEmbedded = D+1；RoleMetadata = 1）。
    pub p_taps: u32,
}

/// 解析后的 `tspi-cal-ir-draft/v1` 草案（绑定用）。
#[derive(Clone, Debug, PartialEq)]
pub struct IrDraft {
    /// schema。
    pub schema: String,
    /// IR 版本（=1）。
    pub ir_version: u32,
    /// IR 内容 hash（64 hex）。
    pub ir_hash: String,
    /// hash 算法（默认/必须 = [`IR_HASH_ALGO`]）。
    pub hash_algo: String,
    /// created_unix（可选；不参与图版本）。
    pub created_unix: Option<u64>,
    /// 源校准 schema（khaen-calibration/v1 或 v1.1）。
    pub source_schema: String,
    /// 源校准文件 SHA-256（64 hex）。
    pub source_sha256: String,
    /// 源校准 sample_rate。
    pub source_sample_rate: u32,
    /// layout id（可缺省）。
    pub layout: Option<String>,
    /// profile 绑定。
    pub profile: IrProfile,
    /// role 绑定（唯一 name/index）。
    pub roles: Vec<IrRole>,
    /// P/phase/D 单角色。
    pub post: Vec<IrPostRole>,
    /// 编码方式。
    pub encoding: PostEncoding,
    /// 目标曲线 hash（None = 无曲线；最多绑定一次）。
    pub curve_target_sha256: Option<String>,
    /// G residual 规则（只允许 modal_residual_only）。
    pub g_residual_rule: String,
    /// G residual participants（role 名）。
    pub g_residual_participants: Vec<String>,
    /// extensions 数量（只记录）。
    pub extensions_count: usize,
}

/// stale 原因（字面值与 T13 [`StaleReason::as_str`] 一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleReason {
    /// 采样率变化。
    Fs,
    /// layout id 变化。
    Layout,
    /// role -> 物理通道映射变化。
    Roles,
    /// 渲染 profile/P 基线 hash 变化。
    Profile,
    /// 目标曲线 hash 变化。
    Target,
    /// 源校准文件 hash 变化。
    Calibration,
}

impl StaleReason {
    /// 稳定字面值（与 T13 相同：STALE_FS/…）。
    pub const fn as_str(self) -> &'static str {
        match self {
            StaleReason::Fs => "STALE_FS",
            StaleReason::Layout => "STALE_LAYOUT",
            StaleReason::Roles => "STALE_ROLES",
            StaleReason::Profile => "STALE_PROFILE",
            StaleReason::Target => "STALE_TARGET",
            StaleReason::Calibration => "STALE_CALIBRATION",
        }
    }
}

/// 当前运行/设计上下文（session 给出；与 IR 记录比较判定 stale）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrCurrent {
    /// 当前 profile 采样率。
    pub fs_hz: u32,
    /// 当前 layout id（None = 无 layout 绑定）。
    pub layout_id: Option<String>,
    /// 当前 role -> 物理通道映射（有序；与 IR 记录比较）。
    pub role_indices: Vec<(String, u32)>,
    /// 当前渲染 profile 文档 hash。
    pub profile_sha256: String,
    /// 当前目标曲线 hash（None = 当前无曲线）。
    pub target_sha256: Option<String>,
    /// 当前源校准文件 hash。
    pub calibration_sha256: String,
}

const IR_KNOWN_FIELDS: [&str; 14] = [
    "schema",
    "ir_version",
    "ir_hash",
    "hash_algo",
    "created_unix",
    "source",
    "layout",
    "profile",
    "roles",
    "post",
    "curve",
    "g_residual",
    "encoding",
    "extensions",
];

/// 解析 `tspi-cal-ir-draft/v1` IR JSON（结构/语义校验；不写文件）。
pub fn parse_ir(value: &Json) -> RunnerResult<IrDraft> {
    let object = value
        .as_object()
        .ok_or_else(|| RunnerError::new(E_FILE_IR, "ir must be a JSON object"))?;
    for key in object.keys() {
        if !IR_KNOWN_FIELDS.contains(&key.as_str()) && !key.starts_with("x_") {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!("unknown ir field '{}'", key),
            ));
        }
    }
    let schema = value.require_str("schema")?;
    if schema != IR_SCHEMA {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!("ir schema must be '{}' (got '{}')", IR_SCHEMA, schema),
        ));
    }
    let ir_version = value.require_u64("ir_version")?;
    if ir_version != IR_VERSION as u64 {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!("ir_version must be {} (got {})", IR_VERSION, ir_version),
        ));
    }
    let ir_hash = value.require_str("ir_hash")?.to_string();
    if !is_hex64(&ir_hash) {
        return Err(RunnerError::new(E_FILE_IR, "ir_hash must be 64 hex chars"));
    }
    let hash_algo = match value.get("hash_algo") {
        Some(entry) => entry
            .as_str()
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "hash_algo must be a string"))?
            .to_string(),
        None => IR_HASH_ALGO.to_string(),
    };
    if hash_algo != IR_HASH_ALGO {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!(
                "hash_algo '{}' unsupported (expected {})",
                hash_algo, IR_HASH_ALGO
            ),
        ));
    }
    let created_unix = match value.get("created_unix") {
        Some(entry) => Some(entry.as_u64().ok_or_else(|| {
            RunnerError::new(E_FILE_IR, "created_unix must be a non-negative integer")
        })?),
        None => None,
    };
    let source = value.require_object("source")?;
    let source_schema = require_object_string(source, "schema", "source")?;
    if source_schema != "khaen-calibration/v1" && source_schema != "khaen-calibration/v1.1" {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!("source.schema '{}' unsupported", source_schema),
        ));
    }
    let source_sha256 = require_object_string(source, "source_sha256", "source")?;
    if !is_hex64(&source_sha256) {
        return Err(RunnerError::new(
            E_FILE_IR,
            "source.source_sha256 must be 64 hex chars",
        ));
    }
    let source_sample_rate = require_object_u64(source, "sample_rate", "source")?;
    if source_sample_rate == 0 || source_sample_rate > 384_000 {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!(
                "source.sample_rate {} outside 1..=384000",
                source_sample_rate
            ),
        ));
    }
    let layout = match value.get("layout") {
        Some(entry) => Some(
            entry
                .as_str()
                .ok_or_else(|| RunnerError::new(E_FILE_IR, "layout must be a string"))?
                .to_string(),
        ),
        None => None,
    };
    let profile_object = value.require_object("profile")?;
    let profile_id = require_object_string(profile_object, "profile_id", "profile")?;
    let profile_sha256 = require_object_string(profile_object, "profile_sha256", "profile")?;
    if !is_hex64(&profile_sha256) {
        return Err(RunnerError::new(
            E_FILE_IR,
            "profile.profile_sha256 must be 64 hex chars",
        ));
    }
    let stereo_strict = match profile_object.get("stereo_strict") {
        Some(entry) => entry
            .as_bool()
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "profile.stereo_strict must be a bool"))?,
        None => false,
    };
    let stereo_mask = match profile_object.get("stereo_mask") {
        Some(entry) => string_array(entry, "profile.stereo_mask")?,
        None => Vec::new(),
    };
    let target_profile = match profile_object.get("target_profile") {
        Some(entry) => entry
            .as_str()
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "profile.target_profile must be a string"))?
            .to_string(),
        None => String::new(),
    };
    let role_entries = value.require_array("roles")?;
    if role_entries.is_empty() || role_entries.len() > IR_MAX_ROLES {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!(
                "roles count {} outside 1..={}",
                role_entries.len(),
                IR_MAX_ROLES
            ),
        ));
    }
    let mut roles: Vec<IrRole> = Vec::with_capacity(role_entries.len());
    let mut role_names: BTreeSet<String> = BTreeSet::new();
    let mut role_indices: BTreeSet<u32> = BTreeSet::new();
    for entry in role_entries {
        let role_object = entry
            .as_object()
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "roles entries must be objects"))?;
        let name = require_object_string(role_object, "name", "roles[]")?;
        if name.is_empty() {
            return Err(RunnerError::new(E_FILE_IR, "roles[].name is empty"));
        }
        let index = require_object_u64(role_object, "index", "roles[]")?;
        if index > u32::MAX as u64 {
            return Err(RunnerError::new(E_FILE_IR, "roles[].index overflows u32"));
        }
        if !role_names.insert(name.clone()) || !role_indices.insert(index as u32) {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!("duplicate role name/index '{}'/{}", name, index),
            ));
        }
        roles.push(IrRole {
            name,
            index: index as u32,
        });
    }
    if stereo_strict {
        let expected = ["FL".to_string(), "FR".to_string()];
        if stereo_mask != expected {
            return Err(RunnerError::new(
                E_FILE_IR,
                "stereo_strict=true requires stereo_mask exactly [FL, FR]",
            ));
        }
    }
    for name in &stereo_mask {
        if !role_names.contains(name) {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!("stereo_mask role '{}' not in roles", name),
            ));
        }
    }
    let encoding = PostEncoding::from_id(value.require_str("encoding")?).ok_or_else(|| {
        RunnerError::new(
            E_FILE_IR,
            format!(
                "encoding must be '{}' or '{}'",
                POST_ENCODING_COEFF_EMBEDDED, POST_ENCODING_ROLE_METADATA
            ),
        )
    })?;
    let post_entries = value.require_array("post")?;
    let mut post: Vec<IrPostRole> = Vec::with_capacity(post_entries.len());
    for entry in post_entries {
        let post_object = entry
            .as_object()
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "post entries must be objects"))?;
        let name = require_object_string(post_object, "name", "post[]")?;
        if !role_names.contains(&name) {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!("post role '{}' not in roles", name),
            ));
        }
        let index = require_object_u64(post_object, "index", "post[]")? as u32;
        let role = roles
            .iter()
            .find(|role| role.name == name)
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "post role missing"))?;
        if role.index != index {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!(
                    "post '{}' index {} != role index {}",
                    name, index, role.index
                ),
            ));
        }
        let delay_samples = require_object_u64(post_object, "delay_samples", "post[]")?;
        if delay_samples > u32::MAX as u64 {
            return Err(RunnerError::new(
                E_FILE_IR,
                "post delay_samples overflows u32",
            ));
        }
        let delay_q16 = match post_object.get("delay_q16") {
            Some(entry) => entry
                .as_u64()
                .ok_or_else(|| RunnerError::new(E_FILE_IR, "post delay_q16 must be a u32"))?,
            None => 0,
        };
        if delay_q16 > u16::MAX as u64 {
            return Err(RunnerError::new(E_FILE_IR, "post delay_q16 overflows u16"));
        }
        if delay_q16 != 0 {
            return Err(RunnerError::new(
                E_FILE_DELAY,
                format!(
                    "post '{}' delay_q16={} is fractional; file runner only supports integer sample delays",
                    name, delay_q16
                ),
            ));
        }
        let polarity =
            Polarity::from_id(&require_object_string(post_object, "polarity", "post[]")?)
                .ok_or_else(|| {
                    RunnerError::new(E_FILE_IR, format!("post '{}' polarity invalid", name))
                })?;
        let gain_db = match post_object.get("gain_db") {
            Some(entry) => {
                let value = entry
                    .as_f64()
                    .ok_or_else(|| RunnerError::new(E_FILE_IR, "post gain_db must be a number"))?;
                if !value.is_finite() {
                    return Err(RunnerError::new(E_FILE_IR, "post gain_db must be finite"));
                }
                value
            }
            None => 0.0,
        };
        let peq_count = match post_object.get("peq") {
            Some(entry) => entry
                .as_array()
                .ok_or_else(|| RunnerError::new(E_FILE_IR, "post peq must be an array"))?
                .len(),
            None => 0,
        };
        let placement = match post_object.get("placement") {
            Some(entry) => string_array(entry, "post[].placement")?,
            None => Vec::new(),
        };
        let p_taps = require_object_u64(post_object, "p_taps", "post[]")?;
        if p_taps == 0 || p_taps > u32::MAX as u64 {
            return Err(RunnerError::new(E_FILE_TAPS, "post p_taps invalid"));
        }
        let p_taps = p_taps as u32;
        match encoding {
            PostEncoding::CoeffEmbedded => {
                if p_taps != delay_samples as u32 + 1 {
                    return Err(RunnerError::new(
                        E_FILE_TAPS,
                        format!(
                            "coeff_embedded post '{}' p_taps {} != delay_samples+1 ({})",
                            name,
                            p_taps,
                            delay_samples + 1
                        ),
                    ));
                }
            }
            PostEncoding::RoleMetadata => {
                if p_taps != 1 {
                    return Err(RunnerError::new(
                        E_FILE_TAPS,
                        format!("role_metadata post '{}' p_taps {} != 1", name, p_taps),
                    ));
                }
            }
        }
        post.push(IrPostRole {
            name,
            index,
            delay_samples: delay_samples as u32,
            delay_q16: delay_q16 as u32,
            polarity,
            gain_db,
            peq_count,
            placement,
            p_taps,
        });
    }
    if post.len() != roles.len() {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!(
                "post covers {} roles but roles has {}",
                post.len(),
                roles.len()
            ),
        ));
    }
    let curve_target_sha256 = match value.get("curve") {
        None | Some(Json::Null) => None,
        Some(entry) => {
            let curve = entry
                .as_object()
                .ok_or_else(|| RunnerError::new(E_FILE_IR, "curve must be an object or null"))?;
            let target = require_object_string(curve, "target_sha256", "curve")?;
            if !is_hex64(&target) {
                return Err(RunnerError::new(
                    E_FILE_IR,
                    "curve.target_sha256 must be 64 hex chars",
                ));
            }
            Some(target)
        }
    };
    let (g_residual_rule, g_residual_participants) = match value.get("g_residual") {
        Some(entry) => {
            let residual = entry
                .as_object()
                .ok_or_else(|| RunnerError::new(E_FILE_IR, "g_residual must be an object"))?;
            let rule = require_object_string(residual, "rule", "g_residual")?;
            if rule != "modal_residual_only" {
                return Err(RunnerError::new(
                    E_FILE_IR,
                    format!(
                        "g_residual.rule '{}' unsupported (only modal_residual_only)",
                        rule
                    ),
                ));
            }
            let participants = match residual.get("participants") {
                Some(entry) => string_array(entry, "g_residual.participants")?,
                None => Vec::new(),
            };
            (rule, participants)
        }
        None => ("modal_residual_only".to_string(), Vec::new()),
    };
    for name in &g_residual_participants {
        if !role_names.contains(name) {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!("g_residual participant '{}' not in roles", name),
            ));
        }
    }
    let extensions_count = match value.get("extensions") {
        Some(entry) => entry
            .as_array()
            .ok_or_else(|| RunnerError::new(E_FILE_IR, "extensions must be an array"))?
            .len(),
        None => 0,
    };
    Ok(IrDraft {
        schema: schema.to_string(),
        ir_version: ir_version as u32,
        ir_hash,
        hash_algo,
        created_unix,
        source_schema,
        source_sha256,
        source_sample_rate: source_sample_rate as u32,
        layout,
        profile: IrProfile {
            profile_id,
            profile_sha256,
            stereo_strict,
            stereo_mask,
            target_profile,
        },
        roles,
        post,
        encoding,
        curve_target_sha256,
        g_residual_rule,
        g_residual_participants,
        extensions_count,
    })
}

impl IrDraft {
    /// role -> 物理通道映射（有序；session/stale 比较用）。
    pub fn role_indices(&self) -> Vec<(String, u32)> {
        self.roles
            .iter()
            .map(|role| (role.name.clone(), role.index))
            .collect()
    }

    /// 取某 role 的 P/phase/D 记录。
    pub fn post_role(&self, name: &str) -> Option<&IrPostRole> {
        self.post.iter().find(|role| role.name == name)
    }

    /// 计算 stale 原因（空 = fresh；顺序固定 Fs/Layout/Roles/Profile/Target/Calibration）。
    pub fn stale_reasons(&self, current: &IrCurrent) -> Vec<StaleReason> {
        let mut reasons = Vec::new();
        if current.fs_hz != self.source_sample_rate {
            reasons.push(StaleReason::Fs);
        }
        if current.layout_id.as_deref() != self.layout.as_deref() {
            reasons.push(StaleReason::Layout);
        }
        let stored_roles = self.role_indices();
        if current.role_indices != stored_roles {
            reasons.push(StaleReason::Roles);
        }
        if current.profile_sha256 != self.profile.profile_sha256 {
            reasons.push(StaleReason::Profile);
        }
        if current.target_sha256.as_deref() != self.curve_target_sha256.as_deref() {
            reasons.push(StaleReason::Target);
        }
        if current.calibration_sha256 != self.source_sha256 {
            reasons.push(StaleReason::Calibration);
        }
        reasons
    }

    /// 是否 stale（任一原因）。
    pub fn is_stale(&self, current: &IrCurrent) -> bool {
        !self.stale_reasons(current).is_empty()
    }

    /// 应用 IR 的 P/phase/D 到某 role 的 P FIR 基系数：
    ///
    /// - `coeff_embedded`：系数已含 D/polarity；要求 base.len() == p_taps，原样返回（禁止再加一次）；
    /// - `role_metadata`：要求 base.len() == p_taps == 1；返回长度为 D+1 的 [0…0, ±base0]。
    ///
    /// 这是 P 阶段唯一应用点（T13 `RolePlanMetadata` 语义）；PEQ/gain 不在此折入（由编译图承担）。
    pub fn apply_post_to_p(&self, role: &str, base: &[f32]) -> RunnerResult<Vec<f32>> {
        let post = self.post_role(role).ok_or_else(|| {
            RunnerError::new(
                E_FILE_IR,
                format!("ir has no post entry for role '{}'", role),
            )
        })?;
        match self.encoding {
            PostEncoding::CoeffEmbedded => {
                if base.len() != post.p_taps as usize {
                    return Err(RunnerError::new(
                        E_FILE_TAPS,
                        format!(
                            "role '{}' coeff_embedded p_taps {} != coefficient len {}",
                            role,
                            post.p_taps,
                            base.len()
                        ),
                    ));
                }
                Ok(base.to_vec())
            }
            PostEncoding::RoleMetadata => {
                if post.p_taps != 1 || base.len() != 1 {
                    return Err(RunnerError::new(
                        E_FILE_TAPS,
                        format!(
                            "role '{}' role_metadata requires p_taps=1 and len=1 (p_taps {}, len {})",
                            role,
                            post.p_taps,
                            base.len()
                        ),
                    ));
                }
                let mut out = vec![0.0f32; post.delay_samples as usize + 1];
                let index = post.delay_samples as usize;
                out[index] = base[0] * post.polarity.multiplier() as f32;
                Ok(out)
            }
        }
    }
}

/// IR 绑定摘要（写进 runner 元数据；不改变语义）。
pub fn binding_json(ir: &IrDraft, stale: &[StaleReason]) -> Json {
    Json::from_entries(vec![
        ("schema", Json::String(ir.schema.clone())),
        ("ir_version", Json::Number(ir.ir_version as f64)),
        ("ir_hash", Json::String(ir.ir_hash.clone())),
        ("hash_algo", Json::String(ir.hash_algo.clone())),
        ("source_schema", Json::String(ir.source_schema.clone())),
        ("source_sha256", Json::String(ir.source_sha256.clone())),
        (
            "source_sample_rate",
            Json::Number(ir.source_sample_rate as f64),
        ),
        (
            "layout",
            match &ir.layout {
                Some(layout) => Json::String(layout.clone()),
                None => Json::Null,
            },
        ),
        ("profile_id", Json::String(ir.profile.profile_id.clone())),
        (
            "profile_sha256",
            Json::String(ir.profile.profile_sha256.clone()),
        ),
        ("stereo_strict", Json::Bool(ir.profile.stereo_strict)),
        ("encoding", Json::String(ir.encoding.as_str().to_string())),
        (
            "stale_reasons",
            Json::Array(
                stale
                    .iter()
                    .map(|reason| Json::String(reason.as_str().to_string()))
                    .collect(),
            ),
        ),
    ])
}

fn require_object_string(
    object: &std::collections::BTreeMap<String, Json>,
    key: &str,
    context: &str,
) -> RunnerResult<String> {
    let entry = object.get(key).ok_or_else(|| {
        RunnerError::new(
            E_FILE_IR,
            format!("missing required field '{}.{}'", context, key),
        )
    })?;
    match entry.as_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(RunnerError::new(
            E_FILE_IR,
            format!("field '{}.{}' must be a string", context, key),
        )),
    }
}

fn require_object_u64(
    object: &std::collections::BTreeMap<String, Json>,
    key: &str,
    context: &str,
) -> RunnerResult<u64> {
    let entry = object.get(key).ok_or_else(|| {
        RunnerError::new(
            E_FILE_IR,
            format!("missing required field '{}.{}'", context, key),
        )
    })?;
    entry.as_u64().ok_or_else(|| {
        RunnerError::new(
            E_FILE_IR,
            format!("field '{}.{}' must be a non-negative integer", context, key),
        )
    })
}

fn string_array(entry: &Json, context: &str) -> RunnerResult<Vec<String>> {
    let array = entry
        .as_array()
        .ok_or_else(|| RunnerError::new(E_FILE_IR, format!("{} must be an array", context)))?;
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        let text = item.as_str().ok_or_else(|| {
            RunnerError::new(E_FILE_IR, format!("{} entries must be strings", context))
        })?;
        out.push(text.to_string());
    }
    Ok(out)
}
