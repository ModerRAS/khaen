//! [NONRT] 每 path WAV/manifest/version/hash 产物（T54） ——WBS T54 写域。
//!
//! **状态：T54 leaf 实现（WBS T54）。** 本模块把 T51/T52 的完整 G 修正系数
//! （g[out][in][tap]，**不含** delta_D baseline；完整 G = delta_D*I + g，与
//! khaen_core::compile::CompileInput.g 布局一致）冻结成一套**逐路径系数 artifact**：
//! 每路径一个单声道 WAVE_FORMAT_IEEE_FLOAT（f32，fs = 设计 fs，长度 = taps，
//! **不做任何归一化**）+ 一个确定性 JSON manifest（schema khaen-modal-artifact/v1）。
//!
//! # 输入（只读 consumer；本模块不改上游语义）
//!
//! - T50 TargetMatrix：fs / layout / 显式 participants（dense->DAC）/ D0 / target delay /
//!   decay 目标 / tone+curve 单次应用证明 / height scope / output scope / target hash 链；
//! - T50 FrozenPostBaseline（P）：P 基线 hash / 每 role P_post 元数据（delay/polarity/gain/
//!   band_limit/artificial delay/modal PEQ 存在性）——**P 只按 hash 绑定，不在此重做 P 阶段**；
//! - T51 SolveOutcome 或 T52 RegularizeOutcome：完整 G + 因果 FIR 系数（f64/f32/hash）；
//! - T57 已解析的 overlay 绑定（role->物理输出槽位 + overlay 源字节 sha256）：本 crate 不依赖
//!   compat（依赖方向），T66/T57 适配器把解析结果转成 OverlayDacBinding 传入；
//! - T53 HoldoutReport（可选）：设计侧预测/隔离/T20 证据；本模块只做**绑定一致性**检查并原样
//!   记录 verdict（**绝不**把 predicted 当实测）；
//! - ArtifactProvenance：profile/P 文档 hash、measurement session hash、calibration IR hash、
//!   tuning inputs hash（可选 host graph hash，编译期由 T66 回填）。
//!
//! # 产物（deterministic；同输入同输出逐字节一致）
//!
//!   artifact/
//!     manifest.json                      # schema khaen-modal-artifact/v1
//!     coeff/p_<in_role>__<out_role>.wav  # M*M 条（4.1.2 = 25；stereo strict = 4）
//!
//! - 文件内容：p_<in>__<out>.wav = G 的 cell [out][in] 的修正系数 g（f64 设计 -> f32 落盘）。
//! - 对角路径记录 diag_baseline=true、delay_samples=D；非对角记录 0（与 core compile 的
//!   MatrixStagePlan 完全同口径；kernel 不得重复加 baseline D）。
//! - manifest 记录：schema/version/hash 算法、status + INCOMPLETE/FAIL 码、tool、identity hash 链
//!   （target/baseline/P/plant/solution/holdout/provenance）、5x5（或 2x2）participants dense->DAC、
//!   **stereo mask**（stereo strict 时恒 FL/FR）、**overlay DAC 绑定**（源 sha256 + role->outputs +
//!   unused outputs，含 4.1.2 ch6 空）、**role_hash**、**P 绑定**（baseline hash + 每 role 元数据）、
//!   **fs 与精度**（f32 WAV + 每 path f64/f32 量化误差）、**D**（整数 samples + q16=0 + ms）、
//!   decay/T20 目标、tone/curve 单次应用、height scope、逐 path sha256、量化/聚合 hash、stale 绑定
//!   所需现值（current_binding()）、诚实字段（applied/measured/requires post-apply remeasurement）。
//! - created_unix（可选）只是 manifest 元数据，**不参与 artifact hash / 系数哈希**；不传时
//!   manifest 逐字节确定。
//!
//! # INCOMPLETE / FAIL（WBS T54：真实数据缺失=INCOMPLETE）
//!
//! manifest status 三态（优先级 FAIL > INCOMPLETE > READY）：
//! - FAIL：target 明确不可行（TargetStatus::Infeasible）/ T53 PREDICTED_FAIL；
//! - INCOMPLETE：无可用解、解不可交付、P 基线缺失、T53 holdout 报告缺失/INCOMPLETE/NOT_RUN、
//!   target 覆盖不足/部分可行、measurement/calibration provenance 缺失、tuning inputs 缺失；
//! - READY：设计侧证据自洽（仍 **不是** applied/measured；必须 post-apply 复测）。
//!
//! READY 只表示"可送 T66 编译/热切换候选"，不表示 P1 校准完成；本文件不测量、不应用、
//! 不编译、不打开设备、不读时钟/环境变量。真实房间/UMIK/MOTU/ALSA、真实 5x5 系数、
//! 应用后复测、独立 holdout、RT/xrun 均为 **NOT_RUN**（见 evidence/T54/MANIFEST.md）。
//!
//! # stale（WBS §5 / T04 §7.4 / compat-matrix Y24）
//!
//! ArtifactBundle::stale_reasons 对当前设计/运行上下文（ArtifactCurrent）逐项比较
//! fs/layout/participants/role_hash/overlay source/P 基线/profile/target/solution/prior/taps/D/
//! holdout report hash；任一变化即该 artifact 失效（必须重设计，禁止复用旧系数）。
//!
//! # 与 T13 IR 草案的关系
//!
//! 语义对齐 T13 的 khaen-cal-ir-draft/v1（version/hash/role/fs/taps/P/phase/D/stale）与
//! compat-matrix Y16/Y24：artifact 带 hash/version/role/stale；G 只 modal residual（curve 只在
//! target/P 侧绑一次，本模块不二次乘）；P 侧 detail 由 P 基线 hash 绑定而不复制/改写。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use khaen_core::rt::{MAX_LOW_BAND_PARTICIPANTS, TAPS_NORMAL_MAX};

use crate::ingest::sha256_hex;
use crate::regularize::RegularizeOutcome;
use crate::solve::SolveOutcome;
use crate::target::{
    BandLimit, FrozenPostBaseline, ModalMatrix, TargetError, TargetMatrix, TargetStatus,
    MODAL_CORRECTION_BAND_HZ, SAMPLE_RATES_HZ,
};
use crate::validate::HoldoutReport;

// ===========================================================================
// 契约常量（schema/版本/hash 算法/文件名/诚实 claim）
// ===========================================================================

/// artifact manifest schema（T54 冻结；T59/T66 消费）。
pub const ARTIFACT_SCHEMA: &str = "khaen-modal-artifact/v1";
/// artifact 整数版本（语义变更必须升版）。
pub const ARTIFACT_VERSION: u32 = 1;
/// artifact 内容 hash 规范化算法标识（确定性；created_unix/warnings 不参与）。
pub const ARTIFACT_HASH_ALGO: &str = "khaen-modal-artifact-canonical-v1";
/// artifact 内容 hash domain（canonical 文本首行）。
pub const ARTIFACT_HASH_DOMAIN: &str = "khaen-modal-artifact/v1";
/// manifest 文件名（相对 artifact 根）。
pub const ARTIFACT_MANIFEST_FILE: &str = "manifest.json";
/// 系数 WAV 子目录（相对 artifact 根）。
pub const ARTIFACT_COEFF_DIR: &str = "coeff";
/// 系数 WAV 文件名前缀（p_<in_role>__<out_role>.wav）。
pub const ARTIFACT_PATH_FILE_PREFIX: &str = "p_";
/// 系数 WAV 格式字面值（T04 §2.1：单声道 IEEE float32，长度=taps，不归一化）。
pub const ARTIFACT_WAV_FORMAT: &str = "wav_f32_mono";
/// WAV 采样位宽（f32）。
pub const ARTIFACT_WAV_BITS: u16 = 32;
/// 规范 WAV 头字节数（RIFF/fmt/data，无扩展 chunk）。
pub const ARTIFACT_WAV_HEADER_BYTES: u32 = 44;
/// G 支撑/路径数量的硬上限（M<=8 -> 64 paths）。
pub const ARTIFACT_MAX_PATHS: usize = MAX_LOW_BAND_PARTICIPANTS * MAX_LOW_BAND_PARTICIPANTS;
/// T54 只接受 T51 的 G 设计支撑上限（TAPS_NORMAL_MAX=4096）；>4096 属 T56 扩展准入。
pub const ARTIFACT_MAX_TAPS: u32 = TAPS_NORMAL_MAX;
/// manifest 内 per-path 设计频带（模态校正带；不是运行期滤波）。
pub const ARTIFACT_PATH_BAND_HZ: (f64, f64) = MODAL_CORRECTION_BAND_HZ;
/// 诚实 claim（逐字写入 manifest；不得改写为实测改善）。
pub const ARTIFACT_CLAIM: &str = "design-side per-path coefficient artifact only; not applied, not compiled, not measured, no runtime validation; evidence gaps are recorded as INCOMPLETE/FAIL; 3 dB / 15% remain pre-declared engineering gates; real post-apply remeasurement and independent holdout are mandatory and NOT provided by this artifact";

/// status：设计侧证据自洽，可送 T66 编译（**不是** applied/measured）。
pub const STATUS_READY: &str = "READY";
/// status：证据缺失/无效（不能签字；仍保留已产出的系数用于审计）。
pub const STATUS_INCOMPLETE: &str = "INCOMPLETE";
/// status：明确门失败（优先级最高）。
pub const STATUS_FAIL: &str = "FAIL";

// status 码（稳定；manifest/evidence 使用；顺序即记录顺序）
/// FAIL：target 明确不可行（不得放宽帽伪装）。
pub const F_TARGET_INFEASIBLE: &str = "F_TARGET_INFEASIBLE";
/// FAIL：T53 holdout 预测门明确失败。
pub const F_HOLDOUT_PREDICTED_FAIL: &str = "F_HOLDOUT_PREDICTED_FAIL";
/// INCOMPLETE：上游没有可用解。
pub const R_SOLUTION_MISSING: &str = "R_SOLUTION_MISSING";
/// INCOMPLETE：解不可交付（best-effort/欠定/headroom 未过）。
pub const R_SOLUTION_NOT_DELIVERABLE: &str = "R_SOLUTION_NOT_DELIVERABLE";
/// INCOMPLETE：未提供冻结 P 基线（P detail 不可记录）。
pub const R_POST_BASELINE_MISSING: &str = "R_POST_BASELINE_MISSING";
/// INCOMPLETE：未提供 T53 holdout 报告。
pub const R_HOLDOUT_MISSING: &str = "R_HOLDOUT_MISSING";
/// INCOMPLETE：T53 holdout 证据不足/无效。
pub const R_HOLDOUT_INCOMPLETE: &str = "R_HOLDOUT_INCOMPLETE";
/// INCOMPLETE：T53 holdout 未执行。
pub const R_HOLDOUT_NOT_RUN: &str = "R_HOLDOUT_NOT_RUN";
/// INCOMPLETE：target 覆盖/输入不足。
pub const R_TARGET_INCOMPLETE: &str = "R_TARGET_INCOMPLETE";
/// INCOMPLETE：target 只部分可行。
pub const R_TARGET_NOT_FULLY_FEASIBLE: &str = "R_TARGET_NOT_FULLY_FEASIBLE";
/// INCOMPLETE：缺 measurement session hash（真实 raw 证据链不完整）。
pub const R_MEASUREMENT_PROVENANCE_MISSING: &str = "R_MEASUREMENT_PROVENANCE_MISSING";
/// INCOMPLETE：缺 calibration IR hash（真实校准证据链不完整）。
pub const R_CALIBRATION_PROVENANCE_MISSING: &str = "R_CALIBRATION_PROVENANCE_MISSING";
/// INCOMPLETE：缺 tuning inputs hash。
pub const R_TUNING_INPUTS_MISSING: &str = "R_TUNING_INPUTS_MISSING";

/// artifact 级 warning（不改变 status；manifest 原样带出）。
pub const W_ART_DESIGN_ONLY: &str = "W_ART_DESIGN_ONLY";
/// 未应用/未实测（恒带）。
pub const W_ART_NOT_APPLIED: &str = "W_ART_NOT_APPLIED";
/// 必须应用后真实复测。
pub const W_ART_REQUIRES_POST_APPLY_REMEASUREMENT: &str = "W_ART_REQUIRES_POST_APPLY_REMEASUREMENT";
/// 系数为 f32 落盘；f64 设计值量化误差已复算并记录。
pub const W_ART_F32_QUANTIZED: &str = "W_ART_F32_QUANTIZED";

// ===========================================================================
// 错误（稳定码；不 panic）
// ===========================================================================

/// artifact 错误类别（稳定码前缀 E_ART_*）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactErrorKind {
    /// schema/版本/工具字段非法。
    Schema,
    /// tool name/version 非法。
    Tool,
    /// fs 不受支持（只接受 T50 设计 fs）。
    Fs,
    /// layout/身份非法。
    Layout,
    /// participants/roles 非法。
    Participants,
    /// overlay 绑定非法（缺失/重复/槽位越界/hash 非法）。
    Overlay,
    /// P 基线缺失或与 target 不匹配。
    Post,
    /// target 结构/校验失败。
    Target,
    /// 上游解缺失/不可用/不匹配。
    Solution,
    /// taps 非法/不匹配。
    Taps,
    /// D（delay）非法/不匹配。
    Delay,
    /// holdout 报告绑定不匹配/契约违规。
    Holdout,
    /// provenance hash 缺失/非法。
    Provenance,
    /// WAV 编码/解码失败。
    Wav,
    /// hash/确定性检查失败（篡改）。
    Hash,
    /// status/诚实字段契约违规。
    Status,
    /// 数值非法（NaN/Inf/越界）。
    Numeric,
    /// 文件写入失败（仅 write_to_dir）。
    Io,
}

impl ArtifactErrorKind {
    /// 稳定错误码。
    pub const fn code(self) -> &'static str {
        match self {
            ArtifactErrorKind::Schema => "E_ART_SCHEMA",
            ArtifactErrorKind::Tool => "E_ART_TOOL",
            ArtifactErrorKind::Fs => "E_ART_FS",
            ArtifactErrorKind::Layout => "E_ART_LAYOUT",
            ArtifactErrorKind::Participants => "E_ART_PARTICIPANTS",
            ArtifactErrorKind::Overlay => "E_ART_OVERLAY",
            ArtifactErrorKind::Post => "E_ART_POST",
            ArtifactErrorKind::Target => "E_ART_TARGET",
            ArtifactErrorKind::Solution => "E_ART_SOLUTION",
            ArtifactErrorKind::Taps => "E_ART_TAPS",
            ArtifactErrorKind::Delay => "E_ART_DELAY",
            ArtifactErrorKind::Holdout => "E_ART_HOLDOUT",
            ArtifactErrorKind::Provenance => "E_ART_PROVENANCE",
            ArtifactErrorKind::Wav => "E_ART_WAV",
            ArtifactErrorKind::Hash => "E_ART_HASH",
            ArtifactErrorKind::Status => "E_ART_STATUS",
            ArtifactErrorKind::Numeric => "E_ART_NUMERIC",
            ArtifactErrorKind::Io => "E_ART_IO",
        }
    }
}

/// artifact 错误（field 指出契约位置；detail 人读；不 panic）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    kind: ArtifactErrorKind,
    field: String,
    detail: String,
}

impl ArtifactError {
    /// 构造。
    pub fn new(
        kind: ArtifactErrorKind,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 类别。
    pub fn kind(&self) -> ArtifactErrorKind {
        self.kind
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// 契约字段。
    pub fn field(&self) -> &str {
        &self.field
    }

    /// 人读说明。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}: {}", self.code(), self.field, self.detail)
    }
}

impl std::error::Error for ArtifactError {}

fn err(
    kind: ArtifactErrorKind,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> ArtifactError {
    ArtifactError::new(kind, field, detail)
}

// ===========================================================================
// WAV f32 mono 编解码（T04 §2.1：IEEE float32、mono、fs、长度=taps、不归一化）
// ===========================================================================

/// 解码后的单声道 f32 WAV。
#[derive(Debug, Clone, PartialEq)]
pub struct WavF32Mono {
    /// 采样率（= 设计 fs）。
    pub sample_rate_hz: u32,
    /// 样本（长度 = taps）。
    pub samples: Vec<f32>,
}

/// 编码单声道 f32 WAV（规范 44 字节头；无 LIST/fact chunk；LE；确定性）。
///
/// 拒绝非有限样本（系数必须有限；NaN/Inf 不得进产物）。
pub fn encode_wav_f32_mono(sample_rate_hz: u32, samples: &[f32]) -> Result<Vec<u8>, ArtifactError> {
    if sample_rate_hz == 0 || sample_rate_hz > 384_000 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.sample_rate_hz",
            format!("sample rate {sample_rate_hz} outside 1..=384000"),
        ));
    }
    if samples.is_empty() {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.samples",
            "WAV must contain at least one sample",
        ));
    }
    if samples.len() > (u32::MAX as usize) / 4 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.samples",
            format!("{} samples exceed RIFF 32-bit size", samples.len()),
        ));
    }
    for (index, sample) in samples.iter().enumerate() {
        if !sample.is_finite() {
            return Err(err(
                ArtifactErrorKind::Wav,
                format!("wav.samples[{index}]"),
                "coefficient must be finite (no NaN/Inf)",
            ));
        }
    }
    let data_bytes = (samples.len() as u32) * 4;
    let riff_size = 36u32
        .checked_add(data_bytes)
        .ok_or_else(|| err(ArtifactErrorKind::Wav, "wav.size", "RIFF size overflow"))?;
    let byte_rate = sample_rate_hz.checked_mul(4).ok_or_else(|| {
        err(
            ArtifactErrorKind::Wav,
            "wav.byte_rate",
            "byte rate overflow",
        )
    })?;
    let mut bytes = Vec::with_capacity(ARTIFACT_WAV_HEADER_BYTES as usize + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_size.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sample_rate_hz.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes()); // block align (1ch * 4 bytes)
    bytes.extend_from_slice(&32u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

/// 严格解码单声道 f32 WAV（只接受 T04 §2.1 规范格式；任何偏差=错误，不尽力解析）。
pub fn decode_wav_f32_mono(bytes: &[u8]) -> Result<WavF32Mono, ArtifactError> {
    if bytes.len() < ARTIFACT_WAV_HEADER_BYTES as usize {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.header",
            format!(
                "WAV length {} < canonical header {}",
                bytes.len(),
                ARTIFACT_WAV_HEADER_BYTES
            ),
        ));
    }
    if &bytes[0..4] != b"RIFF" {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.riff",
            "missing RIFF magic",
        ));
    }
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if riff_size as usize != bytes.len() - 8 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.riff_size",
            format!("RIFF size {} != file length {} - 8", riff_size, bytes.len()),
        ));
    }
    if &bytes[8..12] != b"WAVE" {
        return Err(err(ArtifactErrorKind::Wav, "wav.wave", "missing WAVE form"));
    }
    if &bytes[12..16] != b"fmt " {
        return Err(err(ArtifactErrorKind::Wav, "wav.fmt", "missing fmt chunk"));
    }
    let fmt_size = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    if fmt_size != 16 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.fmt_size",
            format!("fmt chunk size {fmt_size} != 16"),
        ));
    }
    let format = u16::from_le_bytes([bytes[20], bytes[21]]);
    if format != 3 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.format",
            format!("WAVE format {format} != 3 (IEEE float)"),
        ));
    }
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    if channels != 1 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.channels",
            format!("channels {channels} != 1 (mono)"),
        ));
    }
    let sample_rate_hz = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let byte_rate = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
    let block_align = u16::from_le_bytes([bytes[32], bytes[33]]);
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    let expected_rate = sample_rate_hz.checked_mul(4).ok_or_else(|| {
        err(
            ArtifactErrorKind::Wav,
            "wav.byte_rate",
            "sample rate * 4 overflow",
        )
    })?;
    if sample_rate_hz == 0 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.sample_rate_hz",
            "zero rate",
        ));
    }
    if byte_rate != expected_rate || block_align != 4 || bits != ARTIFACT_WAV_BITS {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.fmt_fields",
            format!(
                "byte_rate {byte_rate} / block_align {block_align} / bits {bits} not canonical mono f32"
            ),
        ));
    }
    if &bytes[36..40] != b"data" {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.data",
            "missing data chunk",
        ));
    }
    let data_bytes = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);
    if data_bytes as usize != bytes.len() - 44 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.data_size",
            format!("data size {data_bytes} != payload {}", bytes.len() - 44),
        ));
    }
    if data_bytes % 4 != 0 {
        return Err(err(
            ArtifactErrorKind::Wav,
            "wav.data_size",
            "f32 payload not a multiple of 4 bytes",
        ));
    }
    let mut samples = Vec::with_capacity((data_bytes / 4) as usize);
    for chunk in bytes[44..].chunks_exact(4) {
        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if !sample.is_finite() {
            return Err(err(
                ArtifactErrorKind::Wav,
                "wav.samples",
                "decoded sample is NaN/Inf",
            ));
        }
        samples.push(sample);
    }
    Ok(WavF32Mono {
        sample_rate_hz,
        samples,
    })
}

