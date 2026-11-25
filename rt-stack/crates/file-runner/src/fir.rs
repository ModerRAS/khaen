//! [NONRT] `tspi-fir/v1` FIR 系数 manifest：解析/校验/写作（对齐 T55 reporter + fir-toolbox §2）。
//!
//! WBS T14 的“metadata”与 DSP-HOST-DECISION §2.6 的“每 path mono FIR WAV + manifest/设计报告”：
//! runner 从 `tspi-fir/v1` manifest 加载 A/G/P 每条路径的 f32 系数，并校验 spec/fir-toolbox.md
//! §2.2 的规范性规则（T55 报告字段面直接消费）：
//!
//! 1. `taps` 与系数文件实际样本数相等（E_FILE_TAPS）；系数文件必须是 mono f32 WAV @ manifest.fs，
//!    或 `raw_f32le`（§2.1 允许二者）；
//! 2. `delay_samples` 与家族规则一致（linear/apodizing/brickwall 常数相位 D=(L-1)/2；
//!    min_*/corrected/lowdelay 声明值；nos D=0、taps=1、h=[1]）（E_FILE_DELAY/E_FILE_NUMERIC）；
//! 3. 全部 h 有限，max|h| > 0（nos 除外）（E_FILE_NUMERIC）；
//! 4. `reference.hz` 处 |H| 与 `reference.gain_db` 差 <= 0.01 dB（E_FILE_REF，T55 §1.5 同口径）；
//! 5. `response.endpoints.f20/f20k` 联合类型齐全（measured / not_applicable_at_fs）（E_FILE_SCHEMA）；
//! 6. `coefficients.sha256` 与实际字节一致、`bytes` 与文件字节一致（E_FILE_HASH）；
//! 7. `limits.fits_budget=false` 或 `profile=offline_only` 的产物禁止被 runtime profile 加载
//!    （E_FILE_BUDGET）；regular <= 4096 taps、extended <= 16384 taps，不静默截断。
//!
//! 附加完整性闸门（T55 §2.2 `verification.passed`）：设计报告判据未通过（passed != true）的
//! manifest 不得进入运行图（E_FILE_SCHEMA）；runner 不重算 T55 的 20/20k 频响判据，只消费其字段。
//!
//! 未知字段：`x_` 前缀允许（记录 warning），其它未知顶层字段拒绝（E_FILE_SCHEMA；与 spec §2.2 同口径）。
//! 写作：[`build_fir_manifest`]/[`write_fir_manifest`] 生成确定性 canonical JSON（无
//! created_unix/时间戳），供 T26/T55 导出系数文件后与 WAV 一起下发；runner 加载时重新校验。

use std::collections::BTreeMap;

use crate::json::Json;
use crate::sha256::{is_hex64, sha256_hex};
use crate::wav::WavFormat;
use crate::{
    RunnerError, RunnerResult, E_FILE_BUDGET, E_FILE_DELAY, E_FILE_FORMAT, E_FILE_HASH,
    E_FILE_NUMERIC, E_FILE_PROFILE, E_FILE_RATE, E_FILE_REF, E_FILE_SCHEMA, E_FILE_TAPS,
};

/// 系数 manifest schema（spec/fir-toolbox.md §2.2）。
pub const FIR_SCHEMA: &str = "tspi-fir/v1";
/// 支持的七家族 + lowdelay 别名（manifest family 枚举）。
pub const FIR_FAMILIES: [&str; 8] = [
    "linear",
    "min_fast",
    "min_slow",
    "apodizing",
    "brickwall",
    "corrected",
    "lowdelay",
    "nos",
];
/// WAV 系数格式（mono f32；§2.1 首选）。
pub const FIR_FORMAT_WAV: &str = "wav_f32_mono";
/// 原始 f32le 系数格式（§2.1 允许的备选）。
pub const FIR_FORMAT_RAW: &str = "raw_f32le";
/// reference 归一化容差（spec §2.2 规则 4：±0.01 dB）。
pub const FIR_REFERENCE_TOLERANCE_DB: f64 = 0.01;
/// delay_ms 与 delay_samples/fs 的一致性容差（数值写作误差）。
pub const FIR_DELAY_MS_TOLERANCE: f64 = 1e-6;
/// 设计期 taps 下限（§2.1）。
pub const FIR_TAPS_MIN: u32 = 1;
/// 设计期 taps 上限（§2.1）。
pub const FIR_TAPS_MAX: u32 = 16_384;
/// 常规 profile 名（core-v1 §3.3）。
pub const PROFILE_REGULAR: &str = "regular";
/// extended profile 名（core-v1 §3.3；8192/16384 需显式准入）。
pub const PROFILE_EXTENDED: &str = "extended";
/// 仅离线设计产物（不得被 runtime profile 加载）。
pub const PROFILE_OFFLINE_ONLY: &str = "offline_only";

/// manifest 的系数文件引用（§2.2 `coefficients`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirCoefficients {
    /// 相对路径/文件标识（由调用方 blob source 解析）。
    pub file: String,
    /// `wav_f32_mono` 或 `raw_f32le`。
    pub format: String,
    /// 文件字节数（必须等于实际字节）。
    pub bytes: u64,
    /// 文件字节 SHA-256（小写/大写 hex；比较时不区分大小写）。
    pub sha256: String,
}

/// manifest 的预算声明（§2.2 `limits`）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirLimits {
    /// `regular` / `extended` / `offline_only`。
    pub profile: String,
    /// 每 route taps 上限。
    pub route_taps_cap: u32,
    /// f32 系数 RAM 字节（建议 = taps × 4）。
    pub ram_bytes_f32: u64,
    /// 声明延迟预算（ms；可缺省）。
    pub latency_budget_ms: Option<f64>,
    /// 预算判定；false 禁止加载（§2.2 规则 7）。
    pub fits_budget: bool,
}

/// 解析后的 `tspi-fir/v1` manifest（未加载系数）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirManifest {
    /// schema（必须 = [`FIR_SCHEMA`]）。
    pub schema: String,
    /// tool.name。
    pub tool_name: String,
    /// tool.version。
    pub tool_version: String,
    /// created_unix（可选；不参与系数 hash/图版本）。
    pub created_unix: Option<u64>,
    /// 家族（[`FIR_FAMILIES`] 之一）。
    pub family: String,
    /// 采样率。
    pub fs: u32,
    /// taps（= 系数样本数）。
    pub taps: u32,
    /// 声明延迟（样本；可为半整数）。
    pub delay_samples: f64,
    /// 声明延迟（ms）。
    pub delay_ms: f64,
    /// 系数文件引用。
    pub coefficients: FirCoefficients,
    /// 预算声明。
    pub limits: FirLimits,
    /// warnings（manifest 自带；x_ 未知字段也追加为 warning）。
    pub warnings: Vec<String>,
    /// 原始 JSON（保留 T55 报告字段面：design/source/response/verification 等）。
    pub raw: Json,
}

/// 加载并校验后的系数（可直接进入图；f32 精度锁）。
#[derive(Clone, Debug)]
pub struct LoadedFir {
    /// manifest。
    pub manifest: FirManifest,
    /// f32 系数（长度 = taps；mono）。
    pub coeffs: Vec<f32>,
    /// 系数文件字节 SHA-256（小写 hex）。
    pub file_sha256: String,
    /// manifest 语义摘要 SHA-256（小写 hex；排除 created_unix/warnings/x_，图版本/报告用）。
    pub manifest_sha256: String,
    /// reference.hz 与 gain_db（加载/校验用）。
    pub reference_hz: f64,
    /// reference.gain_db。
    pub reference_gain_db: f64,
}

const FIR_MANIFEST_KNOWN_FIELDS: [&str; 16] = [
    "schema",
    "tool",
    "created_unix",
    "family",
    "fs",
    "taps",
    "delay_samples",
    "delay_ms",
    "design",
    "source",
    "reference",
    "response",
    "verification",
    "coefficients",
    "limits",
    "warnings",
];