// ===========================================================================
// overlay 绑定（T57 解析结果的适配输入；本 crate 不依赖 compat）
// ===========================================================================

/// overlay 中一个 role 的物理输出槽位（首槽位 = 该 role 的 DAC index）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRole {
    /// 角色 id（大写 token）。
    pub role_id: String,
    /// 物理输出槽位（严格升序、唯一、< container_channels）。
    pub outputs: Vec<u16>,
}

impl OverlayRole {
    /// 构造并校验（role token / outputs 非空升序唯一）。
    pub fn new(role_id: impl Into<String>, outputs: Vec<u16>) -> Result<Self, ArtifactError> {
        let role_id = role_id.into();
        if !is_role_token(&role_id) {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.role_id",
                format!("role id {role_id:?} must match ^[A-Z][A-Z0-9]*$"),
            ));
        }
        if outputs.is_empty() {
            return Err(err(
                ArtifactErrorKind::Overlay,
                format!("overlay.roles[{role_id}].outputs"),
                "role must map to at least one output slot",
            ));
        }
        let mut previous: Option<u16> = None;
        for output in &outputs {
            if let Some(previous) = previous {
                if *output <= previous {
                    return Err(err(
                        ArtifactErrorKind::Overlay,
                        format!("overlay.roles[{role_id}].outputs"),
                        "outputs must be strictly increasing and unique",
                    ));
                }
            }
            previous = Some(*output);
        }
        Ok(Self { role_id, outputs })
    }

    /// 首槽位（CONTRACT：role.index = 该 role outputs 的首个）。
    pub fn first_output(&self) -> u16 {
        self.outputs[0]
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        let mut text = format!("overlay_role role={}", self.role_id);
        for output in &self.outputs {
            text.push_str(&format!(" output={output}"));
        }
        text.push('\n');
        text
    }
}

/// 已解析的 overlay 绑定（overlay 是唯一权威；本模块只消费并校验，不解析 YAML）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayDacBinding {
    layout_id: String,
    source_sha256: String,
    container_channels: u16,
    roles: Vec<OverlayRole>,
}

impl OverlayDacBinding {
    /// 构造并校验（layout id / source sha256 / container 宽度 / role 唯一 / 槽位唯一）。
    pub fn new(
        layout_id: impl Into<String>,
        source_sha256: impl Into<String>,
        container_channels: u16,
        roles: Vec<OverlayRole>,
    ) -> Result<Self, ArtifactError> {
        let layout_id = layout_id.into();
        if !is_layout_token(&layout_id) {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.layout_id",
                format!("layout id {layout_id:?} must be a stable id token"),
            ));
        }
        let source_sha256 = source_sha256.into();
        if !is_sha256_hex(&source_sha256) {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.source_sha256",
                format!("{source_sha256:?} must be 64 lowercase hex digits (sha256)"),
            ));
        }
        if container_channels == 0 || container_channels > 64 {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.container_channels",
                format!("container channels {container_channels} outside 1..=64"),
            ));
        }
        if roles.is_empty() {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.roles",
                "overlay must declare at least one role",
            ));
        }
        if roles.len() > 64 {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.roles",
                format!("{} roles exceed 64", roles.len()),
            ));
        }
        let mut seen_roles: Vec<&str> = Vec::with_capacity(roles.len());
        let mut seen_outputs: Vec<u16> = Vec::new();
        for role in &roles {
            if seen_roles.iter().any(|existing| *existing == role.role_id) {
                return Err(err(
                    ArtifactErrorKind::Overlay,
                    format!("overlay.roles[{}]", role.role_id),
                    "duplicate role id",
                ));
            }
            seen_roles.push(&role.role_id);
            for output in &role.outputs {
                if *output >= container_channels {
                    return Err(err(
                        ArtifactErrorKind::Overlay,
                        format!("overlay.roles[{}].outputs", role.role_id),
                        format!(
                            "output slot {output} outside container width {container_channels}"
                        ),
                    ));
                }
                if seen_outputs.contains(output) {
                    return Err(err(
                        ArtifactErrorKind::Overlay,
                        format!("overlay.roles[{}].outputs", role.role_id),
                        format!("output slot {output} mapped by more than one role"),
                    ));
                }
                seen_outputs.push(*output);
            }
        }
        Ok(Self {
            layout_id,
            source_sha256,
            container_channels,
            roles,
        })
    }

    /// layout id。
    pub fn layout_id(&self) -> &str {
        &self.layout_id
    }

    /// overlay 源字节 sha256（版本绑定证据）。
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    /// 容器通道数。
    pub fn container_channels(&self) -> u16 {
        self.container_channels
    }

    /// roles（输入顺序）。
    pub fn roles(&self) -> &[OverlayRole] {
        &self.roles
    }

    /// 按 role 取。
    pub fn role(&self, role_id: &str) -> Option<&OverlayRole> {
        self.roles.iter().find(|role| role.role_id == role_id)
    }

    /// 未被任何 role 映射的输出槽位（4.1.2 应为 [6]）。
    pub fn unused_outputs(&self) -> Vec<u16> {
        let mut used = vec![false; self.container_channels as usize];
        for role in &self.roles {
            for output in &role.outputs {
                used[*output as usize] = true;
            }
        }
        (0..self.container_channels)
            .filter(|output| !used[*output as usize])
            .collect()
    }

    /// canonical 文本（roles 按 role_id 排序；确定性）。
    pub fn canonical(&self) -> String {
        let mut text = format!(
            "overlay layout={} source_sha256={} container={}\n",
            self.layout_id, self.source_sha256, self.container_channels
        );
        let mut roles: Vec<&OverlayRole> = self.roles.iter().collect();
        roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
        for role in roles {
            text.push_str(&role.canonical());
        }
        text
    }

    /// 绑定 hash（role->DAC 映射身份；manifest/role_hash 使用）。
    pub fn binding_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ===========================================================================
// tool / provenance
// ===========================================================================

/// 生成 tool 身份（manifest 必填；deterministic）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactTool {
    /// tool 名。
    pub name: String,
    /// tool 版本（同输入同版本才承诺字节一致）。
    pub version: String,
}

impl ArtifactTool {
    /// 构造并校验（非空/长度/无控制字符）。
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Result<Self, ArtifactError> {
        let name = name.into();
        let version = version.into();
        if !is_tool_field(&name) {
            return Err(err(
                ArtifactErrorKind::Tool,
                "tool.name",
                format!("tool name {name:?} must be non-empty printable ASCII (<=64)"),
            ));
        }
        if !is_tool_field(&version) {
            return Err(err(
                ArtifactErrorKind::Tool,
                "tool.version",
                format!("tool version {version:?} must be non-empty printable ASCII (<=64)"),
            ));
        }
        Ok(Self { name, version })
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        format!("tool={}@{}", self.name, self.version)
    }
}

/// 证据链 provenance（P/profile/measurement/calibration/tuning/host graph hash）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactProvenance {
    /// profile/P 文档 hash（T57/channelmap profile；变化必须重算）。
    pub profile_sha256: String,
    /// T48/T49 measurement session hash（真实 raw 证据；缺失=INCOMPLETE）。
    pub measurement_sha256: Option<String>,
    /// T13 calibration IR / 校准源 hash（缺失=INCOMPLETE）。
    pub calibration_sha256: Option<String>,
    /// tuning inputs hash（T53 声明；缺失=INCOMPLETE）。
    pub tuning_inputs_sha256: Option<String>,
    /// 编译/图版本 hash（T66 在 compile 后回填；设计期为 None）。
    pub host_graph_sha256: Option<String>,
}

impl ArtifactProvenance {
    /// 构造（profile hash 必填；其余可选，缺失会在 status 记录 INCOMPLETE 码）。
    pub fn new(
        profile_sha256: impl Into<String>,
        measurement_sha256: Option<String>,
        calibration_sha256: Option<String>,
    ) -> Self {
        Self {
            profile_sha256: profile_sha256.into(),
            measurement_sha256,
            calibration_sha256,
            tuning_inputs_sha256: None,
            host_graph_sha256: None,
        }
    }

    /// 带 tuning inputs hash。
    pub fn with_tuning_inputs(mut self, tuning_inputs_sha256: impl Into<String>) -> Self {
        self.tuning_inputs_sha256 = Some(tuning_inputs_sha256.into());
        self
    }

    /// 带 host graph hash（T66 编译后回填）。
    pub fn with_host_graph(mut self, host_graph_sha256: impl Into<String>) -> Self {
        self.host_graph_sha256 = Some(host_graph_sha256.into());
        self
    }

    /// 校验全部出现的 hash（64 位小写 hex）。
    pub fn validate(&self) -> Result<(), ArtifactError> {
        check_sha256("provenance.profile_sha256", &self.profile_sha256)?;
        if let Some(value) = &self.measurement_sha256 {
            check_sha256("provenance.measurement_sha256", value)?;
        }
        if let Some(value) = &self.calibration_sha256 {
            check_sha256("provenance.calibration_sha256", value)?;
        }
        if let Some(value) = &self.tuning_inputs_sha256 {
            check_sha256("provenance.tuning_inputs_sha256", value)?;
        }
        if let Some(value) = &self.host_graph_sha256 {
            check_sha256("provenance.host_graph_sha256", value)?;
        }
        Ok(())
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        format!(
            "provenance profile_sha256={} measurement_sha256={} calibration_sha256={} tuning_inputs_sha256={} host_graph_sha256={}",
            self.profile_sha256,
            self.measurement_sha256.as_deref().unwrap_or("none"),
            self.calibration_sha256.as_deref().unwrap_or("none"),
            self.tuning_inputs_sha256.as_deref().unwrap_or("none"),
            self.host_graph_sha256.as_deref().unwrap_or("none")
        )
    }
}

// ===========================================================================
// 公开产物结构（manifest 字段的强类型镜像；测试/T66/T59 直接消费）
// ===========================================================================

/// artifact 状态（优先级 FAIL > INCOMPLETE > READY）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStatus {
    /// 设计侧证据自洽（仍非 applied/measured）。
    Ready,
    /// 证据缺失/无效。
    Incomplete,
    /// 明确门失败。
    Fail,
}

impl ArtifactStatus {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactStatus::Ready => STATUS_READY,
            ArtifactStatus::Incomplete => STATUS_INCOMPLETE,
            ArtifactStatus::Fail => STATUS_FAIL,
        }
    }
}

/// 一个 artifact 文件（相对路径 + 字节；顺序确定）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactFile {
    /// 相对 artifact 根的路径（不得绝对/含 ..）。
    pub rel_path: String,
    /// 文件字节。
    pub bytes: Vec<u8>,
}

/// 一个 participant 的 dense/role/DAC 记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactParticipant {
    /// dense 索引（G 行/列）。
    pub dense: u16,
    /// 角色 id。
    pub role_id: String,
    /// 容器/DAC 槽位（overlay 首槽位）。
    pub dac: u32,
}

/// manifest 记录的 overlay 绑定（解析结果只读快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactOverlay {
    /// layout id。
    pub layout_id: String,
    /// overlay 源字节 sha256。
    pub source_sha256: String,
    /// 容器通道数。
    pub container_channels: u16,
    /// role->outputs（按 role_id 排序；确定性）。
    pub roles: Vec<OverlayRole>,
    /// 未被任何 role 使用的输出槽位（4.1.2 = [6]）。
    pub unused_outputs: Vec<u16>,
    /// overlay 绑定 hash。
    pub binding_hash: String,
}

/// 输出 scope（participants + stereo mask + overlay DAC + role hash）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactScope {
    /// layout id。
    pub layout_id: String,
    /// 设计 fs。
    pub fs: u32,
    /// M。
    pub m: usize,
    /// 容器通道数。
    pub container_channels: u16,
    /// participants（dense 顺序）。
    pub participants: Vec<ArtifactParticipant>,
    /// dense -> DAC。
    pub dense_to_dac: Vec<u32>,
    /// participants 绑定 hash（T50 canonical hash）。
    pub participants_binding_hash: String,
    /// 是否 stereo strict 受限 profile。
    pub stereo_strict: bool,
    /// stereo mask（stereo strict 时恒 FL/FR；否则空）。
    pub stereo_mask: Vec<String>,
    /// 合并 role 绑定 hash（layout+participants+dense->DAC+stereo mask+overlay）。
    pub role_hash: String,
    /// overlay 快照。
    pub overlay: ArtifactOverlay,
}

/// P_post 单角色记录（P 阶段只按 hash 绑定；字段为审计/下游校验用）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactPostRole {
    /// 角色 id。
    pub role_id: String,
    /// 容器/DAC 槽位。
    pub dac: u32,
    /// P_post 整数延迟（samples）。
    pub delay_samples: u32,
    /// 极性（+1/-1）。
    pub polarity: i8,
    /// role gain（dB）。
    pub gain_db: f64,
    /// 保护带限字面值。
    pub band_limit: &'static str,
    /// 人工 delay（必须 0）。
    pub artificial_delay_ms: f64,
    /// 是否有 modal PEQ（PEQ 只在 P 阶段应用一次）。
    pub modal_peq: bool,
}

/// manifest 的 P 绑定（baseline hash + 可选 role 明细）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactPost {
    /// 是否提供冻结 P 基线（缺失 -> INCOMPLETE）。
    pub present: bool,
    /// T50/P 基线 hash（= target.baseline_hash()）。
    pub baseline_hash: String,
    /// profile/P 文档 hash（provenance）。
    pub profile_sha256: String,
    /// P 基线 fs。
    pub sample_rate_hz: u32,
    /// P participants 绑定 hash。
    pub participants_binding_hash: String,
    /// P 基线 fit inputs hash。
    pub fit_inputs_sha256: String,
    /// P 的 tone binding hash。
    pub tone_binding_hash: String,
    /// P 的 curve binding hash。
    pub curve_binding_hash: String,
    /// P roles（顺序同 P 基线声明）。
    pub roles: Vec<ArtifactPostRole>,
    /// P roles canonical hash（审计）。
    pub post_roles_sha256: String,
}

/// 一个 T20 判据频带的 decay 目标记录。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactDecayBand {
    /// 1/3 oct 带中心 Hz。
    pub band_hz: f64,
    /// 基线实测 T60（s）。
    pub baseline_t60_s: f64,
    /// 目标 T*60（s）。
    pub target_t60_s: f64,
}

/// decay/T20 目标声明（不是实测）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactDecay {
    /// 模式字面值（preserve/reduced）。
    pub mode: &'static str,
    /// ratio。
    pub ratio: f64,
    /// 目标下降百分比（声明）。
    pub reduction_percent: f64,
    /// 是否满足 T6 T20 >=15% 目标口径（实测仍须 T53/T68）。
    pub meets_t20_gate: bool,
    /// 频带。
    pub bands: Vec<ArtifactDecayBand>,
}

/// tone 绑定记录（单次应用证明）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactTone {
    /// curve id。
    pub curve_id: String,
    /// shape 字面值。
    pub shape: &'static str,
    /// 参考增益 dB。
    pub reference_gain_db: f64,
    /// binding hash。
    pub binding_hash: String,
    /// 应用证据（1/0/0）。
    pub post_baseline_applications: u32,
    /// 应用证据（0）。
    pub low_band_residual_applications: u32,
    /// 应用证据（0）。
    pub target_applications: u32,
}

/// curve 绑定记录（最多一次应用；G 不二次乘）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactCurve {
    /// curve id。
    pub curve_id: String,
    /// 应用点字面值。
    pub site: &'static str,
    /// 应用次数。
    pub applications: u32,
    /// binding hash。
    pub binding_hash: String,
    /// target 声明 hash（可选）。
    pub target_sha256: Option<String>,
    /// 是否通过单次应用校验。
    pub single_application: bool,
}

/// height role 全频 scope 记录。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactHeightRole {
    /// 角色 id。
    pub role_id: String,
    /// DAC 槽位。
    pub dac: u32,
    /// 全频带 Hz。
    pub band_hz: (f64, f64),
    /// 保护带限字面值（必须 none）。
    pub band_limit: &'static str,
    /// 人工 delay（必须 0）。
    pub artificial_delay_ms: f64,
    /// 是否在低带矩阵（必须 false）。
    pub in_low_band_matrix: bool,
}

/// height 全频 scope（与低带矩阵显式分开）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactHeightScope {
    /// 是否存在（stereo strict 可无）。
    pub present: bool,
    /// 是否排除低带矩阵。
    pub excluded_from_low_band_matrix: bool,
    /// binding hash。
    pub binding_hash: String,
    /// roles。
    pub roles: Vec<ArtifactHeightRole>,
}

/// output scope 单项（审计 role->DAC->矩阵归属）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactOutputScopeEntry {
    /// 角色 id。
    pub role_id: String,
    /// DAC 槽位。
    pub dac: u32,
    /// kind 字面值（low_band_matrix/full_range_height/other）。
    pub kind: &'static str,
    /// 声明频带。
    pub band_hz: (f64, f64),
    /// 保护带限字面值。
    pub band_limit: &'static str,
}

/// T50 target 诊断（只报告，不冒充改善）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactTargetDiagnostics {
    /// design position 数。
    pub design_positions: usize,
    /// 改善带网格覆盖率。
    pub improvement_band_coverage: f64,
    /// 带内所需修正最大值 dB。
    pub required_correction_max_db: f64,
    /// 近零 H_eff 点数。
    pub near_zero_points: usize,
    /// target 预留的 validation labels（不得进入 target）。
    pub validation_positions_reserved: Vec<String>,
}

/// target/decay/tone/curve/height/scope 设计绑定（manifest 的 design 段）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactDesign {
    /// target profile 字面值（multichannel_4x2_5/stereo_strict_2）。
    pub profile: &'static str,
    /// G 支撑 L_G。
    pub taps: u32,
    /// G 公共整数延迟 D（samples@fs）。
    pub delay_samples: u32,
    /// 次样本延迟（本任务恒 0）。
    pub delay_q16: u32,
    /// 设计 fs。
    pub fs: u32,
    /// 联合频率网格点数。
    pub grid_points: usize,
    /// 拟合带。
    pub fit_band_hz: (f64, f64),
    /// 改善带（T6 63-250）。
    pub improvement_band_hz: (f64, f64),
    /// 模态校正带（per-path 设计声明）。
    pub modal_band_hz: (f64, f64),
    /// target 声明附加延迟（samples）。
    pub target_delay_samples: u32,
    /// 公共 D0（samples）。
    pub common_d0_samples: i64,
    /// decay 目标。
    pub decay: ArtifactDecay,
    /// tone 单次应用。
    pub tone: ArtifactTone,
    /// curve 单次应用。
    pub curve: ArtifactCurve,
    /// height scope。
    pub height_scope: ArtifactHeightScope,
    /// output scope 表。
    pub output_scope: Vec<ArtifactOutputScopeEntry>,
    /// target 诊断。
    pub diagnostics: ArtifactTargetDiagnostics,
    /// target warnings 码。
    pub target_warnings: Vec<&'static str>,
}

/// T52 selection 摘要（solve 变体为 None）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactSelection {
    /// 选择方法。
    pub method: &'static str,
    /// CV 是否有效。
    pub cv_valid: bool,
    /// CV 无效原因。
    pub cv_reason: Option<&'static str>,
    /// 选中白名单。
    pub selected_whitelist: &'static str,
    /// 选中 lambda（归一前）。
    pub selected_lambda: f64,
    /// 实际 ridge。
    pub selected_lambda_used: f64,
    /// 适用边界状态。
    pub applicability: &'static str,
    /// 是否用回退。
    pub fallback_used: bool,
    /// headroom 回退次数。
    pub headroom_fallback_attempts: usize,
}

/// T52 headroom 摘要（PR-3；solve 变体为 None）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactHeadroom {
    /// 是否全部通过。
    pub passed: bool,
    /// 修正路径帽 dB。
    pub correction_cap_db: f64,
    /// 对角路径帽 dB。
    pub diagonal_cap_db: f64,
    /// 容差 dB。
    pub tolerance_db: f64,
    /// 修正路径最大增益 dB。
    pub max_correction_gain_db: f64,
    /// 对角路径最大增益 dB。
    pub max_diagonal_gain_db: f64,
    /// 最坏超限 dB（0=通过）。
    pub worst_excess_db: f64,
    /// 未通过的路径（out<-in）。
    pub violated_paths: Vec<String>,
}