/// 解析 manifest JSON（schema/字段/枚举/格式；不加载系数）。
pub fn parse_fir_manifest(value: &Json) -> RunnerResult<FirManifest> {
    let object = value
        .as_object()
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "fir manifest must be a JSON object"))?;
    for key in object.keys() {
        if !FIR_MANIFEST_KNOWN_FIELDS.contains(&key.as_str()) && !key.starts_with("x_") {
            return Err(RunnerError::new(
                E_FILE_SCHEMA,
                format!("unknown fir manifest field '{}' (x_ prefix required)", key),
            ));
        }
    }
    let schema = value.require_str("schema")?;
    if schema != FIR_SCHEMA {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!(
                "fir manifest schema must be '{}' (got '{}')",
                FIR_SCHEMA, schema
            ),
        ));
    }
    let tool = value.require_object("tool")?;
    let tool_name = json_string(tool, "name", "tool")?;
    let tool_version = json_string(tool, "version", "tool")?;
    let created_unix = match value.get("created_unix") {
        Some(entry) => Some(entry.as_u64().ok_or_else(|| {
            RunnerError::new(E_FILE_SCHEMA, "created_unix must be a non-negative integer")
        })?),
        None => None,
    };
    let family = value.require_str("family")?.to_string();
    if !FIR_FAMILIES.contains(&family.as_str()) {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!("unsupported fir family '{}'", family),
        ));
    }
    let fs = value.require_u64("fs")?;
    if fs == 0 || fs > 384_000 {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!("fs {} outside 1..=384000", fs),
        ));
    }
    let taps = value.require_u64("taps")?;
    if taps < FIR_TAPS_MIN as u64 || taps > FIR_TAPS_MAX as u64 {
        return Err(RunnerError::new(
            E_FILE_TAPS,
            format!("taps {} outside {}..={}", taps, FIR_TAPS_MIN, FIR_TAPS_MAX),
        ));
    }
    let taps = taps as u32;
    let delay_samples = value.require_f64("delay_samples")?;
    if !delay_samples.is_finite() || delay_samples < 0.0 {
        return Err(RunnerError::new(
            E_FILE_DELAY,
            "delay_samples must be finite and >= 0",
        ));
    }
    let delay_ms = value.require_f64("delay_ms")?;
    if !delay_ms.is_finite() || delay_ms < 0.0 {
        return Err(RunnerError::new(
            E_FILE_DELAY,
            "delay_ms must be finite and >= 0",
        ));
    }
    let expected_ms = delay_samples / (fs as f64) * 1000.0;
    if (delay_ms - expected_ms).abs() > FIR_DELAY_MS_TOLERANCE {
        return Err(RunnerError::new(
            E_FILE_DELAY,
            format!(
                "delay_ms {} inconsistent with delay_samples/fs ({})",
                delay_ms, expected_ms
            ),
        ));
    }
    let coeff_object = value.require_object("coefficients")?;
    let file = json_string(coeff_object, "file", "coefficients")?;
    if file.is_empty() {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            "coefficients.file is empty",
        ));
    }
    let format = json_string(coeff_object, "format", "coefficients")?;
    if format != FIR_FORMAT_WAV && format != FIR_FORMAT_RAW {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!("coefficients.format '{}' unsupported", format),
        ));
    }
    let coeff_bytes = json_u64(coeff_object, "bytes", "coefficients")?;
    let coeff_sha = json_string(coeff_object, "sha256", "coefficients")?;
    if !is_hex64(&coeff_sha) {
        return Err(RunnerError::new(
            E_FILE_HASH,
            "coefficients.sha256 must be 64 hex chars",
        ));
    }
    let limits_object = value.require_object("limits")?;
    let profile = json_string(limits_object, "profile", "limits")?;
    if ![PROFILE_REGULAR, PROFILE_EXTENDED, PROFILE_OFFLINE_ONLY].contains(&profile.as_str()) {
        return Err(RunnerError::new(
            E_FILE_PROFILE,
            format!("limits.profile '{}' unsupported", profile),
        ));
    }
    let route_taps_cap = json_u64(limits_object, "route_taps_cap", "limits")?;
    if route_taps_cap == 0 || route_taps_cap > FIR_TAPS_MAX as u64 {
        return Err(RunnerError::new(
            E_FILE_BUDGET,
            format!("limits.route_taps_cap {} outside 1..=16384", route_taps_cap),
        ));
    }
    let ram_bytes_f32 = json_u64(limits_object, "ram_bytes_f32", "limits")?;
    let latency_budget_ms = match limits_object.get("latency_budget_ms") {
        Some(entry) => {
            let value = entry.as_f64().ok_or_else(|| {
                RunnerError::new(E_FILE_SCHEMA, "limits.latency_budget_ms must be a number")
            })?;
            if !value.is_finite() || value < 0.0 {
                return Err(RunnerError::new(
                    E_FILE_BUDGET,
                    "limits.latency_budget_ms must be finite and >= 0",
                ));
            }
            Some(value)
        }
        None => None,
    };
    let fits_budget = match limits_object.get("fits_budget") {
        Some(entry) => entry
            .as_bool()
            .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "limits.fits_budget must be a bool"))?,
        None => {
            return Err(RunnerError::new(
                E_FILE_SCHEMA,
                "missing required field 'fits_budget' in limits",
            ))
        }
    };
    let mut warnings_out: Vec<String> = Vec::new();
    if let Some(entries) = value.get("warnings") {
        let array = entries
            .as_array()
            .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "warnings must be an array"))?;
        for entry in array {
            let text = entry.as_str().ok_or_else(|| {
                RunnerError::new(E_FILE_SCHEMA, "warnings entries must be strings")
            })?;
            warnings_out.push(text.to_string());
        }
    }
    if object.keys().any(|key| key.starts_with("x_")) {
        warnings_out.push("manifest contains x_ extension fields (ignored)".to_string());
    }
    Ok(FirManifest {
        schema: schema.to_string(),
        tool_name,
        tool_version,
        created_unix,
        family,
        fs: fs as u32,
        taps,
        delay_samples,
        delay_ms,
        coefficients: FirCoefficients {
            file,
            format,
            bytes: coeff_bytes,
            sha256: coeff_sha,
        },
        limits: FirLimits {
            profile,
            route_taps_cap: route_taps_cap as u32,
            ram_bytes_f32,
            latency_budget_ms,
            fits_budget,
        },
        warnings: warnings_out,
        raw: value.clone(),
    })
}

/// 家族是否常数相位（D = (taps-1)/2；linear/apodizing/brickwall；spec §3.1/§3.4/§3.5）。
pub fn family_constant_delay(family: &str) -> bool {
    matches!(family, "linear" | "apodizing" | "brickwall")
}

/// 家族期望的常数延迟（样本；None = 声明值，无固定规则）。
pub fn family_expected_delay(family: &str, taps: u32) -> Option<f64> {
    if family_constant_delay(family) {
        Some((taps as f64 - 1.0) / 2.0)
    } else if family == "nos" {
        Some(0.0)
    } else {
        None
    }
}