/// 解摘要（report/hash/selection/headroom；manifest 的 solution 段）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactSolutionInfo {
    /// 变体字面值（solve/regularized）。
    pub kind: &'static str,
    /// 是否含解。
    pub present: bool,
    /// 解 hash（上游）。
    pub hash: Option<String>,
    /// 系数 canonical sha256（上游；已复算校验）。
    pub coeffs_sha256: Option<String>,
    /// 上游 base solve hash（regularized 变体）。
    pub base_solve_hash: Option<String>,
    /// 先验 hash（regularized 变体）。
    pub prior_hash: Option<String>,
    /// 是否可交付（设计侧）。
    pub deliverable: bool,
    /// report 状态字面值。
    pub report_status: &'static str,
    /// fs。
    pub fs: u32,
    /// taps。
    pub taps: u32,
    /// D。
    pub delay_samples: u32,
    /// 拟合带。
    pub fit_band_hz: (f64, f64),
    /// 改善带。
    pub improvement_band_hz: (f64, f64),
    /// 网格点数。
    pub grid_points: usize,
    /// design position 数。
    pub design_positions: usize,
    /// 设计侧改善带预测残差（dB；不是实测）。
    pub predicted_residual_max_db: Option<f64>,
    /// 设计侧拟合带预测残差（dB）。
    pub predicted_fit_band_residual_db: Option<f64>,
    /// selection 摘要。
    pub selection: Option<ArtifactSelection>,
    /// headroom 摘要。
    pub headroom: Option<ArtifactHeadroom>,
    /// warning 码。
    pub warning_codes: Vec<&'static str>,
}

/// T53 holdout 报告摘要（原样记录 verdict；不替代实测）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactHoldout {
    /// 是否提供报告。
    pub present: bool,
    /// schema。
    pub schema: &'static str,
    /// 报告内容 hash（T53 report_hash）。
    pub report_hash: String,
    /// verdict。
    pub verdict: &'static str,
    /// verdict code。
    pub verdict_code: &'static str,
    /// verdict reasons。
    pub verdict_reasons: Vec<&'static str>,
    /// 隔离 gate。
    pub isolation_gate: &'static str,
    /// 隔离 gate code。
    pub isolation_gate_code: &'static str,
    /// 预测 gate。
    pub prediction_gate: &'static str,
    /// 预测 gate code。
    pub prediction_gate_code: &'static str,
    /// T20 gate。
    pub t20_gate: &'static str,
    /// T20 gate code。
    pub t20_gate_code: &'static str,
    /// target hash（须与 target 一致）。
    pub target_sha256: String,
    /// baseline hash（须与 target/P 一致）。
    pub baseline_hash: String,
    /// design plant hash（须与 target 一致）。
    pub design_plant_hash: String,
    /// T53 的 ModalMatrix 结构提示 hash（solution_hash_hint；**不是** T51/T52 官方
    /// solve/regularize 解 hash，故不与 artifact solution.hash 强绑定）。
    pub solution_hash: Option<String>,
    /// holdout labels。
    pub holdout_labels: Vec<String>,
    /// tuning inputs hash。
    pub tuning_inputs_sha256: String,
    /// 预声明改善门 dB（回显）。
    pub declared_improvement_gate_db: f64,
    /// 预声明 T20 比例门（回显）。
    pub declared_t20_reduction_ratio: f64,
}

/// 一条 per-path 系数记录。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactPath {
    /// 输出 dense（G 行）。
    pub out_dense: u16,
    /// 输入 dense（G 列）。
    pub in_dense: u16,
    /// 输出角色。
    pub out_role: String,
    /// 输入角色。
    pub in_role: String,
    /// 输出 DAC。
    pub out_dac: u32,
    /// 输入 DAC。
    pub in_dac: u32,
    /// 相对 WAV 路径。
    pub file: String,
    /// 格式字面值（wav_f32_mono）。
    pub format: &'static str,
    /// fs。
    pub fs: u32,
    /// taps。
    pub taps: u32,
    /// baseline 延迟（对角=D；非对角=0）。
    pub delay_samples: u32,
    /// Q16 延迟（恒 0）。
    pub delay_q16: u32,
    /// 是否含 delta_D 对角 baseline（仅对角）。
    pub diag_baseline: bool,
    /// 是否全零 cell（稀疏加载可跳过；对角仍须 baseline）。
    pub zero: bool,
    /// 设计频带（模态校正带）。
    pub band_hz: (f64, f64),
    /// WAV 字节数。
    pub bytes: u64,
    /// WAV sha256。
    pub sha256: String,
    /// 该 path f64 系数的 canonical sha256。
    pub f64_sha256: String,
    /// 该 path f64 系数峰值绝对值。
    pub peak_abs: f64,
}

/// 精度/量化复算（fs 精度口径；WBS T54）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactQuantization {
    /// WAV 采样格式字面值。
    pub sample_format: &'static str,
    /// 位宽。
    pub bits_per_sample: u16,
    /// 上游解 f64 系数 canonical sha256（已复算校验）。
    pub coeffs_f64_sha256: String,
    /// 落盘 f32 系数 canonical sha256（由 WAV 解码复算）。
    pub coeffs_f32_sha256: String,
    /// f64->f32 最大绝对误差。
    pub max_abs_f32_error: f64,
    /// 全部 path sha256 的聚合 hash。
    pub wav_aggregate_sha256: String,
}

/// 诚实字段（与 T51/T52/T53 同口径；恒不冒充 applied/measured）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactHonesty {
    /// 是否应用（恒 false）。
    pub applied: bool,
    /// 是否实测（恒 false）。
    pub measured: bool,
    /// 应用后实测状态（恒 not_run）。
    pub post_apply_measurement: &'static str,
    /// holdout 是否参与调参（恒 false）。
    pub holdout_used_for_tuning: bool,
    /// 是否能替代真实复测（恒 false）。
    pub can_substitute_real_remeasurement: bool,
    /// 是否要求应用后复测（恒 true）。
    pub requires_post_apply_remeasurement: bool,
    /// 是否已编译（恒 false）。
    pub compiled: bool,
    /// 诚实 claim。
    pub claims: &'static str,
}

/// manifest 身份/hash 链（T50/T51/T52/T53/provenance）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactIdentity {
    /// target schema。
    pub target_schema: &'static str,
    /// target id。
    pub target_id: String,
    /// target 状态（T50 可行性口径，不是实测）。
    pub target_status: &'static str,
    /// target sha256。
    pub target_sha256: String,
    /// 冻结 P 基线 hash。
    pub baseline_hash: String,
    /// design-only H_eff plant hash（target 身份）。
    pub design_plant_hash: String,
    /// design plant 实算 hash（T53 identity.design_plant_hash_observed；提供时强绑定）。
    pub design_plant_hash_observed: Option<String>,
    /// H_eff plant hash（含 holdout；provenance）。
    pub plant_hash: String,
    /// G 解 hash（存在时）。
    pub solution_hash: Option<String>,
    /// 解变体（solve/regularized）。
    pub solution_kind: &'static str,
    /// T53 报告 hash（存在时）。
    pub holdout_report_hash: Option<String>,
    /// profile/P 文档 hash。
    pub profile_sha256: String,
    /// measurement session hash。
    pub measurement_sha256: Option<String>,
    /// calibration IR hash。
    pub calibration_sha256: Option<String>,
    /// tuning inputs hash。
    pub tuning_inputs_sha256: Option<String>,
    /// host graph hash（T66 编译后回填）。
    pub host_graph_sha256: Option<String>,
}

/// 完整 artifact（manifest 强类型 + 文件字节 + 内容 hash）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactBundle {
    /// schema。
    pub schema: &'static str,
    /// 整数版本。
    pub artifact_version: u32,
    /// 内容 hash 算法。
    pub hash_algo: &'static str,
    /// tool。
    pub tool: ArtifactTool,
    /// 创建时间（元数据；不参与 artifact hash；None=确定性模式）。
    pub created_unix: Option<u64>,
    /// 状态。
    pub status: ArtifactStatus,
    /// 状态码（固定顺序）。
    pub status_codes: Vec<&'static str>,
    /// 身份/hash 链。
    pub identity: ArtifactIdentity,
    /// role/scope/overlay/stereo mask。
    pub scope: ArtifactScope,
    /// P 绑定。
    pub post: ArtifactPost,
    /// 设计绑定。
    pub design: ArtifactDesign,
    /// 解摘要。
    pub solution: ArtifactSolutionInfo,
    /// T53 摘要。
    pub holdout: ArtifactHoldout,
    /// per-path 记录。
    pub paths: Vec<ArtifactPath>,
    /// 精度/量化。
    pub quantization: ArtifactQuantization,
    /// 诚实字段。
    pub honesty: ArtifactHonesty,
    /// artifact 级 warning。
    pub warnings: Vec<&'static str>,
    /// 内容 hash（canonical 文本 sha256；不含 created_unix/artifact_hash 自身）。
    pub artifact_hash: String,
    /// manifest 文件 sha256。
    pub manifest_sha256: String,
    /// 全部文件（manifest 在首位；其余按 (out,in) 路径顺序）。
    pub files: Vec<ArtifactFile>,
}

/// 上游解变体（T51 solve 或 T52 regularize；两者系数布局一致）。
#[derive(Clone, Copy, Debug)]
pub enum ArtifactSolution<'a> {
    /// T51 纯 solve 产物。
    Solve(&'a SolveOutcome),
    /// T52 正则化产物（生产路径）。
    Regularized(&'a RegularizeOutcome),
}

impl<'a> ArtifactSolution<'a> {
    /// 变体字面值。
    pub fn kind(&self) -> &'static str {
        match self {
            ArtifactSolution::Solve(_) => "solve",
            ArtifactSolution::Regularized(_) => "regularized",
        }
    }

    fn report(&self) -> DesignReport {
        match self {
            ArtifactSolution::Solve(outcome) => {
                let report = &outcome.report;
                DesignReport {
                    kind: "solve",
                    fs: report.sample_rate_hz,
                    taps: report.taps,
                    delay_samples: report.delay_samples,
                    m: report.m,
                    fit_band_hz: report.fit_band_hz,
                    improvement_band_hz: (63.0, 250.0),
                    grid_points: report.grid_points,
                    design_positions: report.design_positions,
                    deliverable: outcome.solution.is_some() && report.status.is_usable(),
                    report_status: report.status.as_str(),
                    predicted_residual_max_db: Some(report.predicted_design_residual_max_db),
                    predicted_fit_band_residual_db: Some(report.predicted_fit_band_residual_max_db),
                    warnings: report.warnings.iter().map(|warning| warning.code).collect(),
                    selection: None,
                    headroom: None,
                    prior_hash: None,
                    solution_hash: outcome
                        .solution
                        .as_ref()
                        .map(|solution| solution.hash.clone()),
                    coeffs_sha256: outcome
                        .solution
                        .as_ref()
                        .map(|solution| solution.coeffs_sha256.clone()),
                    base_solve_hash: None,
                    applied: report.applied,
                    post_apply_measurement: report.post_apply_measurement,
                }
            }
            ArtifactSolution::Regularized(outcome) => {
                let report = &outcome.report;
                let selection = ArtifactSelection {
                    method: report.selection.method,
                    cv_valid: report.selection.cv_valid,
                    cv_reason: report.selection.cv_reason,
                    selected_whitelist: report.selection.selected_whitelist.as_str(),
                    selected_lambda: report.selection.selected_lambda,
                    selected_lambda_used: report.selection.selected_lambda_used,
                    applicability: report.applicability.status,
                    fallback_used: report.selection.fallback_used,
                    headroom_fallback_attempts: report.selection.headroom_fallback_attempts,
                };
                let violated_paths: Vec<String> = report
                    .headroom
                    .paths
                    .iter()
                    .filter(|path| !path.passed)
                    .map(|path| format!("{}<-{}", path.out_role, path.in_role))
                    .collect();
                let headroom = ArtifactHeadroom {
                    passed: report.headroom.passed,
                    correction_cap_db: report.headroom.correction_cap_db,
                    diagonal_cap_db: report.headroom.diagonal_cap_db,
                    tolerance_db: report.headroom.tolerance_db,
                    max_correction_gain_db: report.headroom.max_correction_gain_db,
                    max_diagonal_gain_db: report.headroom.max_diagonal_gain_db,
                    worst_excess_db: report.headroom.worst_excess_db,
                    violated_paths,
                };
                DesignReport {
                    kind: "regularized",
                    fs: report.sample_rate_hz,
                    taps: report.taps,
                    delay_samples: report.delay_samples,
                    m: report.m,
                    fit_band_hz: report.fit_band_hz,
                    improvement_band_hz: report.improvement_band_hz,
                    grid_points: report.grid_points,
                    design_positions: report.design_positions,
                    deliverable: report.deliverable && outcome.solution.is_some(),
                    report_status: if report.deliverable {
                        "regularized_deliverable"
                    } else {
                        "regularized_not_deliverable"
                    },
                    predicted_residual_max_db: Some(report.final_residual.improvement_band_max_db),
                    predicted_fit_band_residual_db: Some(report.final_residual.fit_band_max_db),
                    warnings: report.warnings.iter().map(|warning| warning.code).collect(),
                    selection: Some(selection),
                    headroom: Some(headroom),
                    prior_hash: report.prior_hash.clone(),
                    solution_hash: report.solution_hash.clone(),
                    coeffs_sha256: report.coeffs_sha256.clone(),
                    base_solve_hash: report.base_solve_hash.clone(),
                    applied: report.applied,
                    post_apply_measurement: report.post_apply_measurement,
                }
            }
        }
    }

    fn solution(&self) -> Option<SolutionView<'a>> {
        match self {
            ArtifactSolution::Solve(outcome) => {
                outcome.solution.as_ref().map(|solution| SolutionView {
                    modal: &solution.modal,
                    coeffs_f64: &solution.coeffs_f64,
                    coeffs_f32: &solution.coeffs_f32,
                    coeffs_sha256: &solution.coeffs_sha256,
                    hash: &solution.hash,
                    base_solve_hash: None,
                    prior_hash: None,
                    kind: "solve",
                })
            }
            ArtifactSolution::Regularized(outcome) => {
                outcome.solution.as_ref().map(|solution| SolutionView {
                    modal: &solution.modal,
                    coeffs_f64: &solution.coeffs_f64,
                    coeffs_f32: &solution.coeffs_f32,
                    coeffs_sha256: &solution.coeffs_sha256,
                    hash: &solution.hash,
                    base_solve_hash: solution.base_solve_hash.as_deref(),
                    prior_hash: Some(solution.prior_hash.as_str()),
                    kind: "regularized",
                })
            }
        }
    }
}

/// 解视图（统一 T51/T52 的字段布局）。
#[derive(Clone, Copy)]
struct SolutionView<'a> {
    modal: &'a ModalMatrix,
    coeffs_f64: &'a [f64],
    coeffs_f32: &'a [f32],
    coeffs_sha256: &'a str,
    hash: &'a str,
    base_solve_hash: Option<&'a str>,
    prior_hash: Option<&'a str>,
    kind: &'static str,
}

/// report 摘要（统一 T51/T52 report 语义）。
#[derive(Clone)]
struct DesignReport {
    kind: &'static str,
    fs: u32,
    taps: usize,
    delay_samples: u32,
    m: usize,
    fit_band_hz: (f64, f64),
    improvement_band_hz: (f64, f64),
    grid_points: usize,
    design_positions: usize,
    deliverable: bool,
    report_status: &'static str,
    predicted_residual_max_db: Option<f64>,
    predicted_fit_band_residual_db: Option<f64>,
    warnings: Vec<&'static str>,
    selection: Option<ArtifactSelection>,
    headroom: Option<ArtifactHeadroom>,
    prior_hash: Option<String>,
    solution_hash: Option<String>,
    coeffs_sha256: Option<String>,
    base_solve_hash: Option<String>,
    applied: bool,
    post_apply_measurement: &'static str,
}

/// artifact 构建输入（全部只读借用；不做 I/O）。
pub struct ArtifactInput<'a> {
    /// T50 target。
    pub target: &'a TargetMatrix,
    /// T50 冻结 P 基线（P detail；缺失 -> INCOMPLETE）。
    pub post: Option<&'a FrozenPostBaseline>,
    /// T51/T52 解。
    pub solution: ArtifactSolution<'a>,
    /// T53 holdout 报告（缺失 -> INCOMPLETE）。
    pub holdout: Option<&'a HoldoutReport>,
    /// design plant 实算 hash（可选；提供时强绑定 T53 identity.design_plant_hash_observed）。
    pub design_plant_hash: Option<&'a str>,
    /// T57 overlay 绑定。
    pub overlay: &'a OverlayDacBinding,
    /// 证据链 provenance。
    pub provenance: ArtifactProvenance,
    /// tool 身份。
    pub tool: ArtifactTool,
    /// 创建时间（元数据；None=确定性模式）。
    pub created_unix: Option<u64>,
}

impl<'a> ArtifactInput<'a> {
    /// 构造（必需项）。
    pub fn new(
        target: &'a TargetMatrix,
        solution: ArtifactSolution<'a>,
        overlay: &'a OverlayDacBinding,
        tool: ArtifactTool,
        provenance: ArtifactProvenance,
    ) -> Self {
        Self {
            target,
            post: None,
            solution,
            holdout: None,
            design_plant_hash: None,
            overlay,
            provenance,
            tool,
            created_unix: None,
        }
    }

    /// 带 P 基线。
    pub fn with_post(mut self, post: &'a FrozenPostBaseline) -> Self {
        self.post = Some(post);
        self
    }

    /// 带 T53 报告。
    pub fn with_holdout(mut self, report: &'a HoldoutReport) -> Self {
        self.holdout = Some(report);
        self
    }

    /// 带 design plant 实算 hash（强绑定 T53 observed hash）。
    pub fn with_design_plant_hash(mut self, design_plant_hash: &'a str) -> Self {
        self.design_plant_hash = Some(design_plant_hash);
        self
    }

    /// 带创建时间（只写 manifest，不进 hash）。
    pub fn with_created_unix(mut self, created_unix: u64) -> Self {
        self.created_unix = Some(created_unix);
        self
    }
}

// ===========================================================================
// 入口：构建 artifact（纯内存；不写文件；不做 I/O）
// ===========================================================================