/// 计算 f32 系数在 freq_hz（Hz）处的 DTFT 幅度（dB；f64 内部精度）。
///
/// 用于 spec §2.2 规则 4 的 reference 归一化校验与报告复算；非有限输入返回 NaN（调用方拒绝）。
pub fn magnitude_db_at(coeffs: &[f32], fs: f64, freq_hz: f64) -> f64 {
    if !fs.is_finite() || fs <= 0.0 || !freq_hz.is_finite() {
        return f64::NAN;
    }
    let mut re = 0.0f64;
    let mut im = 0.0f64;
    let omega = -2.0 * std::f64::consts::PI * freq_hz / fs;
    let (sin_step, cos_step) = omega.sin_cos();
    let mut cos_n = 1.0f64;
    let mut sin_n = 0.0f64;
    for &h in coeffs.iter() {
        let value = h as f64;
        re += value * cos_n;
        im += value * sin_n;
        let next_cos = cos_n * cos_step - sin_n * sin_step;
        let next_sin = sin_n * cos_step + cos_n * sin_step;
        cos_n = next_cos;
        sin_n = next_sin;
    }
    let mag = (re * re + im * im).sqrt();
    if !mag.is_finite() {
        return f64::NAN;
    }
    20.0 * mag.max(1e-12).log10()
}

/// reference 归一化声明（§2.2 `reference`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirReference {
    /// 参考频率（Hz）。
    pub hz: f64,
    /// 参考增益（dB）。
    pub gain_db: f64,
    /// 实测参考增益（可选；T55 报告字段）。
    pub measured_gain_db: Option<f64>,
}

/// 解析 `reference` 对象。
pub fn parse_reference(manifest: &FirManifest) -> RunnerResult<FirReference> {
    let object = manifest
        .raw
        .require_object("reference")
        .map_err(|_| RunnerError::new(E_FILE_SCHEMA, "missing required field 'reference'"))?;
    let hz = object
        .get("hz")
        .and_then(Json::as_f64)
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "reference.hz must be a number"))?;
    let gain_db = object
        .get("gain_db")
        .and_then(Json::as_f64)
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "reference.gain_db must be a number"))?;
    if !hz.is_finite() || hz <= 0.0 || hz >= manifest.fs as f64 / 2.0 {
        return Err(RunnerError::new(
            E_FILE_REF,
            format!("reference.hz {} outside (0, fs/2)", hz),
        ));
    }
    if !gain_db.is_finite() {
        return Err(RunnerError::new(
            E_FILE_REF,
            "reference.gain_db must be finite",
        ));
    }
    let measured_gain_db = match object.get("measured_gain_db") {
        Some(entry) => {
            let value = entry.as_f64().ok_or_else(|| {
                RunnerError::new(E_FILE_REF, "reference.measured_gain_db must be a number")
            })?;
            if !value.is_finite() {
                return Err(RunnerError::new(
                    E_FILE_REF,
                    "reference.measured_gain_db must be finite",
                ));
            }
            Some(value)
        }
        None => None,
    };
    Ok(FirReference {
        hz,
        gain_db,
        measured_gain_db,
    })
}

/// 校验 `response.endpoints.f20/f20k` 联合类型（spec §2.4：不省端点、不用裸字符串）。
pub fn validate_report_endpoints(manifest: &FirManifest) -> RunnerResult<()> {
    let response = manifest
        .raw
        .require_object("response")
        .map_err(|_| RunnerError::new(E_FILE_SCHEMA, "missing required field 'response'"))?;
    let endpoints = match response.get("endpoints") {
        Some(value) => value.as_object().ok_or_else(|| {
            RunnerError::new(E_FILE_SCHEMA, "response.endpoints must be an object")
        })?,
        None => {
            return Err(RunnerError::new(
                E_FILE_SCHEMA,
                "missing required field 'endpoints' in response",
            ))
        }
    };
    for key in ["f20", "f20k"] {
        let entry = endpoints.get(key).ok_or_else(|| {
            RunnerError::new(
                E_FILE_SCHEMA,
                format!(
                    "missing endpoint '{}' (20 Hz / 20 kHz endpoints must not be omitted)",
                    key
                ),
            )
        })?;
        validate_endpoint(entry, key)?;
    }
    Ok(())
}

fn validate_endpoint(entry: &Json, key: &str) -> RunnerResult<()> {
    let status = entry
        .require_str("status")
        .map_err(|_| RunnerError::new(E_FILE_SCHEMA, format!("endpoint {} missing status", key)))?;
    match status {
        "measured" => {
            entry.require_f64("freq_hz")?;
            entry.require_f64("mag_db")?;
            entry.require_f64("phase_deg")?;
        }
        "not_applicable_at_fs" => {
            let reason = entry.require_str("reason").map_err(|_| {
                RunnerError::new(E_FILE_SCHEMA, format!("endpoint {} missing reason", key))
            })?;
            if reason.is_empty() {
                return Err(RunnerError::new(
                    E_FILE_SCHEMA,
                    format!("endpoint {} reason is empty", key),
                ));
            }
        }
        other => {
            return Err(RunnerError::new(
                E_FILE_SCHEMA,
                format!("endpoint {} status '{}' invalid", key, other),
            ))
        }
    }
    Ok(())
}