/// 构建 artifact（每 path WAV + manifest；确定性；不写文件）。
///
/// 契约不满足 -> ArtifactError（不 panic）；证据缺失 -> 成功返回但 status=INCOMPLETE/FAIL
/// 并在 status_codes 记录原因（真实数据缺失不伪造 PASS）。
pub fn build_artifact(input: &ArtifactInput<'_>) -> Result<ArtifactBundle, ArtifactError> {
    input.provenance.validate()?;
    let target = input.target;
    let fs = target.sample_rate_hz();
    if !SAMPLE_RATES_HZ.contains(&fs) {
        return Err(err(
            ArtifactErrorKind::Fs,
            "target.sample_rate_hz",
            format!(
                "fs {fs} is not a P1 design rate {:?} (WAV fs must equal the design fs)",
                SAMPLE_RATES_HZ
            ),
        ));
    }
    let m = target.m();
    if m == 0 || m > MAX_LOW_BAND_PARTICIPANTS {
        return Err(err(
            ArtifactErrorKind::Participants,
            "target.participants",
            format!("M={m} outside 1..={MAX_LOW_BAND_PARTICIPANTS}"),
        ));
    }
    let participants = target.participants();
    if !(participants.is_p1_explicit() || participants.is_stereo_strict()) {
        return Err(err(
            ArtifactErrorKind::Participants,
            "target.participants",
            format!(
                "participants {:?} match neither the explicit 5x5 4.1.2 scope nor the stereo strict FL/FR scope (5.3.2/M=8 is NOT_VERIFIED)",
                participants.role_ids()
            ),
        ));
    }
    if target.applied() {
        return Err(err(
            ArtifactErrorKind::Status,
            "target.applied",
            "target claims applied=true; artifact only accepts design-side (not applied) targets",
        ));
    }
    if target.post_apply_measurement() != "not_requested" {
        return Err(err(
            ArtifactErrorKind::Status,
            "target.post_apply_measurement",
            format!(
                "target post_apply_measurement={} must be not_requested (design-side target)",
                target.post_apply_measurement()
            ),
        ));
    }
    target
        .verify_structure()
        .map_err(|error| map_target("target.structure", error))?;
    target
        .verify_tone_single_application()
        .map_err(|error| map_target("target.tone", error))?;
    target
        .curve()
        .validate_single_application()
        .map_err(|error| map_target("target.curve", error))?;

    let scope = build_scope(target, input.overlay)?;
    let report = input.solution.report();
    let solution = input.solution.solution();
    validate_report_binding(&report, target, fs, m)?;
    if report.kind != input.solution.kind() {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.kind",
            "internal solution kind mismatch",
        ));
    }
    if let Some(view) = &solution {
        validate_solution_data(&report, view, target)?;
    }

    let post = build_post(target, input.post, &input.provenance)?;
    let holdout = build_holdout(target, input.holdout, input.design_plant_hash)?;
    let mut design = build_design(target, &report, &post, &scope)?;
    let tuning_inputs_sha256 = resolve_tuning_inputs(&input.provenance, &holdout);
    let identity = build_identity(
        target,
        &report,
        solution.as_ref(),
        &holdout,
        &input.provenance,
        tuning_inputs_sha256.clone(),
        input.design_plant_hash,
    );
    let solution_info = build_solution_info(report.clone(), solution.as_ref());
    if let Some(view) = &solution {
        if let Some(report_hash) = &solution_info.hash {
            if report_hash != view.hash {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "solution.hash",
                    "solution hash changed while assembling artifact",
                ));
            }
        }
    }
    design.tone.post_baseline_applications = target.tone_applications().post_baseline();
    design.tone.low_band_residual_applications = target.tone_applications().low_band_residual();
    design.tone.target_applications = target.tone_applications().target();

    let mut status_codes: Vec<&'static str> = Vec::new();
    collect_status_codes(
        target,
        &solution_info,
        &post,
        &holdout,
        &input.provenance,
        tuning_inputs_sha256.as_deref(),
        &mut status_codes,
    );
    let status = status_from_codes(&status_codes);

    let (paths, coefficient_files, quantization) = match &solution {
        Some(view) => build_paths(target, &report, view)?,
        None => (
            Vec::new(),
            Vec::new(),
            ArtifactQuantization {
                sample_format: ARTIFACT_WAV_FORMAT,
                bits_per_sample: ARTIFACT_WAV_BITS,
                coeffs_f64_sha256: report.coeffs_sha256.clone().unwrap_or_default(),
                coeffs_f32_sha256: String::new(),
                max_abs_f32_error: 0.0,
                wav_aggregate_sha256: String::new(),
            },
        ),
    };

    let mut bundle = ArtifactBundle {
        schema: ARTIFACT_SCHEMA,
        artifact_version: ARTIFACT_VERSION,
        hash_algo: ARTIFACT_HASH_ALGO,
        tool: input.tool.clone(),
        created_unix: input.created_unix,
        status,
        status_codes,
        identity,
        scope,
        post,
        design,
        solution: solution_info,
        holdout,
        paths,
        quantization,
        honesty: ArtifactHonesty {
            applied: false,
            measured: false,
            post_apply_measurement: "not_run",
            holdout_used_for_tuning: false,
            can_substitute_real_remeasurement: false,
            requires_post_apply_remeasurement: true,
            compiled: false,
            claims: ARTIFACT_CLAIM,
        },
        warnings: vec![
            W_ART_DESIGN_ONLY,
            W_ART_NOT_APPLIED,
            W_ART_REQUIRES_POST_APPLY_REMEASUREMENT,
            W_ART_F32_QUANTIZED,
        ],
        artifact_hash: String::new(),
        manifest_sha256: String::new(),
        files: Vec::new(),
    };
    let canonical = bundle.canonical();
    bundle.artifact_hash = sha256_hex(canonical.as_bytes());
    let manifest_bytes = bundle.to_manifest_json().into_bytes();
    bundle.manifest_sha256 = sha256_hex(&manifest_bytes);
    let mut files: Vec<ArtifactFile> = Vec::with_capacity(1 + coefficient_files.len());
    files.push(ArtifactFile {
        rel_path: ARTIFACT_MANIFEST_FILE.to_string(),
        bytes: manifest_bytes,
    });
    files.extend(coefficient_files);
    bundle.files = files;
    Ok(bundle)
}

fn map_target(field: &str, error: TargetError) -> ArtifactError {
    err(
        ArtifactErrorKind::Target,
        field,
        format!("{}: {}", error.code(), error.detail()),
    )
}

fn check_sha256(field: &str, value: &str) -> Result<(), ArtifactError> {
    if !is_sha256_hex(value) {
        return Err(err(
            ArtifactErrorKind::Provenance,
            field,
            format!("{value:?} must be 64 lowercase hex digits (sha256)"),
        ));
    }
    Ok(())
}

fn validate_report_binding(
    report: &DesignReport,
    target: &TargetMatrix,
    fs: u32,
    m: usize,
) -> Result<(), ArtifactError> {
    if report.fs != fs {
        return Err(err(
            ArtifactErrorKind::Fs,
            "solution.sample_rate_hz",
            format!("solution fs {} != target fs {fs}", report.fs),
        ));
    }
    if report.m != m {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.m",
            format!("solution M {} != target M {m}", report.m),
        ));
    }
    if report.taps == 0 || report.taps > ARTIFACT_MAX_TAPS as usize {
        return Err(err(
            ArtifactErrorKind::Taps,
            "solution.taps",
            format!(
                "taps {} outside 1..={} (G design support cap; >4096 is T56 extended admission)",
                report.taps, ARTIFACT_MAX_TAPS
            ),
        ));
    }
    if report.delay_samples as usize >= report.taps {
        return Err(err(
            ArtifactErrorKind::Delay,
            "solution.delay_samples",
            format!(
                "D={} >= taps={} violates delta_D baseline support (L_G >= D+1)",
                report.delay_samples, report.taps
            ),
        ));
    }
    if report.delay_samples != target.target_delay_samples() {
        return Err(err(
            ArtifactErrorKind::Delay,
            "solution.delay_samples",
            format!(
                "G baseline D={} != target.target_delay_samples()={} (T51/T52 requires alignment)",
                report.delay_samples,
                target.target_delay_samples()
            ),
        ));
    }
    if report.grid_points == 0 || report.grid_points != target.grid_hz().len() {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.grid_points",
            format!(
                "solution grid_points {} != target grid {}",
                report.grid_points,
                target.grid_hz().len()
            ),
        ));
    }
    if report.design_positions == 0 {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.design_positions",
            "solution has zero design positions",
        ));
    }
    if report.applied {
        return Err(err(
            ArtifactErrorKind::Status,
            "solution.applied",
            "upstream solution claims applied=true; only design-side solutions are accepted",
        ));
    }
    if report.post_apply_measurement != "not_requested" {
        return Err(err(
            ArtifactErrorKind::Status,
            "solution.post_apply_measurement",
            format!(
                "solution post_apply_measurement={} must be not_requested",
                report.post_apply_measurement
            ),
        ));
    }
    if !bands_equal(report.improvement_band_hz, (63.0, 250.0)) {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.improvement_band_hz",
            format!(
                "improvement band {:?} != frozen (63, 250) Hz",
                report.improvement_band_hz
            ),
        ));
    }
    Ok(())
}

fn validate_solution_data(
    report: &DesignReport,
    view: &SolutionView<'_>,
    target: &TargetMatrix,
) -> Result<(), ArtifactError> {
    let m = target.m();
    let taps = report.taps;
    let participants = target.participants().participants();
    if view.modal.m() != m {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.modal.m",
            format!("G M {} != target M {m}", view.modal.m()),
        ));
    }
    let expected_roles: Vec<&str> = participants
        .iter()
        .map(|participant| participant.role_id.as_str())
        .collect();
    let actual_roles: Vec<&str> = view.modal.roles().iter().map(String::as_str).collect();
    if actual_roles != expected_roles {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.roles",
            format!("G roles {actual_roles:?} != participants {expected_roles:?}"),
        ));
    }
    let target_grid = target.grid_hz();
    if view.modal.grid_hz().len() != target_grid.len() {
        return Err(err(
            ArtifactErrorKind::Solution,
            "solution.grid_hz",
            format!(
                "G grid {} points != target grid {} points",
                view.modal.grid_hz().len(),
                target_grid.len()
            ),
        ));
    }
    for (index, (left, right)) in view
        .modal
        .grid_hz()
        .iter()
        .zip(target_grid.iter())
        .enumerate()
    {
        if !grid_point_equal(*left, *right) {
            return Err(err(
                ArtifactErrorKind::Solution,
                format!("solution.grid_hz[{index}]"),
                format!("G grid {left} != target grid {right}"),
            ));
        }
    }
    for out_dense in 0..m {
        for in_dense in 0..m {
            if view.modal.cell(out_dense as u16, in_dense as u16).is_none() {
                return Err(err(
                    ArtifactErrorKind::Solution,
                    "solution.modal.cells",
                    format!("missing G cell (out={out_dense}, in={in_dense})"),
                ));
            }
        }
    }
    let expected_len = m
        .checked_mul(m)
        .and_then(|value| value.checked_mul(taps))
        .ok_or_else(|| {
            err(
                ArtifactErrorKind::Taps,
                "solution.coeffs",
                "M*M*taps overflows usize",
            )
        })?;
    if view.coeffs_f64.len() != expected_len {
        return Err(err(
            ArtifactErrorKind::Taps,
            "solution.coeffs_f64",
            format!(
                "coeffs_f64 len {} != M*M*taps {expected_len}",
                view.coeffs_f64.len()
            ),
        ));
    }
    if view.coeffs_f32.len() != expected_len {
        return Err(err(
            ArtifactErrorKind::Taps,
            "solution.coeffs_f32",
            format!(
                "coeffs_f32 len {} != M*M*taps {expected_len}",
                view.coeffs_f32.len()
            ),
        ));
    }
    for (index, value) in view.coeffs_f64.iter().enumerate() {
        if !value.is_finite() {
            return Err(err(
                ArtifactErrorKind::Numeric,
                format!("solution.coeffs_f64[{index}]"),
                "coefficient is NaN/Inf",
            ));
        }
    }
    for (index, value) in view.coeffs_f32.iter().enumerate() {
        if !value.is_finite() {
            return Err(err(
                ArtifactErrorKind::Numeric,
                format!("solution.coeffs_f32[{index}]"),
                "coefficient is NaN/Inf",
            ));
        }
        let cast = view.coeffs_f64[index] as f32;
        if cast.to_bits() != value.to_bits() {
            return Err(err(
                ArtifactErrorKind::Hash,
                format!("solution.coeffs_f32[{index}]"),
                format!(
                    "f32 coefficient {value} is not the canonical cast of f64 {}",
                    view.coeffs_f64[index]
                ),
            ));
        }
    }
    let recomputed = match view.kind {
        "solve" => solve_coefficient_sha256(view.coeffs_f64),
        _ => regularize_coefficient_sha256(view.coeffs_f64),
    };
    if recomputed != view.coeffs_sha256 {
        return Err(err(
            ArtifactErrorKind::Hash,
            "solution.coeffs_sha256",
            format!(
                "recomputed coefficient sha256 {recomputed} != upstream {}",
                view.coeffs_sha256
            ),
        ));
    }
    if !is_sha256_hex(view.hash) {
        return Err(err(
            ArtifactErrorKind::Hash,
            "solution.hash",
            "solution hash must be 64 lowercase hex digits",
        ));
    }
    if let Some(report_hash) = &report.solution_hash {
        if report_hash != view.hash {
            return Err(err(
                ArtifactErrorKind::Solution,
                "solution.hash",
                format!("report solution hash {report_hash} != G hash {}", view.hash),
            ));
        }
    }
    if let Some(report_sha) = &report.coeffs_sha256 {
        if report_sha != view.coeffs_sha256 {
            return Err(err(
                ArtifactErrorKind::Solution,
                "solution.coeffs_sha256",
                "report coeffs_sha256 != G coeffs_sha256",
            ));
        }
    }
    if let (Some(left), Some(right)) = (report.base_solve_hash.as_deref(), view.base_solve_hash) {
        if left != right {
            return Err(err(
                ArtifactErrorKind::Solution,
                "solution.base_solve_hash",
                "report base_solve_hash != G base_solve_hash",
            ));
        }
    }
    if let (Some(left), Some(right)) = (report.prior_hash.as_deref(), view.prior_hash) {
        if left != right {
            return Err(err(
                ArtifactErrorKind::Solution,
                "solution.prior_hash",
                "report prior_hash != G prior_hash",
            ));
        }
    }
    Ok(())
}

fn build_scope(
    target: &TargetMatrix,
    overlay: &OverlayDacBinding,
) -> Result<ArtifactScope, ArtifactError> {
    let layout = target.layout_id();
    let fs = target.sample_rate_hz();
    if overlay.layout_id() != layout {
        return Err(err(
            ArtifactErrorKind::Layout,
            "overlay.layout_id",
            format!(
                "overlay layout {} != target layout {layout}",
                overlay.layout_id()
            ),
        ));
    }
    let m = target.m();
    if u32::from(overlay.container_channels()) < m as u32 {
        return Err(err(
            ArtifactErrorKind::Overlay,
            "overlay.container_channels",
            format!("overlay container {} < M={m}", overlay.container_channels()),
        ));
    }
    let mut participants = Vec::with_capacity(m);
    let mut dense_to_dac = Vec::with_capacity(m);
    for participant in target.participants().participants() {
        let role = overlay.role(&participant.role_id).ok_or_else(|| {
            err(
                ArtifactErrorKind::Overlay,
                "scope.participants",
                format!(
                    "participant {} is not mapped by the resolved overlay (actual overlay must be parsed, not guessed)",
                    participant.role_id
                ),
            )
        })?;
        if u32::from(role.first_output()) != participant.out_index {
            return Err(err(
                ArtifactErrorKind::Overlay,
                format!("overlay.roles[{}].outputs", participant.role_id),
                format!(
                    "overlay first output {} != participant dense{} DAC {}",
                    role.first_output(),
                    participant.dense_index,
                    participant.out_index
                ),
            ));
        }
        participants.push(ArtifactParticipant {
            dense: participant.dense_index,
            role_id: participant.role_id.clone(),
            dac: participant.out_index,
        });
        dense_to_dac.push(participant.out_index);
    }
    if layout == crate::target::P1_LAYOUT_4_1_2 {
        if overlay.container_channels() != 8 {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.container_channels",
                format!(
                    "4.1.2 overlay requires 8 container channels, got {}",
                    overlay.container_channels()
                ),
            ));
        }
        let unused = overlay.unused_outputs();
        if unused != vec![6] {
            return Err(err(
                ArtifactErrorKind::Overlay,
                "overlay.unused_outputs",
                format!(
                    "4.1.2 overlay requires ch6 empty (unused outputs == [6]), got {unused:?}; refusing a silent mapping change"
                ),
            ));
        }
    }
    if let Some(height_scope) = target.height_scope() {
        for role in height_scope.roles() {
            if target.participants().contains_role(&role.role_id) {
                return Err(err(
                    ArtifactErrorKind::Participants,
                    format!("height_scope.{}", role.role_id),
                    "height role must stay outside the low-band MIMO matrix",
                ));
            }
            let overlay_role = overlay.role(&role.role_id).ok_or_else(|| {
                err(
                    ArtifactErrorKind::Overlay,
                    format!("overlay.roles[{}]", role.role_id),
                    "height role declared by the frozen P basis is not mapped by the overlay",
                )
            })?;
            if u32::from(overlay_role.first_output()) != role.out_index {
                return Err(err(
                    ArtifactErrorKind::Overlay,
                    format!("overlay.roles[{}].outputs", role.role_id),
                    format!(
                        "overlay first output {} != height scope DAC {}",
                        overlay_role.first_output(),
                        role.out_index
                    ),
                ));
            }
        }
    }
    let stereo_strict = target.participants().is_stereo_strict();
    let stereo_mask: Vec<String> = if stereo_strict {
        crate::target::STEREO_STRICT_OUTPUT_ROLE_IDS
            .iter()
            .map(|role| (*role).to_string())
            .collect()
    } else {
        Vec::new()
    };
    let mut overlay_roles: Vec<OverlayRole> = overlay.roles().to_vec();
    overlay_roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
    let overlay_snapshot = ArtifactOverlay {
        layout_id: layout.to_string(),
        source_sha256: overlay.source_sha256().to_string(),
        container_channels: overlay.container_channels(),
        roles: overlay_roles,
        unused_outputs: overlay.unused_outputs(),
        binding_hash: overlay.binding_hash(),
    };
    let role_hash = role_binding_hash(
        layout,
        fs,
        overlay.container_channels(),
        &participants,
        stereo_strict,
        &stereo_mask,
        &overlay_snapshot,
    );
    Ok(ArtifactScope {
        layout_id: layout.to_string(),
        fs,
        m,
        container_channels: overlay.container_channels(),
        participants,
        dense_to_dac,
        participants_binding_hash: target.participants().binding_hash(),
        stereo_strict,
        stereo_mask,
        role_hash,
        overlay: overlay_snapshot,
    })
}

fn role_binding_hash(
    layout: &str,
    fs: u32,
    container_channels: u16,
    participants: &[ArtifactParticipant],
    stereo_strict: bool,
    stereo_mask: &[String],
    overlay: &ArtifactOverlay,
) -> String {
    let mut text = format!("role_binding layout={layout} fs={fs} container={container_channels}\n");
    for participant in participants {
        text.push_str(&format!(
            "participant dense={} role={} dac={}\n",
            participant.dense, participant.role_id, participant.dac
        ));
    }
    text.push_str(&format!(
        "stereo_strict={} stereo_mask={}\n",
        stereo_strict,
        stereo_mask.join(",")
    ));
    text.push_str(&format!(
        "overlay_source_sha256={} overlay_binding_hash={}\n",
        overlay.source_sha256, overlay.binding_hash
    ));
    sha256_hex(text.as_bytes())
}

fn build_post(
    target: &TargetMatrix,
    post: Option<&FrozenPostBaseline>,
    provenance: &ArtifactProvenance,
) -> Result<ArtifactPost, ArtifactError> {
    let fs = target.sample_rate_hz();
    let Some(post) = post else {
        return Ok(ArtifactPost {
            present: false,
            baseline_hash: target.baseline_hash().to_string(),
            profile_sha256: provenance.profile_sha256.clone(),
            sample_rate_hz: fs,
            participants_binding_hash: String::new(),
            fit_inputs_sha256: String::new(),
            tone_binding_hash: String::new(),
            curve_binding_hash: String::new(),
            roles: Vec::new(),
            post_roles_sha256: String::new(),
        });
    };
    if post.sample_rate_hz() != fs {
        return Err(err(
            ArtifactErrorKind::Post,
            "post.sample_rate_hz",
            format!("P baseline fs {} != target fs {fs}", post.sample_rate_hz()),
        ));
    }
    if post.baseline_hash() != target.baseline_hash() {
        return Err(err(
            ArtifactErrorKind::Post,
            "post.baseline_hash",
            format!(
                "P baseline hash {} != target baseline_hash {}",
                post.baseline_hash(),
                target.baseline_hash()
            ),
        ));
    }
    if post.participants().binding_hash() != target.participants().binding_hash() {
        return Err(err(
            ArtifactErrorKind::Post,
            "post.participants",
            "P participants differ from target participants (G would not be interchangeable)",
        ));
    }
    if post.tone().binding_hash() != target.tone().binding_hash()
        || post.curve().binding_hash() != target.curve().binding_hash()
    {
        return Err(err(
            ArtifactErrorKind::Post,
            "post.tone_curve",
            "P baseline tone/curve binding differs from target (curve must not double-apply)",
        ));
    }
    if post.common_d0().samples() != target.common_d0().samples() {
        return Err(err(
            ArtifactErrorKind::Post,
            "post.common_d0",
            "P baseline common D0 differs from target",
        ));
    }
    post.verify_single_tone_application()
        .map_err(|error| map_target("post.tone", error))?;
    if let Some(target_height) = target.height_scope() {
        let post_height = post
            .optional_height_scope()
            .map_err(|error| map_target("post.height_scope", error))?;
        match post_height {
            Some(post_height) => {
                if post_height.binding_hash() != target_height.binding_hash() {
                    return Err(err(
                        ArtifactErrorKind::Post,
                        "post.height_scope",
                        "P height scope differs from target height scope",
                    ));
                }
            }
            None => {
                return Err(err(
                    ArtifactErrorKind::Post,
                    "post.height_scope",
                    "P baseline has no height scope while target declares one",
                ));
            }
        }
    }
    let mut roles = Vec::with_capacity(post.roles().len());
    for role in post.roles() {
        roles.push(ArtifactPostRole {
            role_id: role.role_id().to_string(),
            dac: role.out_index(),
            delay_samples: role.delay_samples(),
            polarity: role.polarity(),
            gain_db: role.gain_db(),
            band_limit: band_limit_str(role.band_limit()),
            artificial_delay_ms: role.artificial_delay_ms(),
            modal_peq: role.modal_peq().is_some(),
        });
    }
    for participant in target.participants().participants() {
        let role = roles
            .iter()
            .find(|role| role.role_id == participant.role_id)
            .ok_or_else(|| {
                err(
                    ArtifactErrorKind::Post,
                    "post.roles",
                    format!(
                        "P baseline has no role for participant {}",
                        participant.role_id
                    ),
                )
            })?;
        if role.dac != participant.out_index {
            return Err(err(
                ArtifactErrorKind::Post,
                format!("post.roles[{}].dac", role.role_id),
                format!(
                    "P role DAC {} != participant DAC {}",
                    role.dac, participant.out_index
                ),
            ));
        }
    }
    if let Some(height_scope) = target.height_scope() {
        for height in height_scope.roles() {
            let role = roles
                .iter()
                .find(|role| role.role_id == height.role_id)
                .ok_or_else(|| {
                    err(
                        ArtifactErrorKind::Post,
                        format!("post.roles[{}]", height.role_id),
                        "P baseline has no height role declared by the target height scope",
                    )
                })?;
            if role.dac != height.out_index
                || role.band_limit != "none"
                || role.artificial_delay_ms != 0.0
            {
                return Err(err(
                    ArtifactErrorKind::Post,
                    format!("post.roles[{}]", height.role_id),
                    "height role must be full-range (band_limit none, artificial delay 0, same DAC)",
                ));
            }
        }
    }
    let post_roles_sha256 = sha256_hex(post_roles_canonical(&roles).as_bytes());
    Ok(ArtifactPost {
        present: true,
        baseline_hash: post.baseline_hash().to_string(),
        profile_sha256: provenance.profile_sha256.clone(),
        sample_rate_hz: post.sample_rate_hz(),
        participants_binding_hash: post.participants().binding_hash(),
        fit_inputs_sha256: post.fit_inputs_sha256().to_string(),
        tone_binding_hash: post.tone().binding_hash(),
        curve_binding_hash: post.curve().binding_hash(),
        roles,
        post_roles_sha256,
    })
}