/// manifest 语义摘要 SHA-256（排除 created_unix/warnings/x_/报告自由文本；图版本用）。
pub fn manifest_semantic_sha256(manifest: &FirManifest) -> String {
    let reference = parse_reference(manifest).ok();
    let mut object = BTreeMap::new();
    object.insert("schema".to_string(), Json::String(FIR_SCHEMA.to_string()));
    object.insert("family".to_string(), Json::String(manifest.family.clone()));
    object.insert("fs".to_string(), Json::Number(manifest.fs as f64));
    object.insert("taps".to_string(), Json::Number(manifest.taps as f64));
    object.insert(
        "delay_samples".to_string(),
        Json::Number(manifest.delay_samples),
    );
    object.insert("delay_ms".to_string(), Json::Number(manifest.delay_ms));
    object.insert(
        "coefficients_file".to_string(),
        Json::String(manifest.coefficients.file.clone()),
    );
    object.insert(
        "coefficients_format".to_string(),
        Json::String(manifest.coefficients.format.clone()),
    );
    object.insert(
        "coefficients_bytes".to_string(),
        Json::Number(manifest.coefficients.bytes as f64),
    );
    object.insert(
        "coefficients_sha256".to_string(),
        Json::String(manifest.coefficients.sha256.to_lowercase()),
    );
    object.insert(
        "profile".to_string(),
        Json::String(manifest.limits.profile.clone()),
    );
    object.insert(
        "route_taps_cap".to_string(),
        Json::Number(manifest.limits.route_taps_cap as f64),
    );
    object.insert(
        "ram_bytes_f32".to_string(),
        Json::Number(manifest.limits.ram_bytes_f32 as f64),
    );
    object.insert(
        "fits_budget".to_string(),
        Json::Bool(manifest.limits.fits_budget),
    );
    if let Some(reference) = reference {
        object.insert("reference_hz".to_string(), Json::Number(reference.hz));
        object.insert(
            "reference_gain_db".to_string(),
            Json::Number(reference.gain_db),
        );
    }
    sha256_hex(Json::Object(object).to_canonical().as_bytes())
}

/// 校验 manifest 与系数文件字节，返回可进图的 [`LoadedFir`]。
///
/// `runtime_profile` 属于 {regular, extended}（执行 profile）；offline_only 产物不得加载。
pub fn validate_fir_manifest(
    manifest: &FirManifest,
    file_bytes: &[u8],
    runtime_profile: &str,
) -> RunnerResult<LoadedFir> {
    if runtime_profile != PROFILE_REGULAR && runtime_profile != PROFILE_EXTENDED {
        return Err(RunnerError::new(
            E_FILE_PROFILE,
            format!(
                "runtime profile '{}' invalid (regular/extended)",
                runtime_profile
            ),
        ));
    }
    let file_sha256 = sha256_hex(file_bytes);
    if !file_sha256.eq_ignore_ascii_case(&manifest.coefficients.sha256) {
        return Err(RunnerError::new(
            E_FILE_HASH,
            format!(
                "coefficients.sha256 mismatch for '{}' (manifest {}, computed {})",
                manifest.coefficients.file, manifest.coefficients.sha256, file_sha256
            ),
        ));
    }
    if file_bytes.len() as u64 != manifest.coefficients.bytes {
        return Err(RunnerError::new(
            E_FILE_HASH,
            format!(
                "coefficients.bytes mismatch for '{}' (manifest {}, actual {})",
                manifest.coefficients.file,
                manifest.coefficients.bytes,
                file_bytes.len()
            ),
        ));
    }
    if !manifest.limits.fits_budget {
        return Err(RunnerError::new(
            E_FILE_BUDGET,
            format!(
                "manifest '{}' declares fits_budget=false; must not be loaded for runtime",
                manifest.coefficients.file
            ),
        ));
    }
    if manifest.limits.profile == PROFILE_OFFLINE_ONLY {
        return Err(RunnerError::new(
            E_FILE_BUDGET,
            format!(
                "manifest '{}' is offline_only; must not be loaded for runtime",
                manifest.coefficients.file
            ),
        ));
    }
    if manifest.taps as u64 > manifest.limits.route_taps_cap as u64 {
        return Err(RunnerError::new(
            E_FILE_BUDGET,
            format!(
                "taps {} exceeds declared route_taps_cap {}",
                manifest.taps, manifest.limits.route_taps_cap
            ),
        ));
    }
    if runtime_profile == PROFILE_REGULAR {
        if manifest.limits.profile != PROFILE_REGULAR {
            return Err(RunnerError::new(
                E_FILE_BUDGET,
                format!(
                    "manifest profile '{}' cannot load into regular runtime profile",
                    manifest.limits.profile
                ),
            ));
        }
        if manifest.taps > crate::ROUTE_TAPS_REGULAR_MAX {
            return Err(RunnerError::new(
                E_FILE_BUDGET,
                format!(
                    "taps {} exceed regular profile cap {}",
                    manifest.taps,
                    crate::ROUTE_TAPS_REGULAR_MAX
                ),
            ));
        }
    } else if manifest.taps > crate::ROUTE_TAPS_EXTENDED_MAX {
        return Err(RunnerError::new(
            E_FILE_BUDGET,
            format!(
                "taps {} exceed extended profile cap {}",
                manifest.taps,
                crate::ROUTE_TAPS_EXTENDED_MAX
            ),
        ));
    }
    if manifest.limits.ram_bytes_f32 != manifest.taps as u64 * 4 {
        return Err(RunnerError::new(
            E_FILE_BUDGET,
            format!(
                "ram_bytes_f32 {} != taps*4 ({})",
                manifest.limits.ram_bytes_f32,
                manifest.taps as u64 * 4
            ),
        ));
    }

    let coeffs = load_coefficients(manifest, file_bytes)?;
    if coeffs.len() != manifest.taps as usize {
        return Err(RunnerError::new(
            E_FILE_TAPS,
            format!(
                "coefficient sample count {} != declared taps {}",
                coeffs.len(),
                manifest.taps
            ),
        ));
    }
    let mut max_abs = 0.0f64;
    for &value in &coeffs {
        if !value.is_finite() {
            return Err(RunnerError::new(
                E_FILE_NUMERIC,
                format!(
                    "coefficient '{}' contains NaN/Inf",
                    manifest.coefficients.file
                ),
            ));
        }
        max_abs = max_abs.max((value as f64).abs());
    }
    if manifest.family == "nos" {
        if manifest.taps != 1 || coeffs.len() != 1 || coeffs[0] != 1.0 {
            return Err(RunnerError::new(
                E_FILE_NUMERIC,
                "nos family must be exactly h=[1.0] with taps=1",
            ));
        }
    } else if max_abs == 0.0 {
        return Err(RunnerError::new(
            E_FILE_NUMERIC,
            format!(
                "coefficient '{}' is all zero (max|h|=0)",
                manifest.coefficients.file
            ),
        ));
    }
    if let Some(expected) = family_expected_delay(&manifest.family, manifest.taps) {
        if (manifest.delay_samples - expected).abs() > 1e-9 {
            return Err(RunnerError::new(
                E_FILE_DELAY,
                format!(
                    "family '{}' requires delay {} samples (manifest {})",
                    manifest.family, expected, manifest.delay_samples
                ),
            ));
        }
    }
    let reference = parse_reference(manifest)?;
    let measured = magnitude_db_at(&coeffs, manifest.fs as f64, reference.hz);
    if !measured.is_finite() || (measured - reference.gain_db).abs() > FIR_REFERENCE_TOLERANCE_DB {
        return Err(RunnerError::new(
            E_FILE_REF,
            format!(
                "reference normalization failed: |H({} Hz)| = {} dB, declared {} dB (tolerance {} dB)",
                reference.hz, measured, reference.gain_db, FIR_REFERENCE_TOLERANCE_DB
            ),
        ));
    }
    validate_report_endpoints(manifest)?;
    validate_verification(manifest)?;
    Ok(LoadedFir {
        manifest: manifest.clone(),
        coeffs,
        file_sha256,
        manifest_sha256: manifest_semantic_sha256(manifest),
        reference_hz: reference.hz,
        reference_gain_db: reference.gain_db,
    })
}

fn validate_verification(manifest: &FirManifest) -> RunnerResult<()> {
    let verification = manifest
        .raw
        .require_object("verification")
        .map_err(|_| RunnerError::new(E_FILE_SCHEMA, "missing required field 'verification'"))?;
    let method = verification
        .get("method")
        .and_then(Json::as_str)
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "verification.method must be a string"))?;
    if method.is_empty() {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            "verification.method is empty",
        ));
    }
    let passed = verification
        .get("passed")
        .and_then(Json::as_bool)
        .ok_or_else(|| RunnerError::new(E_FILE_SCHEMA, "verification.passed must be a bool"))?;
    if !passed {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!(
                "manifest '{}' reports verification.passed=false; failed design must not load",
                manifest.coefficients.file
            ),
        ));
    }
    Ok(())
}

/// 从系数文件字节加载 f32 样本（WAV mono f32 或 raw_f32le；不做 T22 设备 clip）。
fn load_coefficients(manifest: &FirManifest, file_bytes: &[u8]) -> RunnerResult<Vec<f32>> {
    match manifest.coefficients.format.as_str() {
        FIR_FORMAT_RAW => {
            if file_bytes.len() % 4 != 0 {
                return Err(RunnerError::new(
                    E_FILE_FORMAT,
                    "raw_f32le coefficient bytes are not a multiple of 4",
                ));
            }
            let mut coeffs = Vec::with_capacity(file_bytes.len() / 4);
            for chunk in file_bytes.chunks_exact(4) {
                coeffs.push(f32::from_bits(u32::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3],
                ])));
            }
            Ok(coeffs)
        }
        _ => {
            let (info, samples) = crate::wav::decode_float32_samples(file_bytes)?;
            if info.channels != 1 {
                return Err(RunnerError::new(
                    E_FILE_FORMAT,
                    format!(
                        "coefficient WAV must be mono (got {} channels)",
                        info.channels
                    ),
                ));
            }
            if info.format != WavFormat::Float32 {
                return Err(RunnerError::new(
                    E_FILE_FORMAT,
                    format!(
                        "coefficient WAV must be IEEE float32 (got {})",
                        info.format.as_str()
                    ),
                ));
            }
            if info.sample_rate != manifest.fs {
                return Err(RunnerError::new(
                    E_FILE_RATE,
                    format!(
                        "coefficient WAV sample_rate {} != manifest fs {}",
                        info.sample_rate, manifest.fs
                    ),
                ));
            }
            if info.frames != manifest.taps as u64 {
                return Err(RunnerError::new(
                    E_FILE_TAPS,
                    format!(
                        "coefficient WAV frames {} != declared taps {}",
                        info.frames, manifest.taps
                    ),
                ));
            }
            Ok(samples)
        }
    }
}