fn post_roles_canonical(roles: &[ArtifactPostRole]) -> String {
    let mut text = String::new();
    for role in roles {
        text.push_str(&format!(
            "post_role role={} dac={} delay={} polarity={} gain_db={} band_limit={} artificial_delay_ms={} modal_peq={}\n",
            role.role_id,
            role.dac,
            role.delay_samples,
            role.polarity,
            fmt_f64(role.gain_db),
            role.band_limit,
            fmt_f64(role.artificial_delay_ms),
            role.modal_peq
        ));
    }
    text
}

fn build_design(
    target: &TargetMatrix,
    report: &DesignReport,
    post: &ArtifactPost,
    scope: &ArtifactScope,
) -> Result<ArtifactDesign, ArtifactError> {
    let fs = target.sample_rate_hz();
    let decay = target.decay();
    let mut bands = Vec::with_capacity(decay.bands().len());
    for band in decay.bands() {
        let target_t60 = decay.target_t60_s(band.band_hz).ok_or_else(|| {
            err(
                ArtifactErrorKind::Target,
                "target.decay.bands",
                format!("no target T60 for band {} Hz", band.band_hz),
            )
        })?;
        bands.push(ArtifactDecayBand {
            band_hz: band.band_hz,
            baseline_t60_s: band.baseline_t60_s,
            target_t60_s: target_t60,
        });
    }
    let tone = target.tone();
    let curve = target.curve();
    let height_scope = match target.height_scope() {
        Some(height_scope) => {
            let mut roles = Vec::with_capacity(height_scope.roles().len());
            for role in height_scope.roles() {
                if role.in_low_band_matrix {
                    return Err(err(
                        ArtifactErrorKind::Target,
                        format!("target.height_scope.{}", role.role_id),
                        "height role marked as low-band matrix participant",
                    ));
                }
                if role.band_limit != BandLimit::None || role.artificial_delay_ms != 0.0 {
                    return Err(err(
                        ArtifactErrorKind::Target,
                        format!("target.height_scope.{}", role.role_id),
                        "height role must stay full-range (no HP/LP/artificial delay)",
                    ));
                }
                if let Some(overlay_role) = scope
                    .overlay
                    .roles
                    .iter()
                    .find(|entry| entry.role_id == role.role_id)
                {
                    if u32::from(overlay_role.first_output()) != role.out_index {
                        return Err(err(
                            ArtifactErrorKind::Target,
                            format!("target.height_scope.{}.dac", role.role_id),
                            "height scope DAC differs from the resolved overlay mapping",
                        ));
                    }
                } else {
                    return Err(err(
                        ArtifactErrorKind::Overlay,
                        format!("overlay.roles[{}]", role.role_id),
                        "height role is not mapped by the resolved overlay",
                    ));
                }
                roles.push(ArtifactHeightRole {
                    role_id: role.role_id.clone(),
                    dac: role.out_index,
                    band_hz: role.band_hz,
                    band_limit: band_limit_str(role.band_limit),
                    artificial_delay_ms: role.artificial_delay_ms,
                    in_low_band_matrix: role.in_low_band_matrix,
                });
            }
            ArtifactHeightScope {
                present: true,
                excluded_from_low_band_matrix: height_scope.excluded_from_low_band_matrix(),
                binding_hash: height_scope.binding_hash(),
                roles,
            }
        }
        None => ArtifactHeightScope {
            present: false,
            excluded_from_low_band_matrix: true,
            binding_hash: String::new(),
            roles: Vec::new(),
        },
    };
    if target.participants().is_p1_explicit() && !height_scope.present {
        return Err(err(
            ArtifactErrorKind::Target,
            "target.height_scope",
            "P1 4.1.2 target must declare the full-range height scope (HFL/HFR outside the low-band matrix)",
        ));
    }
    let mut output_scope = Vec::with_capacity(target.output_scope().entries().len());
    for entry in target.output_scope().entries() {
        output_scope.push(ArtifactOutputScopeEntry {
            role_id: entry.role_id.clone(),
            dac: entry.out_index,
            kind: entry.kind.as_str(),
            band_hz: entry.band_hz,
            band_limit: band_limit_str(entry.band_limit),
        });
    }
    let diagnostics = target.diagnostics();
    let target_warnings: Vec<&'static str> = target
        .warnings()
        .iter()
        .map(|warning| warning.code)
        .collect();
    let curve_applications = curve.applications();
    if curve_applications > 1 {
        return Err(err(
            ArtifactErrorKind::Target,
            "target.curve.applications",
            format!("curve applied {curve_applications} times; curve must not double-apply (T13)"),
        ));
    }
    Ok(ArtifactDesign {
        profile: target.profile().as_str(),
        taps: report.taps as u32,
        delay_samples: report.delay_samples,
        delay_q16: 0,
        fs,
        grid_points: report.grid_points,
        fit_band_hz: report.fit_band_hz,
        improvement_band_hz: report.improvement_band_hz,
        modal_band_hz: ARTIFACT_PATH_BAND_HZ,
        target_delay_samples: target.target_delay_samples(),
        common_d0_samples: target.common_d0().samples(),
        decay: ArtifactDecay {
            mode: decay.mode().as_str(),
            ratio: decay.ratio(),
            reduction_percent: decay.reduction_percent(),
            meets_t20_gate: decay.meets_t20_gate(),
            bands,
        },
        tone: ArtifactTone {
            curve_id: tone.curve_id().to_string(),
            shape: tone.shape().as_str(),
            reference_gain_db: tone.reference_gain_db(),
            binding_hash: tone.binding_hash(),
            post_baseline_applications: target.tone_applications().post_baseline(),
            low_band_residual_applications: target.tone_applications().low_band_residual(),
            target_applications: target.tone_applications().target(),
        },
        curve: ArtifactCurve {
            curve_id: curve.curve_id().to_string(),
            site: curve.site().as_str(),
            applications: curve_applications,
            binding_hash: curve.binding_hash(),
            target_sha256: curve.target_sha256().map(ToString::to_string),
            single_application: true,
        },
        height_scope,
        output_scope,
        diagnostics: ArtifactTargetDiagnostics {
            design_positions: diagnostics.design_positions,
            improvement_band_coverage: diagnostics.improvement_band_coverage,
            required_correction_max_db: diagnostics.required_correction_max_db,
            near_zero_points: diagnostics.near_zero_points,
            validation_positions_reserved: diagnostics.validation_positions_reserved.clone(),
        },
        target_warnings,
    })
    .map(|design| {
        let _ = post;
        design
    })
}

fn build_identity(
    target: &TargetMatrix,
    report: &DesignReport,
    solution: Option<&SolutionView<'_>>,
    holdout: &ArtifactHoldout,
    provenance: &ArtifactProvenance,
    tuning_inputs_sha256: Option<String>,
    design_plant_hash_observed: Option<&str>,
) -> ArtifactIdentity {
    let solution_hash = match solution {
        Some(view) => Some(view.hash.to_string()),
        None => report.solution_hash.clone(),
    };
    ArtifactIdentity {
        target_schema: target.schema(),
        target_id: target.target_id().to_string(),
        target_status: target.status().as_str(),
        target_sha256: target.target_sha256(),
        baseline_hash: target.baseline_hash().to_string(),
        design_plant_hash: target.design_plant_hash().to_string(),
        design_plant_hash_observed: design_plant_hash_observed.map(ToString::to_string),
        plant_hash: target.plant_hash().to_string(),
        solution_hash,
        solution_kind: report.kind,
        holdout_report_hash: if holdout.present {
            Some(holdout.report_hash.clone())
        } else {
            None
        },
        profile_sha256: provenance.profile_sha256.clone(),
        measurement_sha256: provenance.measurement_sha256.clone(),
        calibration_sha256: provenance.calibration_sha256.clone(),
        tuning_inputs_sha256,
        host_graph_sha256: provenance.host_graph_sha256.clone(),
    }
}

fn build_solution_info(
    report: DesignReport,
    solution: Option<&SolutionView<'_>>,
) -> ArtifactSolutionInfo {
    let hash = match solution {
        Some(view) => Some(view.hash.to_string()),
        None => report.solution_hash.clone(),
    };
    let coeffs_sha256 = match solution {
        Some(view) => Some(view.coeffs_sha256.to_string()),
        None => report.coeffs_sha256.clone(),
    };
    let base_solve_hash = match solution {
        Some(view) => view
            .base_solve_hash
            .map(ToString::to_string)
            .or(report.base_solve_hash.clone()),
        None => report.base_solve_hash.clone(),
    };
    let prior_hash = match solution {
        Some(view) => view
            .prior_hash
            .map(ToString::to_string)
            .or(report.prior_hash.clone()),
        None => report.prior_hash.clone(),
    };
    ArtifactSolutionInfo {
        kind: report.kind,
        present: solution.is_some(),
        hash,
        coeffs_sha256,
        base_solve_hash,
        prior_hash,
        deliverable: report.deliverable,
        report_status: report.report_status,
        fs: report.fs,
        taps: report.taps as u32,
        delay_samples: report.delay_samples,
        fit_band_hz: report.fit_band_hz,
        improvement_band_hz: report.improvement_band_hz,
        grid_points: report.grid_points,
        design_positions: report.design_positions,
        predicted_residual_max_db: report.predicted_residual_max_db,
        predicted_fit_band_residual_db: report.predicted_fit_band_residual_db,
        selection: report.selection,
        headroom: report.headroom,
        warning_codes: report.warnings,
    }
}

fn build_holdout(
    target: &TargetMatrix,
    holdout: Option<&HoldoutReport>,
    design_plant_hash_observed: Option<&str>,
) -> Result<ArtifactHoldout, ArtifactError> {
    let Some(report) = holdout else {
        return Ok(ArtifactHoldout {
            present: false,
            schema: crate::validate::VALIDATE_REPORT_SCHEMA,
            report_hash: String::new(),
            verdict: "NOT_RUN",
            verdict_code: "not_run",
            verdict_reasons: Vec::new(),
            isolation_gate: "NOT_RUN",
            isolation_gate_code: "not_run",
            prediction_gate: "NOT_RUN",
            prediction_gate_code: "not_run",
            t20_gate: "NOT_RUN",
            t20_gate_code: "not_run",
            target_sha256: String::new(),
            baseline_hash: String::new(),
            design_plant_hash: String::new(),
            solution_hash: None,
            holdout_labels: Vec::new(),
            tuning_inputs_sha256: String::new(),
            declared_improvement_gate_db: 3.0,
            declared_t20_reduction_ratio: 0.85,
        });
    };
    let identity = &report.identity;
    if identity.target_sha256 != target.target_sha256() {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.target_sha256",
            "T53 report was computed for a different target (hash mismatch)",
        ));
    }
    if identity.baseline_hash != target.baseline_hash() {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.baseline_hash",
            "T53 report baseline hash differs from the frozen P baseline",
        ));
    }
    if identity.design_plant_hash != target.plant_hash() {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.design_plant_hash",
            "T53 report design plant provenance hash differs from target.plant_hash()",
        ));
    }
    if let Some(observed) = design_plant_hash_observed {
        if identity.design_plant_hash_observed != observed {
            return Err(err(
                ArtifactErrorKind::Holdout,
                "holdout.identity.design_plant_hash_observed",
                "T53 report observed design plant hash differs from the supplied design plant hash",
            ));
        }
    }
    if identity.m != target.m() {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.m",
            format!("T53 report M={} != target M={}", identity.m, target.m()),
        ));
    }
    let expected_roles: Vec<String> = target
        .participants()
        .role_ids()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    if identity.participants != expected_roles {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.participants",
            "T53 report participants differ from target participants",
        ));
    }
    if identity.sample_rate_hz != target.sample_rate_hz() {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.sample_rate_hz",
            "T53 report fs differs from target fs",
        ));
    }
    if identity.layout_id != target.layout_id() {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.identity.layout_id",
            "T53 report layout differs from target layout",
        ));
    }
    if report.applied || report.measured {
        return Err(err(
            ArtifactErrorKind::Status,
            "holdout.applied",
            "T53 report claims applied/measured; predicted-only contract violated",
        ));
    }
    if report.post_apply_measurement != "not_run" {
        return Err(err(
            ArtifactErrorKind::Status,
            "holdout.post_apply_measurement",
            "T53 post_apply_measurement must be not_run",
        ));
    }
    if report.holdout_used_for_tuning {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.holdout_used_for_tuning",
            "T53 report claims holdout used for tuning; artifact must never reuse holdout for tuning",
        ));
    }
    if report.can_substitute_real_remeasurement {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.can_substitute_real_remeasurement",
            "T53 report must not substitute real remeasurement",
        ));
    }
    if !report.requires_post_apply_remeasurement {
        return Err(err(
            ArtifactErrorKind::Holdout,
            "holdout.requires_post_apply_remeasurement",
            "T53 report must require post-apply remeasurement",
        ));
    }
    Ok(ArtifactHoldout {
        present: true,
        schema: report.schema,
        report_hash: report.report_hash.clone(),
        verdict: report.verdict.as_str(),
        verdict_code: report.verdict_code,
        verdict_reasons: report.verdict_reasons.clone(),
        isolation_gate: report.isolation.gate.as_str(),
        isolation_gate_code: report.isolation.gate_code,
        prediction_gate: report.prediction.gate.as_str(),
        prediction_gate_code: report.prediction.gate_code,
        t20_gate: report.t20.gate.as_str(),
        t20_gate_code: report.t20.gate_code,
        target_sha256: identity.target_sha256.clone(),
        baseline_hash: identity.baseline_hash.clone(),
        design_plant_hash: identity.design_plant_hash.clone(),
        solution_hash: identity.solution_hash.clone(),
        holdout_labels: report.isolation.holdout_labels.clone(),
        tuning_inputs_sha256: report.isolation.tuning_inputs_sha256.clone(),
        declared_improvement_gate_db: report.declared.improvement_gate_db,
        declared_t20_reduction_ratio: report.declared.t20_reduction_gate_ratio,
    })
}

fn resolve_tuning_inputs(
    provenance: &ArtifactProvenance,
    holdout: &ArtifactHoldout,
) -> Option<String> {
    if let Some(value) = &provenance.tuning_inputs_sha256 {
        return Some(value.clone());
    }
    if holdout.present && is_sha256_hex(&holdout.tuning_inputs_sha256) {
        return Some(holdout.tuning_inputs_sha256.clone());
    }
    None
}

fn collect_status_codes(
    target: &TargetMatrix,
    solution: &ArtifactSolutionInfo,
    post: &ArtifactPost,
    holdout: &ArtifactHoldout,
    provenance: &ArtifactProvenance,
    tuning_inputs_sha256: Option<&str>,
    codes: &mut Vec<&'static str>,
) {
    match target.status() {
        TargetStatus::Infeasible => codes.push(F_TARGET_INFEASIBLE),
        TargetStatus::Incomplete => codes.push(R_TARGET_INCOMPLETE),
        TargetStatus::PredictedPartial => codes.push(R_TARGET_NOT_FULLY_FEASIBLE),
        TargetStatus::Preserve | TargetStatus::PredictedPass => {}
    }
    if holdout.present {
        match holdout.verdict {
            "PREDICTED_FAIL" => codes.push(F_HOLDOUT_PREDICTED_FAIL),
            "INCOMPLETE" => codes.push(R_HOLDOUT_INCOMPLETE),
            "NOT_RUN" => codes.push(R_HOLDOUT_NOT_RUN),
            _ => {}
        }
    } else {
        codes.push(R_HOLDOUT_MISSING);
    }
    if !solution.present {
        codes.push(R_SOLUTION_MISSING);
    }
    if !solution.deliverable {
        codes.push(R_SOLUTION_NOT_DELIVERABLE);
    }
    if !post.present {
        codes.push(R_POST_BASELINE_MISSING);
    }
    if provenance.measurement_sha256.is_none() {
        codes.push(R_MEASUREMENT_PROVENANCE_MISSING);
    }
    if provenance.calibration_sha256.is_none() {
        codes.push(R_CALIBRATION_PROVENANCE_MISSING);
    }
    if tuning_inputs_sha256.is_none() {
        codes.push(R_TUNING_INPUTS_MISSING);
    }
    codes.dedup();
}

fn status_from_codes(codes: &[&'static str]) -> ArtifactStatus {
    if codes.iter().any(|code| code.starts_with('F')) {
        ArtifactStatus::Fail
    } else if codes.is_empty() {
        ArtifactStatus::Ready
    } else {
        ArtifactStatus::Incomplete
    }
}

fn build_paths(
    target: &TargetMatrix,
    report: &DesignReport,
    view: &SolutionView<'_>,
) -> Result<(Vec<ArtifactPath>, Vec<ArtifactFile>, ArtifactQuantization), ArtifactError> {
    let m = target.m();
    let taps = report.taps;
    let fs = target.sample_rate_hz();
    let participants = target.participants().participants();
    let mut paths = Vec::with_capacity(m * m);
    let mut files = Vec::with_capacity(m * m);
    let mut all_f32: Vec<f32> = Vec::with_capacity(m * m * taps);
    let mut aggregate = String::new();
    let mut max_abs_f32_error = 0.0f64;
    for out_dense in 0..m {
        for in_dense in 0..m {
            let offset = (out_dense * m + in_dense) * taps;
            let f64_slice = &view.coeffs_f64[offset..offset + taps];
            let f32_slice = &view.coeffs_f32[offset..offset + taps];
            let wav = encode_wav_f32_mono(fs, f32_slice)?;
            let sha256 = sha256_hex(&wav);
            let f64_sha = f64_path_sha256(f64_slice);
            let zero = f32_slice.iter().all(|value| *value == 0.0);
            // manifest records the exported f32 peak (WAV truth); f64 design peak stays in f64_sha256.
            let mut peak_abs = 0.0f64;
            for (value, cast) in f64_slice.iter().zip(f32_slice.iter()) {
                peak_abs = peak_abs.max(f64::from(cast.abs()));
                max_abs_f32_error = max_abs_f32_error.max((*value - f64::from(*cast)).abs());
            }
            let in_role = participants[in_dense].role_id.clone();
            let out_role = participants[out_dense].role_id.clone();
            let file = format!(
                "{ARTIFACT_COEFF_DIR}/{ARTIFACT_PATH_FILE_PREFIX}{in_role}__{out_role}.wav"
            );
            let diag_baseline = out_dense == in_dense;
            paths.push(ArtifactPath {
                out_dense: out_dense as u16,
                in_dense: in_dense as u16,
                out_role: out_role.clone(),
                in_role,
                out_dac: participants[out_dense].out_index,
                in_dac: participants[in_dense].out_index,
                file: file.clone(),
                format: ARTIFACT_WAV_FORMAT,
                fs,
                taps: taps as u32,
                delay_samples: if diag_baseline {
                    report.delay_samples
                } else {
                    0
                },
                delay_q16: 0,
                diag_baseline,
                zero,
                band_hz: ARTIFACT_PATH_BAND_HZ,
                bytes: wav.len() as u64,
                sha256: sha256.clone(),
                f64_sha256: f64_sha,
                peak_abs,
            });
            files.push(ArtifactFile {
                rel_path: file,
                bytes: wav,
            });
            aggregate.push_str(&sha256);
            aggregate.push('\n');
            all_f32.extend_from_slice(f32_slice);
        }
    }
    let quantization = ArtifactQuantization {
        sample_format: ARTIFACT_WAV_FORMAT,
        bits_per_sample: ARTIFACT_WAV_BITS,
        coeffs_f64_sha256: view.coeffs_sha256.to_string(),
        coeffs_f32_sha256: f32_bits_sha256(&all_f32),
        max_abs_f32_error,
        wav_aggregate_sha256: sha256_hex(aggregate.as_bytes()),
    };
    Ok((paths, files, quantization))
}

// ===========================================================================
// stale 上下文（当前设计/运行绑定；WBS §5 / T04 §7.4）
// ===========================================================================

/// 当前设计/运行上下文（与 artifact 记录逐项比较判 stale）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactCurrent {
    /// fs。
    pub sample_rate_hz: u32,
    /// layout id。
    pub layout_id: String,
    /// participants 绑定 hash。
    pub participants_binding_hash: String,
    /// role 绑定 hash。
    pub role_hash: String,
    /// overlay 源字节 sha256。
    pub overlay_source_sha256: String,
    /// P 基线 hash。
    pub post_baseline_hash: String,
    /// profile/P 文档 hash。
    pub profile_sha256: String,
    /// target sha256。
    pub target_sha256: String,
    /// 解 hash。
    pub solution_hash: String,
    /// 先验 hash（regularized）。
    pub prior_hash: Option<String>,
    /// taps。
    pub taps: u32,
    /// D。
    pub delay_samples: u32,
    /// T53 report hash（存在时）。
    pub holdout_report_hash: Option<String>,
}

impl ArtifactCurrent {
    /// 构造（全部绑定量显式；caller 从当前设计流程/运行期状态取值）。
    pub fn new(
        sample_rate_hz: u32,
        layout_id: impl Into<String>,
        participants_binding_hash: impl Into<String>,
        role_hash: impl Into<String>,
        overlay_source_sha256: impl Into<String>,
        post_baseline_hash: impl Into<String>,
        profile_sha256: impl Into<String>,
        target_sha256: impl Into<String>,
        solution_hash: impl Into<String>,
        prior_hash: Option<String>,
        taps: u32,
        delay_samples: u32,
        holdout_report_hash: Option<String>,
    ) -> Self {
        Self {
            sample_rate_hz,
            layout_id: layout_id.into(),
            participants_binding_hash: participants_binding_hash.into(),
            role_hash: role_hash.into(),
            overlay_source_sha256: overlay_source_sha256.into(),
            post_baseline_hash: post_baseline_hash.into(),
            profile_sha256: profile_sha256.into(),
            target_sha256: target_sha256.into(),
            solution_hash: solution_hash.into(),
            prior_hash,
            taps,
            delay_samples,
            holdout_report_hash,
        }
    }
}

/// stale 原因（固定顺序；任一即失效，必须重设计/重编译，禁止复用旧系数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStaleReason {
    /// fs 变化。
    Fs,
    /// layout 变化。
    Layout,
    /// participants/dense->DAC 绑定变化。
    Participants,
    /// role/overlay/stereo mask 合并 hash 变化。
    Roles,
    /// overlay 源字节变化（实际解析结果必须重新绑定）。
    Overlay,
    /// P 基线变化。
    PostBaseline,
    /// profile/P 文档变化。
    Profile,
    /// target 变化。
    Target,
    /// G 解变化（含先验/白名单/lambda 结果）。
    Solution,
    /// 先验变化。
    Prior,
    /// taps 变化。
    Taps,
    /// D 变化。
    Delay,
    /// T53 holdout 报告变化。
    Holdout,
}

impl ArtifactStaleReason {
    /// 稳定码。
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactStaleReason::Fs => "STALE_FS",
            ArtifactStaleReason::Layout => "STALE_LAYOUT",
            ArtifactStaleReason::Participants => "STALE_PARTICIPANTS",
            ArtifactStaleReason::Roles => "STALE_ROLES",
            ArtifactStaleReason::Overlay => "STALE_OVERLAY",
            ArtifactStaleReason::PostBaseline => "STALE_POST_BASELINE",
            ArtifactStaleReason::Profile => "STALE_PROFILE",
            ArtifactStaleReason::Target => "STALE_TARGET",
            ArtifactStaleReason::Solution => "STALE_SOLUTION",
            ArtifactStaleReason::Prior => "STALE_PRIOR",
            ArtifactStaleReason::Taps => "STALE_TAPS",
            ArtifactStaleReason::Delay => "STALE_DELAY",
            ArtifactStaleReason::Holdout => "STALE_HOLDOUT",
        }
    }

    /// 全部原因（固定顺序）。
    pub const ALL: [ArtifactStaleReason; 13] = [
        ArtifactStaleReason::Fs,
        ArtifactStaleReason::Layout,
        ArtifactStaleReason::Participants,
        ArtifactStaleReason::Roles,
        ArtifactStaleReason::Overlay,
        ArtifactStaleReason::PostBaseline,
        ArtifactStaleReason::Profile,
        ArtifactStaleReason::Target,
        ArtifactStaleReason::Solution,
        ArtifactStaleReason::Prior,
        ArtifactStaleReason::Taps,
        ArtifactStaleReason::Delay,
        ArtifactStaleReason::Holdout,
    ];
}

// ===========================================================================
// ArtifactBundle 访问器：canonical / manifest / verify / write / stale
// ===========================================================================

impl ArtifactBundle {
    /// artifact schema。
    pub const SCHEMA: &'static str = ARTIFACT_SCHEMA;

    /// 按相对路径取文件。
    pub fn file(&self, rel_path: &str) -> Option<&ArtifactFile> {
        self.files.iter().find(|file| file.rel_path == rel_path)
    }

    /// 全部系数 WAV 文件（按 (out,in) 顺序）。
    pub fn coefficient_files(&self) -> impl Iterator<Item = &ArtifactFile> {
        self.files
            .iter()
            .filter(|file| file.rel_path.starts_with(ARTIFACT_COEFF_DIR))
    }

    /// manifest 文件字节。
    pub fn manifest_bytes(&self) -> &[u8] {
        match self.files.first() {
            Some(file) => &file.bytes,
            None => &[],
        }
    }

    /// 当前 design/run 绑定（供 stale 比较）。
    pub fn current_binding(&self) -> ArtifactCurrent {
        ArtifactCurrent {
            sample_rate_hz: self.design.fs,
            layout_id: self.scope.layout_id.clone(),
            participants_binding_hash: self.scope.participants_binding_hash.clone(),
            role_hash: self.scope.role_hash.clone(),
            overlay_source_sha256: self.scope.overlay.source_sha256.clone(),
            post_baseline_hash: self.post.baseline_hash.clone(),
            profile_sha256: self.identity.profile_sha256.clone(),
            target_sha256: self.identity.target_sha256.clone(),
            solution_hash: self.identity.solution_hash.clone().unwrap_or_default(),
            prior_hash: self.solution.prior_hash.clone(),
            taps: self.design.taps,
            delay_samples: self.design.delay_samples,
            holdout_report_hash: self.identity.holdout_report_hash.clone(),
        }
    }

    /// 与当前上下文比较 stale 原因（空 = fresh；顺序固定）。
    pub fn stale_reasons(&self, current: &ArtifactCurrent) -> Vec<ArtifactStaleReason> {
        let mut reasons = Vec::new();
        if current.sample_rate_hz != self.design.fs {
            reasons.push(ArtifactStaleReason::Fs);
        }
        if current.layout_id != self.scope.layout_id {
            reasons.push(ArtifactStaleReason::Layout);
        }
        if current.participants_binding_hash != self.scope.participants_binding_hash {
            reasons.push(ArtifactStaleReason::Participants);
        }
        if current.role_hash != self.scope.role_hash {
            reasons.push(ArtifactStaleReason::Roles);
        }
        if current.overlay_source_sha256 != self.scope.overlay.source_sha256 {
            reasons.push(ArtifactStaleReason::Overlay);
        }
        if current.post_baseline_hash != self.post.baseline_hash {
            reasons.push(ArtifactStaleReason::PostBaseline);
        }
        if current.profile_sha256 != self.identity.profile_sha256 {
            reasons.push(ArtifactStaleReason::Profile);
        }
        if current.target_sha256 != self.identity.target_sha256 {
            reasons.push(ArtifactStaleReason::Target);
        }
        let recorded_solution = self.identity.solution_hash.as_deref().unwrap_or("");
        if current.solution_hash != recorded_solution {
            reasons.push(ArtifactStaleReason::Solution);
        }
        if current.prior_hash != self.solution.prior_hash {
            reasons.push(ArtifactStaleReason::Prior);
        }
        if current.taps != self.design.taps {
            reasons.push(ArtifactStaleReason::Taps);
        }
        if current.delay_samples != self.design.delay_samples {
            reasons.push(ArtifactStaleReason::Delay);
        }
        let recorded_holdout = self.identity.holdout_report_hash.as_deref().unwrap_or("");
        let current_holdout = current.holdout_report_hash.as_deref().unwrap_or("");
        if current_holdout != recorded_holdout {
            reasons.push(ArtifactStaleReason::Holdout);
        }
        reasons
    }

    /// 是否 stale（任一原因）。
    pub fn is_stale(&self, current: &ArtifactCurrent) -> bool {
        !self.stale_reasons(current).is_empty()
    }