/// 解析 manifest JSON + 校验系数文件字节。
pub fn load_fir_manifest(
    manifest_json: &Json,
    file_bytes: &[u8],
    runtime_profile: &str,
) -> RunnerResult<LoadedFir> {
    let manifest = parse_fir_manifest(manifest_json)?;
    validate_fir_manifest(&manifest, file_bytes, runtime_profile)
}

/// 便捷：给定 manifest 文本与系数文件字节加载。
pub fn load_fir_manifest_str(
    manifest_text: &str,
    file_bytes: &[u8],
    runtime_profile: &str,
) -> RunnerResult<LoadedFir> {
    let json = Json::parse(manifest_text)?;
    load_fir_manifest(&json, file_bytes, runtime_profile)
}

/// 写作 `tspi-fir/v1` manifest 的规范输入（T26/T55 导出系数文件后调用）。
#[derive(Clone, Debug)]
pub struct FirManifestSpec<'a> {
    /// 家族（[`FIR_FAMILIES`]）。
    pub family: &'a str,
    /// 采样率（必须 = 系数 WAV 的 fs）。
    pub fs: u32,
    /// taps（= 系数样本数）。
    pub taps: u32,
    /// 声明延迟（样本；常数相位家族必填 (taps-1)/2）。
    pub delay_samples: f64,
    /// 工具名。
    pub tool_name: &'a str,
    /// 工具版本。
    pub tool_version: &'a str,
    /// 系数文件相对路径。
    pub coefficients_file: &'a str,
    /// `wav_f32_mono` 或 `raw_f32le`。
    pub coefficients_format: &'a str,
    /// 系数文件字节数。
    pub coefficients_bytes: u64,
    /// 系数文件 SHA-256（64 hex）。
    pub coefficients_sha256: &'a str,
    /// `regular` / `extended` / `offline_only`。
    pub profile: &'a str,
    /// 每 route taps 上限。
    pub route_taps_cap: u32,
    /// f32 系数字节（taps × 4）。
    pub ram_bytes_f32: u64,
    /// 预算判定。
    pub fits_budget: bool,
    /// reference.hz。
    pub reference_hz: f64,
    /// reference.gain_db。
    pub reference_gain_db: f64,
    /// reference.measured_gain_db（可选）。
    pub measured_gain_db: Option<f64>,
    /// f20 端点（[`measured`] / [`not_applicable_at_fs`] 联合类型 JSON）。
    pub endpoint_f20: Json,
    /// f20k 端点。
    pub endpoint_f20k: Json,
    /// design 对象（家族相关；缺省 = 空对象，保持 spec 字段面）。
    pub design: Option<Json>,
    /// source 对象（可选）。
    pub source: Option<Json>,
    /// verification.method。
    pub verification_method: &'a str,
    /// warnings。
    pub warnings: Vec<String>,
}