    /// 规范化内容文本（确定性；created_unix/warnings/artifact_hash 自身不参与）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str(ARTIFACT_HASH_DOMAIN);
        text.push('\n');
        text.push_str(&format!("artifact_version={}\n", self.artifact_version));
        text.push_str(&format!("hash_algo={}\n", self.hash_algo));
        text.push_str(&format!("status={}\n", self.status.as_str()));
        text.push_str("status_codes=");
        for (index, code) in self.status_codes.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(code);
        }
        text.push('\n');
        text.push_str(&self.tool.canonical());
        text.push('\n');
        text.push_str(&format!(
            "identity target_schema={} target_id={} target_status={} target_sha256={} baseline_hash={} design_plant_hash={} design_plant_hash_observed={} plant_hash={}\n",
            self.identity.target_schema,
            self.identity.target_id,
            self.identity.target_status,
            self.identity.target_sha256,
            self.identity.baseline_hash,
            self.identity.design_plant_hash,
            self.identity.design_plant_hash_observed.as_deref().unwrap_or("none"),
            self.identity.plant_hash
        ));
        text.push_str(&format!(
            "identity solution_kind={} solution_hash={} holdout_report_hash={}\n",
            self.identity.solution_kind,
            self.identity.solution_hash.as_deref().unwrap_or("none"),
            self.identity
                .holdout_report_hash
                .as_deref()
                .unwrap_or("none")
        ));
        text.push_str(&format!(
            "identity profile_sha256={} measurement_sha256={} calibration_sha256={} tuning_inputs_sha256={} host_graph_sha256={}\n",
            self.identity.profile_sha256,
            self.identity.measurement_sha256.as_deref().unwrap_or("none"),
            self.identity.calibration_sha256.as_deref().unwrap_or("none"),
            self.identity.tuning_inputs_sha256.as_deref().unwrap_or("none"),
            self.identity.host_graph_sha256.as_deref().unwrap_or("none")
        ));
        text.push_str(&format!(
            "scope layout={} fs={} m={} container={} participants_binding_hash={} stereo_strict={} stereo_mask={} dense_to_dac=",
            self.scope.layout_id,
            self.scope.fs,
            self.scope.m,
            self.scope.container_channels,
            self.scope.participants_binding_hash,
            self.scope.stereo_strict,
            self.scope.stereo_mask.join(",")
        ));
        for (index, dac) in self.scope.dense_to_dac.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&format!("{dac}"));
        }
        text.push('\n');
        for participant in &self.scope.participants {
            text.push_str(&format!(
                "participant dense={} role={} dac={}\n",
                participant.dense, participant.role_id, participant.dac
            ));
        }
        text.push_str(&format!(
            "role_hash={}\noverlay layout={} source_sha256={} container={} binding_hash={} unused_outputs=",
            self.scope.role_hash,
            self.scope.overlay.layout_id,
            self.scope.overlay.source_sha256,
            self.scope.overlay.container_channels,
            self.scope.overlay.binding_hash
        ));
        for (index, output) in self.scope.overlay.unused_outputs.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(&format!("{output}"));
        }
        text.push('\n');
        for role in &self.scope.overlay.roles {
            text.push_str(&role.canonical());
        }
        text.push_str(&format!(
            "post present={} baseline_hash={} profile_sha256={} fs={} participants_binding_hash={} fit_inputs_sha256={} tone_binding_hash={} curve_binding_hash={} post_roles_sha256={}\n",
            self.post.present,
            self.post.baseline_hash,
            self.post.profile_sha256,
            self.post.sample_rate_hz,
            self.post.participants_binding_hash,
            self.post.fit_inputs_sha256,
            self.post.tone_binding_hash,
            self.post.curve_binding_hash,
            self.post.post_roles_sha256
        ));
        text.push_str(&post_roles_canonical(&self.post.roles));
        text.push_str(&format!(
            "design profile={} taps={} delay_samples={} delay_q16={} fs={} grid_points={} fit_band={},{} improvement_band={},{} modal_band={},{} target_delay_samples={} common_d0_samples={}\n",
            self.design.profile,
            self.design.taps,
            self.design.delay_samples,
            self.design.delay_q16,
            self.design.fs,
            self.design.grid_points,
            fmt_f64(self.design.fit_band_hz.0),
            fmt_f64(self.design.fit_band_hz.1),
            fmt_f64(self.design.improvement_band_hz.0),
            fmt_f64(self.design.improvement_band_hz.1),
            fmt_f64(self.design.modal_band_hz.0),
            fmt_f64(self.design.modal_band_hz.1),
            self.design.target_delay_samples,
            self.design.common_d0_samples
        ));
        text.push_str(&format!(
            "decay mode={} ratio={} reduction_percent={} meets_t20_gate={}\n",
            self.design.decay.mode,
            fmt_f64(self.design.decay.ratio),
            fmt_f64(self.design.decay.reduction_percent),
            self.design.decay.meets_t20_gate
        ));
        for band in &self.design.decay.bands {
            text.push_str(&format!(
                "decay_band hz={} baseline_t60_s={} target_t60_s={}\n",
                fmt_f64(band.band_hz),
                fmt_f64(band.baseline_t60_s),
                fmt_f64(band.target_t60_s)
            ));
        }
        text.push_str(&format!(
            "tone curve_id={} shape={} reference_gain_db={} binding_hash={} applications={},{},{}\n",
            self.design.tone.curve_id,
            self.design.tone.shape,
            fmt_f64(self.design.tone.reference_gain_db),
            self.design.tone.binding_hash,
            self.design.tone.post_baseline_applications,
            self.design.tone.low_band_residual_applications,
            self.design.tone.target_applications
        ));
        text.push_str(&format!(
            "curve curve_id={} site={} applications={} binding_hash={} target_sha256={} single_application={}\n",
            self.design.curve.curve_id,
            self.design.curve.site,
            self.design.curve.applications,
            self.design.curve.binding_hash,
            self.design.curve.target_sha256.as_deref().unwrap_or("none"),
            self.design.curve.single_application
        ));
        text.push_str(&format!(
            "height_scope present={} excluded_from_low_band_matrix={} binding_hash={}\n",
            self.design.height_scope.present,
            self.design.height_scope.excluded_from_low_band_matrix,
            self.design.height_scope.binding_hash
        ));
        for role in &self.design.height_scope.roles {
            text.push_str(&format!(
                "height_role role={} dac={} band={},{} band_limit={} artificial_delay_ms={} in_low_band_matrix={}\n",
                role.role_id,
                role.dac,
                fmt_f64(role.band_hz.0),
                fmt_f64(role.band_hz.1),
                role.band_limit,
                fmt_f64(role.artificial_delay_ms),
                role.in_low_band_matrix
            ));
        }
        for entry in &self.design.output_scope {
            text.push_str(&format!(
                "output_scope role={} dac={} kind={} band={},{} band_limit={}\n",
                entry.role_id,
                entry.dac,
                entry.kind,
                fmt_f64(entry.band_hz.0),
                fmt_f64(entry.band_hz.1),
                entry.band_limit
            ));
        }
        text.push_str(&format!(
            "target_diagnostics design_positions={} improvement_band_coverage={} required_correction_max_db={} near_zero_points={} validation_positions_reserved={}\n",
            self.design.diagnostics.design_positions,
            fmt_f64(self.design.diagnostics.improvement_band_coverage),
            fmt_f64(self.design.diagnostics.required_correction_max_db),
            self.design.diagnostics.near_zero_points,
            self.design.diagnostics.validation_positions_reserved.join(",")
        ));
        for code in &self.design.target_warnings {
            text.push_str(&format!("target_warning {code}\n"));
        }
        text.push_str(&format!(
            "solution kind={} present={} hash={} coeffs_sha256={} base_solve_hash={} prior_hash={} deliverable={} report_status={} fs={} taps={} delay_samples={} fit_band={},{} improvement_band={},{} grid_points={} design_positions={} predicted_residual_max_db={} predicted_fit_band_residual_db={}\n",
            self.solution.kind,
            self.solution.present,
            self.solution.hash.as_deref().unwrap_or("none"),
            self.solution.coeffs_sha256.as_deref().unwrap_or("none"),
            self.solution.base_solve_hash.as_deref().unwrap_or("none"),
            self.solution.prior_hash.as_deref().unwrap_or("none"),
            self.solution.deliverable,
            self.solution.report_status,
            self.solution.fs,
            self.solution.taps,
            self.solution.delay_samples,
            fmt_f64(self.solution.fit_band_hz.0),
            fmt_f64(self.solution.fit_band_hz.1),
            fmt_f64(self.solution.improvement_band_hz.0),
            fmt_f64(self.solution.improvement_band_hz.1),
            self.solution.grid_points,
            self.solution.design_positions,
            self.solution.predicted_residual_max_db.map(fmt_f64).unwrap_or_else(|| "none".to_string()),
            self.solution.predicted_fit_band_residual_db.map(fmt_f64).unwrap_or_else(|| "none".to_string())
        ));
        if let Some(selection) = &self.solution.selection {
            text.push_str(&format!(
                "selection method={} cv_valid={} cv_reason={} whitelist={} lambda={} lambda_used={} applicability={} fallback_used={} headroom_fallback_attempts={}\n",
                selection.method,
                selection.cv_valid,
                selection.cv_reason.unwrap_or("none"),
                selection.selected_whitelist,
                fmt_f64(selection.selected_lambda),
                fmt_f64(selection.selected_lambda_used),
                selection.applicability,
                selection.fallback_used,
                selection.headroom_fallback_attempts
            ));
        }
        if let Some(headroom) = &self.solution.headroom {
            text.push_str(&format!(
                "headroom passed={} correction_cap_db={} diagonal_cap_db={} tolerance_db={} max_correction_gain_db={} max_diagonal_gain_db={} worst_excess_db={} violated_paths={}\n",
                headroom.passed,
                fmt_f64(headroom.correction_cap_db),
                fmt_f64(headroom.diagonal_cap_db),
                fmt_f64(headroom.tolerance_db),
                fmt_f64(headroom.max_correction_gain_db),
                fmt_f64(headroom.max_diagonal_gain_db),
                fmt_f64(headroom.worst_excess_db),
                headroom.violated_paths.join(",")
            ));
        }
        for code in &self.solution.warning_codes {
            text.push_str(&format!("solution_warning {code}\n"));
        }
        text.push_str(&format!(
            "holdout present={} schema={} report_hash={} verdict={} verdict_code={} isolation_gate={}/{} prediction_gate={}/{} t20_gate={}/{} target_sha256={} baseline_hash={} design_plant_hash={} solution_hash={} holdout_labels={} tuning_inputs_sha256={} declared_improvement_gate_db={} declared_t20_reduction_ratio={}\n",
            self.holdout.present,
            self.holdout.schema,
            self.holdout.report_hash,
            self.holdout.verdict,
            self.holdout.verdict_code,
            self.holdout.isolation_gate,
            self.holdout.isolation_gate_code,
            self.holdout.prediction_gate,
            self.holdout.prediction_gate_code,
            self.holdout.t20_gate,
            self.holdout.t20_gate_code,
            self.holdout.target_sha256,
            self.holdout.baseline_hash,
            self.holdout.design_plant_hash,
            self.holdout.solution_hash.as_deref().unwrap_or("none"),
            self.holdout.holdout_labels.join(","),
            self.holdout.tuning_inputs_sha256,
            fmt_f64(self.holdout.declared_improvement_gate_db),
            fmt_f64(self.holdout.declared_t20_reduction_ratio)
        ));
        for path in &self.paths {
            text.push_str(&format!(
                "path out={} in={} out_role={} in_role={} out_dac={} in_dac={} file={} format={} fs={} taps={} delay_samples={} delay_q16={} diag_baseline={} zero={} band={},{} bytes={} sha256={} f64_sha256={} peak_abs={}\n",
                path.out_dense,
                path.in_dense,
                path.out_role,
                path.in_role,
                path.out_dac,
                path.in_dac,
                path.file,
                path.format,
                path.fs,
                path.taps,
                path.delay_samples,
                path.delay_q16,
                path.diag_baseline,
                path.zero,
                fmt_f64(path.band_hz.0),
                fmt_f64(path.band_hz.1),
                path.bytes,
                path.sha256,
                path.f64_sha256,
                fmt_f64(path.peak_abs)
            ));
        }
        text.push_str(&format!(
            "quantization sample_format={} bits={} coeffs_f64_sha256={} coeffs_f32_sha256={} max_abs_f32_error={} wav_aggregate_sha256={}\n",
            self.quantization.sample_format,
            self.quantization.bits_per_sample,
            self.quantization.coeffs_f64_sha256,
            self.quantization.coeffs_f32_sha256,
            fmt_f64(self.quantization.max_abs_f32_error),
            self.quantization.wav_aggregate_sha256
        ));
        text.push_str(&format!(
            "honesty applied={} measured={} post_apply_measurement={} holdout_used_for_tuning={} can_substitute_real_remeasurement={} requires_post_apply_remeasurement={} compiled={} claims={}\n",
            self.honesty.applied,
            self.honesty.measured,
            self.honesty.post_apply_measurement,
            self.honesty.holdout_used_for_tuning,
            self.honesty.can_substitute_real_remeasurement,
            self.honesty.requires_post_apply_remeasurement,
            self.honesty.compiled,
            self.honesty.claims
        ));
        for warning in &self.warnings {
            text.push_str(&format!("warning {warning}\n"));
        }
        text
    }

    /// manifest JSON（确定性键序；created_unix 只在此出现且不参与 artifact hash）。
    pub fn to_manifest_json(&self) -> String {
        let mut json = String::new();
        json.push('{');
        json.push_str(&format!("\"schema\":{},", json_str(self.schema)));
        json.push_str(&format!(
            "\"artifact_version\":{},\"hash_algo\":{},\"artifact_hash\":{},",
            self.artifact_version,
            json_str(self.hash_algo),
            json_str(&self.artifact_hash)
        ));
        match self.created_unix {
            Some(value) => json.push_str(&format!("\"created_unix\":{value},")),
            None => json.push_str("\"created_unix\":null,"),
        }
        json.push_str(&format!(
            "\"status\":{},\"status_codes\":{},",
            json_str(self.status.as_str()),
            json_str_array(&self.status_codes)
        ));
        json.push_str(&format!(
            "\"tool\":{{\"name\":{},\"version\":{}}},",
            json_str(&self.tool.name),
            json_str(&self.tool.version)
        ));
        json.push_str("\"identity\":{");
        json.push_str(&format!(
            "\"target_schema\":{},",
            json_str(self.identity.target_schema)
        ));
        json.push_str(&format!(
            "\"target_id\":{},",
            json_str(&self.identity.target_id)
        ));
        json.push_str(&format!(
            "\"target_status\":{},",
            json_str(self.identity.target_status)
        ));
        json.push_str(&format!(
            "\"target_sha256\":{},\"baseline_hash\":{},\"design_plant_hash\":{},\"design_plant_hash_observed\":{},\"plant_hash\":{},",
            json_str(&self.identity.target_sha256),
            json_str(&self.identity.baseline_hash),
            json_str(&self.identity.design_plant_hash),
            json_opt_str(&self.identity.design_plant_hash_observed),
            json_str(&self.identity.plant_hash)
        ));
        json.push_str(&format!(
            "\"solution_hash\":{},\"solution_kind\":{},\"holdout_report_hash\":{},",
            json_opt_str(&self.identity.solution_hash),
            json_str(self.identity.solution_kind),
            json_opt_str(&self.identity.holdout_report_hash)
        ));
        json.push_str(&format!(
            "\"profile_sha256\":{},\"measurement_sha256\":{},\"calibration_sha256\":{},\"tuning_inputs_sha256\":{},\"host_graph_sha256\":{}}},",
            json_str(&self.identity.profile_sha256),
            json_opt_str(&self.identity.measurement_sha256),
            json_opt_str(&self.identity.calibration_sha256),
            json_opt_str(&self.identity.tuning_inputs_sha256),
            json_opt_str(&self.identity.host_graph_sha256)
        ));
        json.push_str("\"scope\":{");
        json.push_str(&format!(
            "\"layout_id\":{},\"fs\":{},\"m\":{},\"container_channels\":{},",
            json_str(&self.scope.layout_id),
            self.scope.fs,
            self.scope.m,
            self.scope.container_channels
        ));
        json.push_str("\"participants\":[");
        for (index, participant) in self.scope.participants.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"dense\":{},\"role_id\":{},\"dac\":{}}}",
                participant.dense,
                json_str(&participant.role_id),
                participant.dac
            ));
        }
        json.push_str("],\"dense_to_dac\":[");
        for (index, dac) in self.scope.dense_to_dac.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!("{dac}"));
        }
        json.push_str("],");
        json.push_str(&format!(
            "\"participants_binding_hash\":{},\"stereo_strict\":{},\"stereo_mask\":{},\"role_hash\":{},",
            json_str(&self.scope.participants_binding_hash),
            json_bool(self.scope.stereo_strict),
            json_str_array(&self.scope.stereo_mask),
            json_str(&self.scope.role_hash)
        ));
        json.push_str("\"overlay\":{");
        json.push_str(&format!(
            "\"layout_id\":{},\"source_sha256\":{},\"container_channels\":{},\"roles\":[",
            json_str(&self.scope.overlay.layout_id),
            json_str(&self.scope.overlay.source_sha256),
            self.scope.overlay.container_channels
        ));
        for (index, role) in self.scope.overlay.roles.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"role_id\":{},\"outputs\":[",
                json_str(&role.role_id)
            ));
            for (output_index, output) in role.outputs.iter().enumerate() {
                if output_index > 0 {
                    json.push(',');
                }
                json.push_str(&format!("{output}"));
            }
            json.push_str("]}");
        }
        json.push_str("],\"unused_outputs\":[");
        for (index, output) in self.scope.overlay.unused_outputs.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!("{output}"));
        }
        json.push_str("],");
        json.push_str(&format!(
            "\"binding_hash\":{}",
            json_str(&self.scope.overlay.binding_hash)
        ));
        json.push_str("}},");
        json.push_str("\"post\":{");
        json.push_str(&format!(
            "\"present\":{},\"baseline_hash\":{},\"profile_sha256\":{},\"sample_rate_hz\":{},\"participants_binding_hash\":{},\"fit_inputs_sha256\":{},\"tone_binding_hash\":{},\"curve_binding_hash\":{},\"post_roles_sha256\":{},",
            json_bool(self.post.present),
            json_str(&self.post.baseline_hash),
            json_str(&self.post.profile_sha256),
            self.post.sample_rate_hz,
            json_str(&self.post.participants_binding_hash),
            json_str(&self.post.fit_inputs_sha256),
            json_str(&self.post.tone_binding_hash),
            json_str(&self.post.curve_binding_hash),
            json_str(&self.post.post_roles_sha256)
        ));
        json.push_str("\"roles\":[");
        for (index, role) in self.post.roles.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"role_id\":{},\"dac\":{},\"delay_samples\":{},\"polarity\":{},\"gain_db\":{},\"band_limit\":{},\"artificial_delay_ms\":{},\"modal_peq\":{}}}",
                json_str(&role.role_id),
                role.dac,
                role.delay_samples,
                role.polarity,
                json_num(role.gain_db),
                json_str(role.band_limit),
                json_num(role.artificial_delay_ms),
                json_bool(role.modal_peq)
            ));
        }
        json.push_str("]},");
        json.push_str("\"design\":{");
        json.push_str(&format!(
            "\"profile\":{},\"taps\":{},\"delay_samples\":{},\"delay_q16\":{},\"fs\":{},\"grid_points\":{},\"fit_band_hz\":{},\"improvement_band_hz\":{},\"modal_band_hz\":{},\"target_delay_samples\":{},\"common_d0_samples\":{},",
            json_str(self.design.profile),
            self.design.taps,
            self.design.delay_samples,
            self.design.delay_q16,
            self.design.fs,
            self.design.grid_points,
            json_pair(self.design.fit_band_hz),
            json_pair(self.design.improvement_band_hz),
            json_pair(self.design.modal_band_hz),
            self.design.target_delay_samples,
            self.design.common_d0_samples
        ));
        json.push_str(&format!(
            "\"decay\":{{\"mode\":{},\"ratio\":{},\"reduction_percent\":{},\"meets_t20_gate\":{},\"bands\":[",
            json_str(self.design.decay.mode),
            json_num(self.design.decay.ratio),
            json_num(self.design.decay.reduction_percent),
            json_bool(self.design.decay.meets_t20_gate)
        ));
        for (index, band) in self.design.decay.bands.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"band_hz\":{},\"baseline_t60_s\":{},\"target_t60_s\":{}}}",
                json_num(band.band_hz),
                json_num(band.baseline_t60_s),
                json_num(band.target_t60_s)
            ));
        }
        json.push_str("]},");
        json.push_str(&format!(
            "\"tone\":{{\"curve_id\":{},\"shape\":{},\"reference_gain_db\":{},\"binding_hash\":{},\"post_baseline_applications\":{},\"low_band_residual_applications\":{},\"target_applications\":{}}},",
            json_str(&self.design.tone.curve_id),
            json_str(self.design.tone.shape),
            json_num(self.design.tone.reference_gain_db),
            json_str(&self.design.tone.binding_hash),
            self.design.tone.post_baseline_applications,
            self.design.tone.low_band_residual_applications,
            self.design.tone.target_applications
        ));
        json.push_str(&format!(
            "\"curve\":{{\"curve_id\":{},\"site\":{},\"applications\":{},\"binding_hash\":{},\"target_sha256\":{},\"single_application\":{}}},",
            json_str(&self.design.curve.curve_id),
            json_str(self.design.curve.site),
            self.design.curve.applications,
            json_str(&self.design.curve.binding_hash),
            json_opt_str(&self.design.curve.target_sha256),
            json_bool(self.design.curve.single_application)
        ));
        json.push_str(&format!(
            "\"height_scope\":{{\"present\":{},\"excluded_from_low_band_matrix\":{},\"binding_hash\":{},\"roles\":[",
            json_bool(self.design.height_scope.present),
            json_bool(self.design.height_scope.excluded_from_low_band_matrix),
            json_str(&self.design.height_scope.binding_hash)
        ));
        for (index, role) in self.design.height_scope.roles.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"role_id\":{},\"dac\":{},\"band_hz\":{},\"band_limit\":{},\"artificial_delay_ms\":{},\"in_low_band_matrix\":{}}}",
                json_str(&role.role_id),
                role.dac,
                json_pair(role.band_hz),
                json_str(role.band_limit),
                json_num(role.artificial_delay_ms),
                json_bool(role.in_low_band_matrix)
            ));
        }
        json.push_str("]},");
        json.push_str("\"output_scope\":[");
        for (index, entry) in self.design.output_scope.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"role_id\":{},\"dac\":{},\"kind\":{},\"band_hz\":{},\"band_limit\":{}}}",
                json_str(&entry.role_id),
                entry.dac,
                json_str(entry.kind),
                json_pair(entry.band_hz),
                json_str(entry.band_limit)
            ));
        }
        json.push_str("],");
        json.push_str(&format!(
            "\"diagnostics\":{{\"design_positions\":{},\"improvement_band_coverage\":{},\"required_correction_max_db\":{},\"near_zero_points\":{},\"validation_positions_reserved\":{}}},",
            self.design.diagnostics.design_positions,
            json_num(self.design.diagnostics.improvement_band_coverage),
            json_num(self.design.diagnostics.required_correction_max_db),
            self.design.diagnostics.near_zero_points,
            json_str_array(&self.design.diagnostics.validation_positions_reserved)
        ));
        json.push_str(&format!(
            "\"target_warnings\":{}}},",
            json_str_array(&self.design.target_warnings)
        ));
        json.push_str("\"solution\":{");
        json.push_str(&format!(
            "\"kind\":{},\"present\":{},\"hash\":{},\"coeffs_sha256\":{},\"base_solve_hash\":{},\"prior_hash\":{},\"deliverable\":{},\"report_status\":{},\"fs\":{},\"taps\":{},\"delay_samples\":{},\"fit_band_hz\":{},\"improvement_band_hz\":{},\"grid_points\":{},\"design_positions\":{},\"predicted_residual_max_db\":{},\"predicted_fit_band_residual_db\":{},",
            json_str(self.solution.kind),
            json_bool(self.solution.present),
            json_opt_str(&self.solution.hash),
            json_opt_str(&self.solution.coeffs_sha256),
            json_opt_str(&self.solution.base_solve_hash),
            json_opt_str(&self.solution.prior_hash),
            json_bool(self.solution.deliverable),
            json_str(self.solution.report_status),
            self.solution.fs,
            self.solution.taps,
            self.solution.delay_samples,
            json_pair(self.solution.fit_band_hz),
            json_pair(self.solution.improvement_band_hz),
            self.solution.grid_points,
            self.solution.design_positions,
            json_opt_num(self.solution.predicted_residual_max_db),
            json_opt_num(self.solution.predicted_fit_band_residual_db)
        ));
        match &self.solution.selection {
            None => json.push_str("\"selection\":null,"),
            Some(selection) => json.push_str(&format!(
                "\"selection\":{{\"method\":{},\"cv_valid\":{},\"cv_reason\":{},\"selected_whitelist\":{},\"selected_lambda\":{},\"selected_lambda_used\":{},\"applicability\":{},\"fallback_used\":{},\"headroom_fallback_attempts\":{}}},",
                json_str(selection.method),
                json_bool(selection.cv_valid),
                selection.cv_reason.map(json_str).unwrap_or_else(|| "null".to_string()),
                json_str(selection.selected_whitelist),
                json_num(selection.selected_lambda),
                json_num(selection.selected_lambda_used),
                json_str(selection.applicability),
                json_bool(selection.fallback_used),
                selection.headroom_fallback_attempts
            )),
        }
        match &self.solution.headroom {
            None => json.push_str("\"headroom\":null,"),
            Some(headroom) => json.push_str(&format!(
                "\"headroom\":{{\"passed\":{},\"correction_cap_db\":{},\"diagonal_cap_db\":{},\"tolerance_db\":{},\"max_correction_gain_db\":{},\"max_diagonal_gain_db\":{},\"worst_excess_db\":{},\"violated_paths\":{}}},",
                json_bool(headroom.passed),
                json_num(headroom.correction_cap_db),
                json_num(headroom.diagonal_cap_db),
                json_num(headroom.tolerance_db),
                json_num(headroom.max_correction_gain_db),
                json_num(headroom.max_diagonal_gain_db),
                json_num(headroom.worst_excess_db),
                json_str_array(&headroom.violated_paths)
            )),
        }
        json.push_str(&format!(
            "\"warning_codes\":{}}},",
            json_str_array(&self.solution.warning_codes)
        ));
        json.push_str("\"holdout\":{");
        json.push_str(&format!(
            "\"present\":{},\"schema\":{},\"report_hash\":{},\"verdict\":{},\"verdict_code\":{},\"verdict_reasons\":{},\"isolation_gate\":{},\"isolation_gate_code\":{},\"prediction_gate\":{},\"prediction_gate_code\":{},\"t20_gate\":{},\"t20_gate_code\":{},\"target_sha256\":{},\"baseline_hash\":{},\"design_plant_hash\":{},\"solution_hash\":{},\"holdout_labels\":{},\"tuning_inputs_sha256\":{},\"declared_improvement_gate_db\":{},\"declared_t20_reduction_ratio\":{}}},",
            json_bool(self.holdout.present),
            json_str(self.holdout.schema),
            json_str(&self.holdout.report_hash),
            json_str(self.holdout.verdict),
            json_str(self.holdout.verdict_code),
            json_str_array(&self.holdout.verdict_reasons),
            json_str(self.holdout.isolation_gate),
            json_str(self.holdout.isolation_gate_code),
            json_str(self.holdout.prediction_gate),
            json_str(self.holdout.prediction_gate_code),
            json_str(self.holdout.t20_gate),
            json_str(self.holdout.t20_gate_code),
            json_str(&self.holdout.target_sha256),
            json_str(&self.holdout.baseline_hash),
            json_str(&self.holdout.design_plant_hash),
            json_opt_str(&self.holdout.solution_hash),
            json_str_array(&self.holdout.holdout_labels),
            json_str(&self.holdout.tuning_inputs_sha256),
            json_num(self.holdout.declared_improvement_gate_db),
            json_num(self.holdout.declared_t20_reduction_ratio)
        ));
        json.push_str("\"paths\":[");
        for (index, path) in self.paths.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"out_dense\":{},\"in_dense\":{},\"out_role\":{},\"in_role\":{},\"out_dac\":{},\"in_dac\":{},\"file\":{},\"format\":{},\"fs\":{},\"taps\":{},\"delay_samples\":{},\"delay_q16\":{},\"diag_baseline\":{},\"zero\":{},\"band_hz\":{},\"bytes\":{},\"sha256\":{},\"f64_sha256\":{},\"peak_abs\":{}}}",
                path.out_dense,
                path.in_dense,
                json_str(&path.out_role),
                json_str(&path.in_role),
                path.out_dac,
                path.in_dac,
                json_str(&path.file),
                json_str(path.format),
                path.fs,
                path.taps,
                path.delay_samples,
                path.delay_q16,
                json_bool(path.diag_baseline),
                json_bool(path.zero),
                json_pair(path.band_hz),
                path.bytes,
                json_str(&path.sha256),
                json_str(&path.f64_sha256),
                json_num(path.peak_abs)
            ));
        }
        json.push_str("],");
        json.push_str(&format!(
            "\"quantization\":{{\"sample_format\":{},\"bits_per_sample\":{},\"coeffs_f64_sha256\":{},\"coeffs_f32_sha256\":{},\"max_abs_f32_error\":{},\"wav_aggregate_sha256\":{}}},",
            json_str(self.quantization.sample_format),
            self.quantization.bits_per_sample,
            json_str(&self.quantization.coeffs_f64_sha256),
            json_str(&self.quantization.coeffs_f32_sha256),
            json_num(self.quantization.max_abs_f32_error),
            json_str(&self.quantization.wav_aggregate_sha256)
        ));
        json.push_str(&format!(
            "\"honesty\":{{\"applied\":{},\"measured\":{},\"post_apply_measurement\":{},\"holdout_used_for_tuning\":{},\"can_substitute_real_remeasurement\":{},\"requires_post_apply_remeasurement\":{},\"compiled\":{},\"claims\":{}}},",
            json_bool(self.honesty.applied),
            json_bool(self.honesty.measured),
            json_str(self.honesty.post_apply_measurement),
            json_bool(self.honesty.holdout_used_for_tuning),
            json_bool(self.honesty.can_substitute_real_remeasurement),
            json_bool(self.honesty.requires_post_apply_remeasurement),
            json_bool(self.honesty.compiled),
            json_str(self.honesty.claims)
        ));
        json.push_str(&format!("\"warnings\":{}", json_str_array(&self.warnings)));
        json.push('}');
        json
    }

    /// 自检（WAV/manifest/hash/role/P/延迟/量化一致性；篡改/缺文件可检出）。
    pub fn verify(&self) -> Result<(), ArtifactError> {
        if self.schema != ARTIFACT_SCHEMA {
            return Err(err(
                ArtifactErrorKind::Schema,
                "schema",
                format!("schema {} != {ARTIFACT_SCHEMA}", self.schema),
            ));
        }
        if self.artifact_version != ARTIFACT_VERSION {
            return Err(err(
                ArtifactErrorKind::Schema,
                "artifact_version",
                format!(
                    "artifact_version {} != {ARTIFACT_VERSION}",
                    self.artifact_version
                ),
            ));
        }
        if self.hash_algo != ARTIFACT_HASH_ALGO {
            return Err(err(
                ArtifactErrorKind::Schema,
                "hash_algo",
                format!("hash_algo {} != {ARTIFACT_HASH_ALGO}", self.hash_algo),
            ));
        }
        if self.status != status_from_codes(&self.status_codes) {
            return Err(err(
                ArtifactErrorKind::Status,
                "status",
                format!(
                    "status {} inconsistent with codes {:?}",
                    self.status.as_str(),
                    self.status_codes
                ),
            ));
        }
        let mut unique_codes: Vec<&str> = Vec::new();
        for code in &self.status_codes {
            if unique_codes.contains(code) {
                return Err(err(
                    ArtifactErrorKind::Status,
                    "status_codes",
                    format!("duplicate status code {code}"),
                ));
            }
            if !is_known_status_code(code) {
                return Err(err(
                    ArtifactErrorKind::Status,
                    "status_codes",
                    format!("unknown status code {code}"),
                ));
            }
            unique_codes.push(code);
        }
        if self.files.is_empty() {
            return Err(err(
                ArtifactErrorKind::Hash,
                "files",
                "artifact has no manifest file",
            ));
        }
        if self.files[0].rel_path != ARTIFACT_MANIFEST_FILE {
            return Err(err(
                ArtifactErrorKind::Hash,
                "files[0]",
                "first file must be manifest.json",
            ));
        }
        if self.files.len() != 1 + self.paths.len() {
            return Err(err(
                ArtifactErrorKind::Hash,
                "files",
                format!(
                    "file count {} != 1 manifest + {} paths",
                    self.files.len(),
                    self.paths.len()
                ),
            ));
        }
        let mut seen_paths: Vec<&str> = Vec::new();
        for file in &self.files {
            validate_rel_path(&file.rel_path)?;
            if seen_paths.contains(&file.rel_path.as_str()) {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "files",
                    format!("duplicate file path {}", file.rel_path),
                ));
            }
            seen_paths.push(&file.rel_path);
        }
        let manifest = self.to_manifest_json();
        if self.files[0].bytes != manifest.as_bytes() {
            return Err(err(
                ArtifactErrorKind::Hash,
                "manifest.json",
                "manifest bytes differ from the in-memory document",
            ));
        }
        if sha256_hex(&self.files[0].bytes) != self.manifest_sha256 {
            return Err(err(
                ArtifactErrorKind::Hash,
                "manifest_sha256",
                "manifest sha256 mismatch",
            ));
        }
        if sha256_hex(self.canonical().as_bytes()) != self.artifact_hash {
            return Err(err(
                ArtifactErrorKind::Hash,
                "artifact_hash",
                "artifact content sha256 mismatch",
            ));
        }
        let m = self.scope.m;
        if m == 0
            || m > MAX_LOW_BAND_PARTICIPANTS
            || m != self.scope.participants.len()
            || m != self.scope.dense_to_dac.len()
        {
            return Err(err(
                ArtifactErrorKind::Participants,
                "scope.m",
                "scope M/participants/dense_to_dac lengths inconsistent",
            ));
        }
        for (index, participant) in self.scope.participants.iter().enumerate() {
            if participant.dense as usize != index
                || self.scope.dense_to_dac[index] != participant.dac
            {
                return Err(err(
                    ArtifactErrorKind::Participants,
                    format!("scope.participants[{index}]"),
                    "participant dense/dac ordering inconsistent",
                ));
            }
        }
        let overlay_canonical = stored_overlay_canonical(&self.scope.overlay);
        if sha256_hex(overlay_canonical.as_bytes()) != self.scope.overlay.binding_hash {
            return Err(err(
                ArtifactErrorKind::Hash,
                "scope.overlay.binding_hash",
                "overlay binding hash mismatch",
            ));
        }
        let role_hash = role_binding_hash(
            &self.scope.layout_id,
            self.scope.fs,
            self.scope.container_channels,
            &self.scope.participants,
            self.scope.stereo_strict,
            &self.scope.stereo_mask,
            &self.scope.overlay,
        );
        if role_hash != self.scope.role_hash {
            return Err(err(
                ArtifactErrorKind::Hash,
                "scope.role_hash",
                "role hash mismatch",
            ));
        }
        if self.scope.layout_id != self.identity.target_schema {
            // no-op: layouts and schema are independent namespaces
        }
        verify_sha256("identity.target_sha256", &self.identity.target_sha256)?;
        verify_sha256("identity.baseline_hash", &self.identity.baseline_hash)?;
        verify_sha256(
            "identity.design_plant_hash",
            &self.identity.design_plant_hash,
        )?;
        verify_sha256("identity.plant_hash", &self.identity.plant_hash)?;
        if let Some(value) = &self.identity.design_plant_hash_observed {
            verify_sha256("identity.design_plant_hash_observed", value)?;
        }
        verify_sha256("identity.profile_sha256", &self.identity.profile_sha256)?;
        if let Some(value) = &self.identity.solution_hash {
            verify_sha256("identity.solution_hash", value)?;
        }
        if let Some(value) = &self.identity.measurement_sha256 {
            verify_sha256("identity.measurement_sha256", value)?;
        }
        if let Some(value) = &self.identity.calibration_sha256 {
            verify_sha256("identity.calibration_sha256", value)?;
        }
        if let Some(value) = &self.identity.tuning_inputs_sha256 {
            verify_sha256("identity.tuning_inputs_sha256", value)?;
        }
        if let Some(value) = &self.identity.host_graph_sha256 {
            verify_sha256("identity.host_graph_sha256", value)?;
        }
        if self.post.present {
            verify_sha256("post.baseline_hash", &self.post.baseline_hash)?;
            if self.post.baseline_hash != self.identity.baseline_hash {
                return Err(err(
                    ArtifactErrorKind::Post,
                    "post.baseline_hash",
                    "P baseline hash differs from identity baseline hash",
                ));
            }
            if self.post.roles.is_empty() {
                return Err(err(
                    ArtifactErrorKind::Post,
                    "post.roles",
                    "present P binding has no role records",
                ));
            }
            let post_sha = sha256_hex(post_roles_canonical(&self.post.roles).as_bytes());
            if post_sha != self.post.post_roles_sha256 {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "post.post_roles_sha256",
                    "P roles sha256 mismatch",
                ));
            }
        }
        if self.holdout.present {
            verify_sha256("holdout.report_hash", &self.holdout.report_hash)?;
            if self.identity.holdout_report_hash.as_deref()
                != Some(self.holdout.report_hash.as_str())
            {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "holdout.report_hash",
                    "holdout report hash differs from identity",
                ));
            }
        } else if self.identity.holdout_report_hash.is_some() {
            return Err(err(
                ArtifactErrorKind::Hash,
                "holdout.present",
                "identity records a holdout report hash but holdout.present=false",
            ));
        }
        if self.solution.present {
            if self.paths.len() != m * m {
                return Err(err(
                    ArtifactErrorKind::Taps,
                    "paths",
                    format!("{} paths != M*M {}", self.paths.len(), m * m),
                ));
            }
            if self.solution.taps != self.design.taps
                || self.solution.delay_samples != self.design.delay_samples
                || self.solution.fs != self.design.fs
            {
                return Err(err(
                    ArtifactErrorKind::Solution,
                    "solution",
                    "solution fs/taps/D differ from design binding",
                ));
            }
            if self.solution.hash != self.identity.solution_hash {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "solution.hash",
                    "solution hash differs from identity",
                ));
            }
            if let (Some(hash), Some(coeffs_sha)) =
                (&self.solution.hash, &self.solution.coeffs_sha256)
            {
                verify_sha256("solution.hash", hash)?;
                verify_sha256("solution.coeffs_sha256", coeffs_sha)?;
                if self.quantization.coeffs_f64_sha256 != *coeffs_sha {
                    return Err(err(
                        ArtifactErrorKind::Hash,
                        "quantization.coeffs_f64_sha256",
                        "quantization f64 sha256 differs from solution coeffs_sha256",
                    ));
                }
            } else {
                return Err(err(
                    ArtifactErrorKind::Solution,
                    "solution.hash",
                    "present solution must record hash and coeffs_sha256",
                ));
            }
            if self.quantization.sample_format != ARTIFACT_WAV_FORMAT
                || self.quantization.bits_per_sample != ARTIFACT_WAV_BITS
            {
                return Err(err(
                    ArtifactErrorKind::Wav,
                    "quantization.sample_format",
                    "quantization format is not wav_f32_mono/32-bit",
                ));
            }
            if !self.quantization.max_abs_f32_error.is_finite()
                || self.quantization.max_abs_f32_error < 0.0
            {
                return Err(err(
                    ArtifactErrorKind::Numeric,
                    "quantization.max_abs_f32_error",
                    "f32 quantization error must be finite and >= 0",
                ));
            }
        } else if !self.paths.is_empty() {
            return Err(err(
                ArtifactErrorKind::Solution,
                "solution.present",
                "paths exist without a solution",
            ));
        }
        let mut seen_cells: Vec<(u16, u16)> = Vec::new();
        let mut all_f32: Vec<f32> = Vec::new();
        let mut aggregate = String::new();
        for (index, path) in self.paths.iter().enumerate() {
            if path.out_dense as usize >= m || path.in_dense as usize >= m {
                return Err(err(
                    ArtifactErrorKind::Participants,
                    format!("paths[{index}]"),
                    "dense index out of range",
                ));
            }
            let cell = (path.out_dense, path.in_dense);
            if seen_cells.contains(&cell) {
                return Err(err(
                    ArtifactErrorKind::Taps,
                    format!("paths[{index}]"),
                    "duplicate (out,in) cell",
                ));
            }
            seen_cells.push(cell);
            if path.fs != self.design.fs || path.taps != self.design.taps {
                return Err(err(
                    ArtifactErrorKind::Taps,
                    format!("paths[{index}]"),
                    "path fs/taps differ from artifact design",
                ));
            }
            let diag = path.out_dense == path.in_dense;
            if path.diag_baseline != diag {
                return Err(err(
                    ArtifactErrorKind::Delay,
                    format!("paths[{index}].diag_baseline"),
                    "diag_baseline flag inconsistent with (out,in)",
                ));
            }
            let expected_delay = if diag { self.design.delay_samples } else { 0 };
            if path.delay_samples != expected_delay || path.delay_q16 != 0 {
                return Err(err(
                    ArtifactErrorKind::Delay,
                    format!("paths[{index}].delay_samples"),
                    "path delay must be D on the diagonal and 0 off-diagonal (q16=0)",
                ));
            }
            if !bands_equal(path.band_hz, self.design.modal_band_hz) {
                return Err(err(
                    ArtifactErrorKind::Taps,
                    format!("paths[{index}].band_hz"),
                    "path band differs from artifact modal band",
                ));
            }
            verify_sha256(&format!("paths[{index}].sha256"), &path.sha256)?;
            verify_sha256(&format!("paths[{index}].f64_sha256"), &path.f64_sha256)?;
            let file = self.file(&path.file).ok_or_else(|| {
                err(
                    ArtifactErrorKind::Hash,
                    format!("paths[{index}].file"),
                    format!("WAV file {} not present in artifact files", path.file),
                )
            })?;
            if file.bytes.len() as u64 != path.bytes {
                return Err(err(
                    ArtifactErrorKind::Wav,
                    format!("paths[{index}].bytes"),
                    "recorded byte length differs from file",
                ));
            }
            if sha256_hex(&file.bytes) != path.sha256 {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    format!("paths[{index}].sha256"),
                    format!("WAV {} sha256 mismatch", path.file),
                ));
            }
            let wav = decode_wav_f32_mono(&file.bytes)?;
            if wav.sample_rate_hz != path.fs {
                return Err(err(
                    ArtifactErrorKind::Wav,
                    format!("paths[{index}].fs"),
                    "WAV fs differs from manifest",
                ));
            }
            if wav.samples.len() != path.taps as usize {
                return Err(err(
                    ArtifactErrorKind::Taps,
                    format!("paths[{index}].taps"),
                    "WAV sample count differs from manifest taps",
                ));
            }
            let zero = wav.samples.iter().all(|value| *value == 0.0);
            if zero != path.zero {
                return Err(err(
                    ArtifactErrorKind::Wav,
                    format!("paths[{index}].zero"),
                    "WAV zero flag mismatch",
                ));
            }
            let mut peak = 0.0f64;
            for sample in &wav.samples {
                peak = peak.max(f64::from(sample.abs()));
            }
            if (peak - path.peak_abs).abs() > 1.0e-12 {
                return Err(err(
                    ArtifactErrorKind::Wav,
                    format!("paths[{index}].peak_abs"),
                    "WAV peak mismatch",
                ));
            }
            aggregate.push_str(&path.sha256);
            aggregate.push('\n');
            all_f32.extend_from_slice(&wav.samples);
        }
        if self.solution.present {
            if self.paths.len() != m * m {
                return Err(err(
                    ArtifactErrorKind::Taps,
                    "paths",
                    "missing path WAV records",
                ));
            }
            if f32_bits_sha256(&all_f32) != self.quantization.coeffs_f32_sha256 {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "quantization.coeffs_f32_sha256",
                    "f32 coefficients sha256 mismatch",
                ));
            }
            if sha256_hex(aggregate.as_bytes()) != self.quantization.wav_aggregate_sha256 {
                return Err(err(
                    ArtifactErrorKind::Hash,
                    "quantization.wav_aggregate_sha256",
                    "WAV aggregate sha256 mismatch",
                ));
            }
        }
        if self.honesty.applied
            || self.honesty.measured
            || self.honesty.compiled
            || self.honesty.holdout_used_for_tuning
            || self.honesty.can_substitute_real_remeasurement
            || !self.honesty.requires_post_apply_remeasurement
            || self.honesty.post_apply_measurement != "not_run"
            || self.honesty.claims != ARTIFACT_CLAIM
        {
            return Err(err(
                ArtifactErrorKind::Status,
                "honesty",
                "honesty fields must record design-only/not-applied/not-measured/requires-remeasurement",
            ));
        }
        Ok(())
    }

    /// 写入目录（manifest + coeff WAV；返回实际路径）。
    ///
    /// 生命周期（只增不改/current 选择/原子切换）归 T60/T66；本函数只做确定性落盘并回读校验。
    pub fn write_to_dir(
        &self,
        dir: impl AsRef<Path>,
    ) -> Result<ArtifactWritten, ArtifactWriteError> {
        let root = dir.as_ref();
        fs::create_dir_all(root).map_err(|error| write_error(root, &error))?;
        let mut written = Vec::with_capacity(self.files.len());
        let mut total_bytes = 0u64;
        for file in &self.files {
            let target = root.join(&file.rel_path);
            validate_rel_path(&file.rel_path)
                .map_err(|error| invalid_path_error(&target, error.detail().to_string()))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| write_error(parent, &error))?;
            }
            fs::write(&target, &file.bytes).map_err(|error| write_error(&target, &error))?;
            let read_back = fs::read(&target).map_err(|error| write_error(&target, &error))?;
            if read_back != file.bytes {
                return Err(ArtifactWriteError {
                    path: target,
                    kind: io::ErrorKind::InvalidData,
                    detail: "read-back bytes differ from written bytes".to_string(),
                });
            }
            total_bytes += file.bytes.len() as u64;
            written.push(target);
        }
        let manifest_path = root.join(ARTIFACT_MANIFEST_FILE);
        Ok(ArtifactWritten {
            dir: root.to_path_buf(),
            files: written,
            manifest_path,
            total_bytes,
        })
    }
}