/// 构建 `tspi-fir/v1` manifest JSON（确定性；不含 created_unix 时间戳）。
pub fn build_fir_manifest(spec: &FirManifestSpec<'_>) -> RunnerResult<Json> {
    if !FIR_FAMILIES.contains(&spec.family) {
        return Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!("unsupported fir family '{}'", spec.family),
        ));
    }
    if spec.taps < FIR_TAPS_MIN || spec.taps > FIR_TAPS_MAX {
        return Err(RunnerError::new(
            E_FILE_TAPS,
            format!(
                "taps {} outside {}..={}",
                spec.taps, FIR_TAPS_MIN, FIR_TAPS_MAX
            ),
        ));
    }
    if !spec.delay_samples.is_finite() || spec.delay_samples < 0.0 {
        return Err(RunnerError::new(
            E_FILE_DELAY,
            "delay_samples must be finite and >= 0",
        ));
    }
    if !is_hex64(spec.coefficients_sha256) {
        return Err(RunnerError::new(
            E_FILE_HASH,
            "coefficients sha256 must be 64 hex chars",
        ));
    }
    let delay_ms = spec.delay_samples / (spec.fs as f64) * 1000.0;
    let reference = Json::from_entries(vec![
        ("hz", Json::Number(spec.reference_hz)),
        ("gain_db", Json::Number(spec.reference_gain_db)),
        (
            "measured_gain_db",
            match spec.measured_gain_db {
                Some(value) => Json::Number(value),
                None => Json::Number(spec.reference_gain_db),
            },
        ),
    ]);
    let response = Json::from_entries(vec![(
        "endpoints",
        Json::from_entries(vec![
            ("f20", spec.endpoint_f20.clone()),
            ("f20k", spec.endpoint_f20k.clone()),
        ]),
    )]);
    let verification = Json::from_entries(vec![
        ("method", Json::String(spec.verification_method.to_string())),
        ("passed", Json::Bool(true)),
        ("ref_impl", Json::String("tspi-file-runner/v1".to_string())),
    ]);
    let mut entries: Vec<(&str, Json)> = vec![
        ("schema", Json::String(FIR_SCHEMA.to_string())),
        (
            "tool",
            Json::from_entries(vec![
                ("name", Json::String(spec.tool_name.to_string())),
                ("version", Json::String(spec.tool_version.to_string())),
            ]),
        ),
        ("family", Json::String(spec.family.to_string())),
        ("fs", Json::Number(spec.fs as f64)),
        ("taps", Json::Number(spec.taps as f64)),
        ("delay_samples", Json::Number(spec.delay_samples)),
        ("delay_ms", Json::Number(delay_ms)),
        ("design", spec.design.clone().unwrap_or_else(Json::object)),
        ("source", spec.source.clone().unwrap_or_else(Json::object)),
        ("reference", reference),
        ("response", response),
        ("verification", verification),
        (
            "coefficients",
            Json::from_entries(vec![
                ("file", Json::String(spec.coefficients_file.to_string())),
                ("format", Json::String(spec.coefficients_format.to_string())),
                ("bytes", Json::Number(spec.coefficients_bytes as f64)),
                (
                    "sha256",
                    Json::String(spec.coefficients_sha256.to_lowercase()),
                ),
            ]),
        ),
        (
            "limits",
            Json::from_entries(vec![
                ("profile", Json::String(spec.profile.to_string())),
                ("route_taps_cap", Json::Number(spec.route_taps_cap as f64)),
                ("ram_bytes_f32", Json::Number(spec.ram_bytes_f32 as f64)),
                ("fits_budget", Json::Bool(spec.fits_budget)),
            ]),
        ),
        (
            "warnings",
            Json::Array(
                spec.warnings
                    .iter()
                    .map(|text| Json::String(text.clone()))
                    .collect(),
            ),
        ),
    ];
    entries.sort_by(|a, b| a.0.cmp(b.0));
    Ok(Json::from_entries(entries))
}

/// 构建并规范化为单个 JSON 字符串（确定性；writer 端）。
pub fn write_fir_manifest(spec: &FirManifestSpec<'_>) -> RunnerResult<String> {
    Ok(build_fir_manifest(spec)?.to_canonical())
}

/// manifest 绑定摘要（写进 runner 元数据；不改变语义）。
pub fn binding_json(loaded: &LoadedFir) -> Json {
    Json::from_entries(vec![
        (
            "file",
            Json::String(loaded.manifest.coefficients.file.clone()),
        ),
        ("family", Json::String(loaded.manifest.family.clone())),
        ("fs", Json::Number(loaded.manifest.fs as f64)),
        ("taps", Json::Number(loaded.manifest.taps as f64)),
        ("delay_samples", Json::Number(loaded.manifest.delay_samples)),
        (
            "profile",
            Json::String(loaded.manifest.limits.profile.clone()),
        ),
        (
            "fits_budget",
            Json::Bool(loaded.manifest.limits.fits_budget),
        ),
        ("file_sha256", Json::String(loaded.file_sha256.clone())),
        (
            "manifest_sha256",
            Json::String(loaded.manifest_sha256.clone()),
        ),
        (
            "verification_passed",
            Json::Bool(matches!(
                loaded
                    .manifest
                    .raw
                    .get("verification")
                    .and_then(|entry| entry.get("passed"))
                    .and_then(Json::as_bool),
                Some(true)
            )),
        ),
    ])
}

fn json_string(object: &BTreeMap<String, Json>, key: &str, context: &str) -> RunnerResult<String> {
    let entry = object.get(key).ok_or_else(|| {
        RunnerError::new(
            E_FILE_SCHEMA,
            format!("missing required field '{}.{}'", context, key),
        )
    })?;
    match entry.as_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(RunnerError::new(
            E_FILE_SCHEMA,
            format!("field '{}.{}' must be a string", context, key),
        )),
    }
}

fn json_u64(object: &BTreeMap<String, Json>, key: &str, context: &str) -> RunnerResult<u64> {
    let entry = object.get(key).ok_or_else(|| {
        RunnerError::new(
            E_FILE_SCHEMA,
            format!("missing required field '{}.{}'", context, key),
        )
    })?;
    entry.as_u64().ok_or_else(|| {
        RunnerError::new(
            E_FILE_SCHEMA,
            format!("field '{}.{}' must be a non-negative integer", context, key),
        )
    })
}