// ===========================================================================
// 落盘结果/错误
// ===========================================================================

/// artifact 落盘结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactWritten {
    /// artifact 根目录。
    pub dir: PathBuf,
    /// 写入的文件（manifest 在首位）。
    pub files: Vec<PathBuf>,
    /// manifest 绝对/相对路径。
    pub manifest_path: PathBuf,
    /// 写入总字节数。
    pub total_bytes: u64,
}

/// artifact 落盘错误（含目标路径；不 panic）。
#[derive(Debug)]
pub struct ArtifactWriteError {
    /// 失败路径。
    pub path: PathBuf,
    /// io 类别（路径非法用 InvalidInput）。
    pub kind: io::ErrorKind,
    /// 人读说明。
    pub detail: String,
}

impl fmt::Display for ArtifactWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "artifact write failed at {} ({:?}): {}",
            self.path.display(),
            self.kind,
            self.detail
        )
    }
}

impl std::error::Error for ArtifactWriteError {}

fn write_error(path: &Path, error: &io::Error) -> ArtifactWriteError {
    ArtifactWriteError {
        path: path.to_path_buf(),
        kind: error.kind(),
        detail: error.to_string(),
    }
}

fn invalid_path_error(path: &Path, detail: String) -> ArtifactWriteError {
    ArtifactWriteError {
        path: path.to_path_buf(),
        kind: io::ErrorKind::InvalidInput,
        detail,
    }
}

// ===========================================================================
// 内部辅助（hash/JSON/数值/路径/角色）
// ===========================================================================

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn verify_sha256(field: &str, value: &str) -> Result<(), ArtifactError> {
    if !is_sha256_hex(value) {
        return Err(err(
            ArtifactErrorKind::Hash,
            field,
            format!("{value:?} is not a 64-digit lowercase sha256"),
        ));
    }
    Ok(())
}

fn is_known_status_code(code: &str) -> bool {
    matches!(
        code,
        F_TARGET_INFEASIBLE
            | F_HOLDOUT_PREDICTED_FAIL
            | R_SOLUTION_MISSING
            | R_SOLUTION_NOT_DELIVERABLE
            | R_POST_BASELINE_MISSING
            | R_HOLDOUT_MISSING
            | R_HOLDOUT_INCOMPLETE
            | R_HOLDOUT_NOT_RUN
            | R_TARGET_INCOMPLETE
            | R_TARGET_NOT_FULLY_FEASIBLE
            | R_MEASUREMENT_PROVENANCE_MISSING
            | R_CALIBRATION_PROVENANCE_MISSING
            | R_TUNING_INPUTS_MISSING
    )
}

fn is_role_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !bytes[0].is_ascii_uppercase() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_layout_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_tool_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn validate_rel_path(rel_path: &str) -> Result<(), ArtifactError> {
    if rel_path.is_empty() {
        return Err(err(
            ArtifactErrorKind::Hash,
            "files.rel_path",
            "empty relative path",
        ));
    }
    if rel_path.starts_with('/') || rel_path.contains('\\') || rel_path.contains(':') {
        return Err(err(
            ArtifactErrorKind::Hash,
            "files.rel_path",
            format!("{rel_path:?} must be a relative POSIX path"),
        ));
    }
    for segment in rel_path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(err(
                ArtifactErrorKind::Hash,
                "files.rel_path",
                format!("{rel_path:?} contains an empty/./.. segment"),
            ));
        }
    }
    Ok(())
}

fn stored_overlay_canonical(overlay: &ArtifactOverlay) -> String {
    let mut text = format!(
        "overlay layout={} source_sha256={} container={}\n",
        overlay.layout_id, overlay.source_sha256, overlay.container_channels
    );
    let mut roles: Vec<&OverlayRole> = overlay.roles.iter().collect();
    roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
    for role in roles {
        text.push_str(&role.canonical());
    }
    text
}

fn grid_point_equal(left: f64, right: f64) -> bool {
    let tolerance = 1.0e-9 * left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= tolerance
}

fn bands_equal(left: (f64, f64), right: (f64, f64)) -> bool {
    grid_point_equal(left.0, right.0) && grid_point_equal(left.1, right.1)
}

fn band_limit_str(limit: BandLimit) -> &'static str {
    match limit {
        BandLimit::None => "none",
        BandLimit::HighPass { .. } => "highpass",
        BandLimit::LowPass { .. } => "lowpass",
    }
}

fn fmt_f64(value: f64) -> String {
    let normalized = if value == 0.0 { 0.0 } else { value };
    format!("{normalized:.17e}")
}

fn f64_path_sha256(values: &[f64]) -> String {
    let mut text = String::new();
    for value in values {
        text.push_str(&fmt_f64(*value));
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

fn f32_bits_sha256(values: &[f32]) -> String {
    let mut text = String::new();
    for value in values {
        text.push_str(&format!("{:08x}\n", value.to_bits()));
    }
    sha256_hex(text.as_bytes())
}

fn solve_coefficient_sha256(coeffs: &[f64]) -> String {
    let mut text = String::new();
    for value in coeffs {
        text.push_str(&fmt_f64(*value));
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

fn regularize_coefficient_sha256(coeffs: &[f64]) -> String {
    let mut text = String::new();
    text.push_str(crate::regularize::REGULARIZE_HASH_DOMAIN);
    text.push('\n');
    text.push_str("coeffs\n");
    for value in coeffs {
        text.push_str(&fmt_f64(*value));
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

fn json_opt_str(value: &Option<String>) -> String {
    match value {
        Some(value) => json_str(value),
        None => "null".to_string(),
    }
}

fn json_num(value: f64) -> String {
    if !value.is_finite() {
        "null".to_string()
    } else {
        fmt_f64(value)
    }
}

fn json_opt_num(value: Option<f64>) -> String {
    match value {
        Some(value) => json_num(value),
        None => "null".to_string(),
    }
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn json_str_array<S: AsRef<str>>(values: &[S]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_str(value.as_ref()));
    }
    out.push(']');
    out
}

fn json_pair(value: (f64, f64)) -> String {
    format!("[{},{}]", json_num(value.0), json_num(value.1))
}

/// 静态守卫文本（required 语义探针；evidence/T54 用）。
pub fn artifact_source_text_for_guards() -> &'static str {
    "build_artifact;encode_wav_f32_mono;decode_wav_f32_mono;to_manifest_json;role_hash;stale_reasons;INCOMPLETE;PREDICTED_FAIL;holdout_used_for_tuning;wav_f32_mono;participants;stereo_mask;overlay"
}
