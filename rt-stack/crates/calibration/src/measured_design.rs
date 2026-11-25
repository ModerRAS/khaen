//! [NONRT] T67 补齐：真实测量 -> 设计 fit 输入接线（旧工具 analysis JSON -> FitWiringDesign）。
//!
//! 背景：T67 pilot 的测量/分析链已通（UMIK-1 + MOTU 参考采集，真实 FR/IR），但新栈
//! `tspi-cal` 只支持 `--design synthetic`——真实测量数据无法进入 T64/T65 fit，导致
//! "测量 -> 设计 fit" 断链（pilot REPORT §9 B4）。
//!
//! 本模块把 **真实板端测量产物**（旧工具 `khaen-calibrate` 的 `*.analysis.json`，
//! schema `tspi-channel-measurement/v1`，含 1/48 oct FR 幅度+相位、绝对延迟、SNR、
//! 极性置信度；以及同目录 `*.diag.json` 的槽位 nominal/actual onset）转换为
//! [crate::fit_wiring::FitWiringDesign]：
//!
//! - T64 幅度曲线：analysis `fr[].db`（相对 1 kHz 形状，保留原始点）；
//! - T65 复响应：analysis `fr[].phase_deg` 按 `X(f)=|X|e^{+jφ}` 约定重建；
//!   相位加回实测到达延迟（优先 `diag.actual_start-nominal_start`，缺失时用
//!   analysis `delay_samples`），与 T65 `aligned_segment` 口径一致（延迟贡献
//!   `+2π·f·τ/fs`）；
//! - 计划/质量：单测点 pilot 无法满足 T63 "design+validation 独立分组"，故
//!   validation 用 `same_as_design` 代理并在证据里显式标注（proxy）；T6/T68 的
//!   独立 holdout 仍 NOT_RUN；T63 design 质量报告不注入 T64（`require_design_quality=false`），
//!   真实 SNR 写入 measured summary 供人工判读；
//! - **spec v2 多点（2026-11-22）**：可选 `design_positions[]` / `validation_positions[]`
//!   （每项 label/mic_orientation/可选 weight/channels）。v2 多点构建按位置生成
//!   `PositionCurve`（T64 用 design 位置聚合，validation 只进报告/holdout 评估）、
//!   T65 复响应只给 design 位置、T63 plan 独立标 design/validation；**v1 单点文件仍
//!   原样接受**（等价单点的 proxy 行为不变）。真实测量多点窗口/T6/T68 仍 NOT_RUN；
//! - T54 模态 artifact：pilot 单测点（或 2+1 pilot 点数 < T6 6+3）无法产生 READY 的
//!   T50-T54 artifact，因此 measured 路径的 artifact binding 标记 INCOMPLETE、G 为
//!   identity baseline（显式 provenance）；T66 compile 会以 `E_CA_ARTIFACT_STATUS`
//!   如实拒绝——**不伪造 READY**。
//!
//! # 诚实边界
//!
//! - 本模块只读文件、构造内存输入；不打开设备、不写板端、不 git、不碰 CueMix。
//! - `design_source="measured"+` 真实文件 hash 写入 evidence；analysis/diag 缺失/非法
//!   一律稳定错误码，不退化为合成数据。
//! - "measured 路径能跑到 compile 前置" != T67/T6 验收：T54 artifact/T3 复测/独立
//!   holdout/两轮复现仍 NOT_RUN（见 evidence/cursor-fix-calib-apply/）。
//! - spec v2 schema/语义见 `rt-stack/spec/measured-design-v2.md`；多点 holdout 目前是
//!   measured-at-validation-positions 的 **predicted 评估**（P 作用到 validation 位置实测
//!   曲线），不是 apply 后真实 remeasure；真实多点窗口/T6/T68 仍 NOT_RUN。

use std::fs;
use std::path::{Path, PathBuf};

use khaen_compat::ws::{json_object, json_parse, JsonValue};
use khaen_core::rt::RT_WORKERS;
use khaen_designer::modal_peq::CurvePoint;
use khaen_designer::targets::{builtin_target, BuiltinOptions, BuiltinTarget};
use khaen_modal_design::timebase::CommonD0;
use khaen_rt_host::admission::AdmissionEnv;

use crate::compile_apply::{
    ArtifactBinding, CompileApplyOptions, DelayPolarityEncoding, ExperimentMode, GPathInput,
    MappingBinding, RoleDacSlot, EXPERIMENT_ACCEPTABILITY, EXPERIMENT_VERDICT,
};
use crate::crossover_post::{CrossoverSpecConfig, E_MEASURED_CROSSOVER};
use crate::fit_wiring::{
    apply_measured_crossover_experiment, CompileWiringConfig, CrossoverPPostOutcome,
    FitWiringDesign,
};
use crate::job::sha256_hex;
use crate::peq_fit::{ChannelBand, PeqFitOptions, PeqFitReport, PositionCurve};
use crate::phase_fit::{
    DePhasedResponse, PhaseChannelBand, PhaseChannelIndex, PhaseFitOptions, PhaseFitPoint,
};
use crate::positions::{MultipointPlan, MultipointPosition};

/// measured design spec schema v1（旧单点 proxy 文件；仍接受）。
pub const MEASURED_SPEC_SCHEMA_V1: &str = "tspi-cal-measured-design/v1";
/// measured design spec schema v2：可选 `design_positions[]` / `validation_positions[]`。
pub const MEASURED_SPEC_SCHEMA_V2: &str = "tspi-cal-measured-design/v2";
/// measured design spec schema（兼容别名 = v1；v2 见 [MEASURED_SPEC_SCHEMA_V2]）。
pub const MEASURED_SPEC_SCHEMA: &str = MEASURED_SPEC_SCHEMA_V1;
/// measured design 证据 schema（summary artifact）。
pub const MEASURED_SUMMARY_SCHEMA: &str = "tspi-cal-measured-summary/v1";
/// 多点 measured 证据 schema（design/validation 逐位置残差 + 座位间 spread + holdout 口径）。
pub const MEASURED_MULTIPOINT_SCHEMA: &str = "tspi-cal-measured-multipoint/v1";
/// spec 文件非法/缺字段。
pub const E_MEASURED_SPEC: &str = "E_WIRING_MEASURED_SPEC";
/// analysis/diag 文件读取/解析失败。
pub const E_MEASURED_FILE: &str = "E_WIRING_MEASURED_FILE";
/// 测量数据不满足 fit 输入要求（FR 点不足/非单调/延迟异常）。
pub const E_MEASURED_DATA: &str = "E_WIRING_MEASURED_DATA";
/// 该 spec 需要但当前未实现的接线（T54 modal artifact 等）。
pub const E_MEASURED_NOT_WIRED: &str = "E_WIRING_MEASURED_NOT_WIRED";

/// 相位重建延迟来源（证据字段）。
pub const DELAY_SOURCE_DIAG_ONSET: &str = "diag_actual_minus_nominal";
/// 相位重建延迟来源（fallback）。
pub const DELAY_SOURCE_ANALYSIS: &str = "analysis_delay_samples";
/// 单测点 pilot 的 validation 代理来源标签。
pub const VALIDATION_PROXY_SAME_AS_DESIGN: &str = "pilot_single_position_same_as_design_proxy";
/// v2 多点 spec 的独立 validation/holdout 点位策略标签。
pub const VALIDATION_INDEPENDENT_POSITIONS: &str = "independent_validation_positions";
/// legacy proxy 的 validation 测点 label（历史证据固定值，不得静默改名）。
pub const VALIDATION_PROXY_POSITION_LABEL: &str = "V1_proxy";

/// 一个通道的测量文件引用。
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredChannelFile {
    /// 通道/role 名（^[A-Z][A-Z0-9]*$；与 mapping role id 一致）。
    pub name: String,
    /// 旧工具 analysis JSON 路径（绝对或相对 spec 目录）。
    pub analysis: PathBuf,
    /// 可选 diag JSON（槽位 nominal/actual onset；用于相位重建延迟）。
    pub diag: Option<PathBuf>,
}

/// 一个测量测点（v1 `design_position` / v2 `design_positions[]` / `validation_positions[]` 的项）。
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredPositionSpec {
    /// 测点 label（T63 plan；全 spec 唯一）。
    pub label: String,
    /// 麦克风朝向（T63 听音位 plan 要求 90deg）。
    pub mic_orientation: String,
    /// T63 权重（v2 可选字段；旧文件默认 1.0）。
    pub weight: f64,
    /// 该测点逐通道分析文件。
    pub channels: Vec<MeasuredChannelFile>,
}

/// measured design spec（CLI `--measurement FILE`）。
///
/// v1（`tspi-cal-measured-design/v1`）只有单个 `design_position` + same_as_design
/// proxy；v2（`.../v2`）可给 `design_positions[]` / `validation_positions[]`。
/// 内存模型统一为两个 Vec：`validation_positions` 为空 = legacy proxy。
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredDesignSpec {
    /// schema（v1 或 v2；解析后保留原字符串）。
    pub schema: String,
    /// session id。
    pub session_id: String,
    /// layout（如 stereo-2.0）。
    pub layout: String,
    /// 采样率（48k/96k）。
    pub sample_rate_hz: u32,
    /// T64 目标（只接受 flat/tilt10）。
    pub target: String,
    /// design 测点（>=1；参与 T64/T65 fit）。
    pub design_positions: Vec<MeasuredPositionSpec>,
    /// 独立 validation/holdout 测点（>=1 = 独立点位；空 = legacy same_as_design proxy）。
    pub validation_positions: Vec<MeasuredPositionSpec>,
    /// validation 策略（proxy 或 independent_validation_positions）。
    pub validation: String,
    /// 每通道工作带（可空 = 全带）。
    pub channel_bands: Vec<ChannelBand>,
    /// dense participant -> DAC 槽（如 stereo 2.0 = [0,1]）。
    pub dense_to_dac: Vec<u16>,
    /// 全部 role（role_index/role_id/dac/outputs）。
    pub roles: Vec<RoleDacSlot>,
    /// 容器通道数。
    pub container_channels: u16,
    /// 未使用输出槽。
    pub unused_outputs: Vec<u16>,
    /// T66 编译常量。
    pub compile: CompileWiringConfig,
    /// T54 modal artifact（目录/文件；当前 NOT_WIRED，只记录 provenance）。
    pub modal_artifact: Option<PathBuf>,
    /// T64 boost 上限（dB；None=默认 3）。
    pub boost_cap_db: Option<f64>,
    /// T64 总段数上限（None=默认）。
    pub max_total_stages: Option<usize>,
    /// T64 低频模态是否可信（None=默认 true；pilot 带限低 SNR 应显式 false）。
    pub low_band_trusted: Option<bool>,
    /// 测量来源证据标签（如 board_measured_pilot）。
    pub provenance: String,
    /// crossover（T39 输出 FIR 分频）配置（None = 关闭；生产默认 OFF）。
    pub crossover: Option<CrossoverSpecConfig>,
}

impl MeasuredDesignSpec {
    /// 首个 design 测点（spec 校验保证非空；调用前需确认 `!design_positions.is_empty()`）。
    pub fn primary_design_position(&self) -> &MeasuredPositionSpec {
        &self.design_positions[0]
    }

    /// validation 是否为 legacy single-point proxy（无独立 validation 点位）。
    pub fn is_validation_proxy(&self) -> bool {
        self.validation_positions.is_empty()
    }

    /// 真实测量点位总数（design + 独立 validation；proxy 不计）。
    pub fn measured_position_count(&self) -> usize {
        self.design_positions.len() + self.validation_positions.len()
    }
}

/// 从真实 analysis JSON 解析出的单通道测量（内部）。
#[derive(Clone, Debug, PartialEq)]
struct ParsedAnalysis {
    channel: u64,
    sample_rate_hz: u32,
    sweep_start_hz: f64,
    sweep_end_hz: f64,
    points: Vec<CurvePoint>,
    phases_deg: Vec<f64>,
    delay_samples: f64,
    snr_band_db: f64,
    polarity_confidence: String,
    sha256: String,
}

/// measured design 构造结果（design + 证据 JSON）。
#[derive(Clone, Debug)]
pub struct MeasuredDesign {
    /// 可直接交给 FitWiring 的输入。
    pub design: FitWiringDesign,
    /// 证据 JSON（measured summary；写 job artifact/CLI summary）。
    pub summary_json: String,
    /// 实验模式（OFF = 生产 measured 构建）。
    pub experiment: ExperimentMode,
    /// 生效 layout（生产 = spec.layout；实验可覆盖为 4.1.2）。
    pub effective_layout: String,
    /// G 来源标签（identity baseline / injected:<file>）。
    pub g_source: String,
    /// G 路径表 canonical sha256（证据）。
    pub g_paths_sha256: String,
}

// ---------------------------------------------------------------------------
// spec 解析
// ---------------------------------------------------------------------------

fn spec_err<T>(message: impl Into<String>) -> Result<T, String> {
    Err(format!("{E_MEASURED_SPEC}: {}", message.into()))
}

fn spec_string(document: &JsonValue, key: &str) -> Result<String, String> {
    document
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{E_MEASURED_SPEC}: 缺字符串字段 {key}"))
}

fn spec_u64(document: &JsonValue, key: &str) -> Result<u64, String> {
    document
        .get(key)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| format!("{E_MEASURED_SPEC}: 缺整数字段 {key}"))
}

fn spec_f64_opt(value: Option<&JsonValue>, what: &str) -> Result<f64, String> {
    value
        .and_then(|value| value.as_f64())
        .ok_or_else(|| format!("{E_MEASURED_SPEC}: 缺数字字段 {what}"))
}

fn spec_array<'a>(document: &'a JsonValue, key: &str) -> Result<&'a [JsonValue], String> {
    document
        .get(key)
        .and_then(|value| value.as_array())
        .ok_or_else(|| format!("{E_MEASURED_SPEC}: 缺数组字段 {key}"))
}

/// 读取并校验 spec JSON（相对路径按 spec 文件所在目录解析）。
pub fn load_measured_spec(spec_path: &Path) -> Result<MeasuredDesignSpec, String> {
    let text = fs::read_to_string(spec_path).map_err(|error| {
        format!(
            "{E_MEASURED_SPEC}: 读 spec {} 失败：{error}",
            spec_path.display()
        )
    })?;
    let document = json_parse(&text).map_err(|error| {
        format!(
            "{E_MEASURED_SPEC}: spec JSON 解析失败（offset {}）：{error}",
            error.offset
        )
    })?;
    let base_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let schema = spec_string(&document, "schema")?;
    if schema != MEASURED_SPEC_SCHEMA_V1 && schema != MEASURED_SPEC_SCHEMA_V2 {
        return spec_err(format!(
            "schema 必须为 {MEASURED_SPEC_SCHEMA_V1} 或 {MEASURED_SPEC_SCHEMA_V2}，得到 {schema:?}"
        ));
    }
    let (design_positions, validation_positions, validation) =
        parse_positions_section(&document, &schema, base_dir)?;
    let mut channel_bands = Vec::new();
    if let Some(bands) = document
        .get("channel_bands")
        .and_then(|value| value.as_array())
    {
        for band in bands {
            let channel = spec_string(band, "channel")?;
            let band_hz = spec_array(band, "band_hz")?;
            if band_hz.len() != 2 {
                return spec_err(format!("channel_bands[{channel}].band_hz 必须是 2 元数组"));
            }
            channel_bands.push(ChannelBand::new(
                channel,
                [
                    spec_f64_opt(band_hz.first(), "band_hz[0]")?,
                    spec_f64_opt(band_hz.get(1), "band_hz[1]")?,
                ],
            ));
        }
    }
    let dense_to_dac: Vec<u16> = spec_array(&document, "dense_to_dac")?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .ok_or_else(|| format!("{E_MEASURED_SPEC}: dense_to_dac 含非法值"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut roles = Vec::new();
    for role in spec_array(&document, "roles")? {
        let role_index = spec_u64(role, "role_index")? as u16;
        let role_id = spec_string(role, "role_id")?;
        let dac = spec_u64(role, "dac")? as u16;
        let outputs: Vec<u16> = spec_array(role, "outputs")?
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| format!("{E_MEASURED_SPEC}: role {role_id} outputs 含非法值"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        roles.push(
            RoleDacSlot::new(role_index, role_id, dac, outputs)
                .map_err(|error| format!("{E_MEASURED_SPEC}: role 构造失败：{error}"))?,
        );
    }
    let compile_value = document
        .get("compile")
        .ok_or_else(|| format!("{E_MEASURED_SPEC}: 缺 compile"))?;
    let mut compile = CompileWiringConfig::default();
    compile.block_size = spec_u64(compile_value, "block_size")? as u16;
    compile.n_in = spec_u64(compile_value, "n_in")? as u16;
    compile.container_channels = spec_u64(compile_value, "container_channels")? as u16;
    compile.low_band_d = spec_u64(compile_value, "low_band_d")? as u32;
    compile.l_g = spec_u64(compile_value, "l_g")? as u32;
    compile.l_p = spec_u64(compile_value, "l_p")? as u32;
    compile.encoding = DelayPolarityEncoding::RoleMetadata;
    compile.options = CompileApplyOptions::default();
    // plan ABI v2（GAP-T66-3）：T64 PEQ 段进 plan 的开关，profile 可显式回退；缺省 ON。
    if let Some(value) = compile_value.get("t64_peq_in_plan") {
        compile.t64_peq_in_plan = value.as_bool().ok_or_else(|| {
            format!("{E_MEASURED_SPEC}: compile.t64_peq_in_plan 必须是 bool")
        })?;
    }
    let unused_outputs: Vec<u16> = document
        .get("unused_outputs")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_u64())
                .filter_map(|value| u16::try_from(value).ok())
                .collect()
        })
        .unwrap_or_default();
    let modal_artifact = document
        .get("modal_artifact")
        .and_then(|value| value.as_str())
        .map(|value| resolve_relative(base_dir, value));
    let (boost_cap_db, max_total_stages, low_band_trusted) =
        parse_fit_options(document.get("fit_options"))?;
    let crossover = CrossoverSpecConfig::parse_section(&document)
        .map_err(|error| format!("{E_MEASURED_SPEC}: {error}"))?;
    let spec = MeasuredDesignSpec {
        schema,
        session_id: spec_string(&document, "session_id")?,
        layout: spec_string(&document, "layout")?,
        sample_rate_hz: spec_u64(&document, "sample_rate_hz")? as u32,
        target: spec_string(&document, "target")?,
        design_positions,
        validation_positions,
        validation,
        channel_bands,
        dense_to_dac,
        roles,
        container_channels: spec_u64(&document, "container_channels")? as u16,
        unused_outputs,
        compile,
        modal_artifact,
        boost_cap_db,
        max_total_stages,
        low_band_trusted,
        provenance: spec_string(&document, "provenance")?,
        crossover,
    };
    Ok(spec)
}

/// 解析 spec 的 position 段（v1/v2 向后兼容；不合法一律稳定错误码）。
fn parse_positions_section(
    document: &JsonValue,
    schema: &str,
    base_dir: &Path,
) -> Result<(Vec<MeasuredPositionSpec>, Vec<MeasuredPositionSpec>, String), String> {
    let design_single = document.get("design_position");
    let design_multi = document.get("design_positions");
    let validation_multi = document.get("validation_positions");
    let design_positions: Vec<MeasuredPositionSpec>;
    let validation_positions: Vec<MeasuredPositionSpec>;
    let validation: String;
    if schema == MEASURED_SPEC_SCHEMA_V1 {
        if design_multi.is_some() {
            return spec_err(
                "schema v1 不支持 design_positions[]（请用 schema v2 或改回 design_position）",
            );
        }
        if validation_multi.is_some() {
            return spec_err("schema v1 不支持 validation_positions[]（请用 schema v2）");
        }
        let value =
            design_single.ok_or_else(|| format!("{E_MEASURED_SPEC}: v1 缺 design_position"))?;
        design_positions = vec![parse_position(value, base_dir)?];
        validation_positions = Vec::new();
        validation = spec_string(document, "validation")?;
        if validation != VALIDATION_PROXY_SAME_AS_DESIGN {
            return spec_err(format!(
                "validation={validation:?} 在 v1 不支持：v1 单点只能 {VALIDATION_PROXY_SAME_AS_DESIGN} 代理（独立 validation 点位请用 schema v2 + validation_positions[]）"
            ));
        }
    } else {
        // v2：design_position 与 design_positions[] 二选一（避免歧义）。
        let design_positions_raw = match (design_single, design_multi) {
            (Some(_), Some(_)) => {
                return spec_err(
                    "v2 同时给了 design_position 与 design_positions[]（歧义；请只保留一种）",
                )
            }
            (None, None) => {
                return spec_err("v2 缺 design_positions[]（或单点兼容的 design_position）")
            }
            (Some(value), None) => vec![value],
            (None, Some(values)) => {
                let array = values
                    .as_array()
                    .ok_or_else(|| format!("{E_MEASURED_SPEC}: design_positions 必须是数组"))?;
                if array.is_empty() {
                    return spec_err("design_positions[] 为空（v2 至少 1 个 design 测点）");
                }
                array.iter().collect()
            }
        };
        design_positions = design_positions_raw
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                parse_position_at(value, base_dir, &format!("design_positions[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        validation_positions = match validation_multi {
            Some(values) => {
                let array = values
                    .as_array()
                    .ok_or_else(|| format!("{E_MEASURED_SPEC}: validation_positions 必须是数组"))?;
                array
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        parse_position_at(
                            value,
                            base_dir,
                            &format!("validation_positions[{index}]"),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => Vec::new(),
        };
        let validation_field = document.get("validation").and_then(|value| value.as_str());
        if validation_positions.is_empty() {
            // 无独立 validation：只允许 legacy proxy，且必须单 design 点（多点必须独立 holdout）。
            let value = validation_field.ok_or_else(|| {
                format!("{E_MEASURED_SPEC}: v2 缺 validation（无 validation_positions[] 时需要 {VALIDATION_PROXY_SAME_AS_DESIGN}）")
            })?;
            if value != VALIDATION_PROXY_SAME_AS_DESIGN {
                return spec_err(format!(
                    "v2 validation={value:?} 但未给 validation_positions[]：独立 holdout 必须提供点位文件（不接受空口号）"
                ));
            }
            if design_positions.len() != 1 {
                return spec_err(format!(
                    "v2 有 {} 个 design 测点但无 validation_positions[]：多点必须给独立 validation/holdout 点位（proxy 只允许单点）",
                    design_positions.len()
                ));
            }
            validation = value.to_string();
        } else {
            if let Some(value) = validation_field {
                if value != VALIDATION_INDEPENDENT_POSITIONS {
                    return spec_err(format!(
                        "v2 有 validation_positions[] 但 validation={value:?}（须省略或 {VALIDATION_INDEPENDENT_POSITIONS}）"
                    ));
                }
            }
            validation = VALIDATION_INDEPENDENT_POSITIONS.to_string();
        }
    }
    // 全 spec label 唯一 + 权重合法（plan 还会做一次 90deg 校验）。
    let mut labels: Vec<&str> =
        Vec::with_capacity(design_positions.len() + validation_positions.len());
    for position in design_positions.iter().chain(validation_positions.iter()) {
        if labels.contains(&position.label.as_str()) {
            return spec_err(format!(
                "测点 label {:?} 重复（design/validation 全 spec 唯一）",
                position.label
            ));
        }
        labels.push(position.label.as_str());
        if !position.weight.is_finite() || position.weight <= 0.0 {
            return spec_err(format!(
                "测点 {} weight={} 非法（须有限且 >0）",
                position.label, position.weight
            ));
        }
    }
    Ok((design_positions, validation_positions, validation))
}

/// 带错误上下文的位置解析（v2 数组项）。
fn parse_position_at(
    value: &JsonValue,
    base_dir: &Path,
    context: &str,
) -> Result<MeasuredPositionSpec, String> {
    parse_position(value, base_dir).map_err(|error| format!("{context}: {error}"))
}

fn parse_fit_options(
    value: Option<&JsonValue>,
) -> Result<(Option<f64>, Option<usize>, Option<bool>), String> {
    let Some(value) = value else {
        return Ok((None, None, None));
    };
    let boost_cap_db = value.get("boost_cap_db").and_then(|value| value.as_f64());
    if let Some(boost) = boost_cap_db {
        if !(0.0..=3.0).contains(&boost) {
            return spec_err(format!("fit_options.boost_cap_db={boost} 越界（0..=3）"));
        }
    }
    let max_total_stages = value
        .get("max_total_stages")
        .and_then(|value| value.as_u64())
        .map(|value| value as usize);
    if let Some(stages) = max_total_stages {
        if stages == 0 || stages > 64 {
            return spec_err(format!(
                "fit_options.max_total_stages={stages} 越界（1..=64）"
            ));
        }
    }
    let low_band_trusted = value
        .get("low_band_trusted")
        .and_then(|value| value.as_bool());
    Ok((boost_cap_db, max_total_stages, low_band_trusted))
}

// ---------------------------------------------------------------------------
// 实验旁路：G 注入文件（tspi-cal-g-paths/v1）+ 构建选项
// ---------------------------------------------------------------------------
//
// 生产 measured 桥只生成 identity baseline G（对角线 1、非对角 0）——单点 5x5 G
// 没有任何入口（g-singlepoint REPORT §4.3 "G 注入 NOT_WIRED"）。本处新增**显式文件入口**：
// 求解器（T51/T52）输出的 dense m x m x l_g 系数按 tspi-cal-g-paths/v1 落成 JSON，
// measured 构建时替换 identity。**只有 MeasuredBuildOptions.experiment 显式开启才会用到**。

/// G 注入文件非法/不一致。
pub const E_MEASURED_G_PATHS: &str = "E_WIRING_MEASURED_G_PATHS";
/// G 注入文件 schema。
pub const G_PATHS_SCHEMA: &str = "tspi-cal-g-paths/v1";
/// 实验允许覆盖的 layout（仅 4.1.2 五执行器）。
pub const EXPERIMENT_LAYOUT_4_1_2: &str = "4.1.2";
/// 实验 G 支撑上限（与 TAPS_NORMAL_MAX 一致；超档需 extended 准入，本实验不支持）。
pub const EXPERIMENT_L_G_MAX: u32 = 4096;

/// 求解器输出的 G 系数（dense m x m x l_g；行=out、列=in）。
#[derive(Clone, Debug)]
pub struct InjectedGPaths {
    /// 源文件路径（证据）。
    pub source_path: PathBuf,
    /// 源文件 sha256。
    pub source_sha256: String,
    /// G 声明支撑 L_G。
    pub l_g: u32,
    /// baseline 整数延迟 D（必须 < L_G）。
    pub low_band_d: u32,
    /// dense 参与者顺序（必须等于 plan.participants）。
    pub participants: Vec<String>,
    /// M*M 条路径（dense 全覆盖）。
    pub paths: Vec<GPathInput>,
    /// 求解器元信息（原样透传到证据；不参与判定）。
    pub solver_note: Option<String>,
}

/// measured 构建选项（**Default 全关 = 历史生产行为不变**）。
#[derive(Clone, Default, Debug)]
pub struct MeasuredBuildOptions {
    /// 实验模式（默认 OFF）。
    pub experiment: ExperimentMode,
    /// layout 覆盖（仅 `experiment.single_point` 下允许；目前只接受 "4.1.2"）。
    pub layout_override: Option<String>,
    /// G 注入（None = 历史 identity baseline）。
    pub g_paths: Option<InjectedGPaths>,
}

impl MeasuredBuildOptions {
    /// 生产默认（实验全关）。
    pub fn production() -> Self {
        Self::default()
    }

    /// 单点实验（G 注入 + 4.1.2 允许空 stereo_mask + 允许 INCOMPLETE artifact 编译）。
    pub fn single_point(g_paths: InjectedGPaths) -> Self {
        Self {
            experiment: ExperimentMode::SINGLE_POINT,
            layout_override: Some(EXPERIMENT_LAYOUT_4_1_2.to_string()),
            g_paths: Some(g_paths),
        }
    }
}

/// 载入 tspi-cal-g-paths/v1 文件（严格解析；任何不一致稳定报错，不静默退化）。
pub fn load_g_paths_file(path: &Path) -> Result<InjectedGPaths, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "{E_MEASURED_G_PATHS}: 读 g_paths {} 失败：{error}",
            path.display()
        )
    })?;
    let source_sha256 = crate::job::sha256_hex(text.as_bytes());
    let document = json_parse(&text).map_err(|error| {
        format!(
            "{E_MEASURED_G_PATHS}: g_paths JSON 解析失败（offset {}）：{error}",
            error.offset
        )
    })?;
    let schema = spec_string(&document, "schema")?;
    if schema != G_PATHS_SCHEMA {
        return Err(format!(
            "{E_MEASURED_G_PATHS}: g_paths schema 必须为 {G_PATHS_SCHEMA}，得到 {schema:?}"
        ));
    }
    let l_g = spec_u64(&document, "l_g")? as u32;
    if l_g == 0 || l_g > EXPERIMENT_L_G_MAX {
        return Err(format!(
            "{E_MEASURED_G_PATHS}: l_g={l_g} 越界（1..={EXPERIMENT_L_G_MAX}）"
        ));
    }
    let low_band_d = document
        .get("low_band_d")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    if low_band_d >= l_g {
        return Err(format!(
            "{E_MEASURED_G_PATHS}: low_band_d={low_band_d} 必须 < l_g={l_g}"
        ));
    }
    let participants: Vec<String> = spec_array(&document, "participants")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(|text| text.to_string())
                .ok_or_else(|| format!("{E_MEASURED_G_PATHS}: participants 含非字符串"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if participants.is_empty() {
        return Err(format!("{E_MEASURED_G_PATHS}: participants 为空"));
    }
    let m = participants.len();
    if m > khaen_core::rt::MAX_LOW_BAND_PARTICIPANTS {
        return Err(format!(
            "{E_MEASURED_G_PATHS}: participants {m} 超过 MAX_LOW_BAND_PARTICIPANTS"
        ));
    }
    let entries = spec_array(&document, "paths")?;
    if entries.len() != m * m {
        return Err(format!(
            "{E_MEASURED_G_PATHS}: paths {} 条 != dense M*M {}（M={m}；dense 必须全覆盖）",
            entries.len(),
            m * m
        ));
    }
    let mut paths: Vec<GPathInput> = Vec::with_capacity(m * m);
    let mut seen = vec![false; m * m];
    for entry in entries {
        let out = spec_u64(entry, "out")? as u16;
        let in_dense = spec_u64(entry, "in")? as u16;
        if out as usize >= m || in_dense as usize >= m {
            return Err(format!(
                "{E_MEASURED_G_PATHS}: path out={out}/in={in_dense} 越界（M={m}）"
            ));
        }
        let key = out as usize * m + in_dense as usize;
        if seen[key] {
            return Err(format!(
                "{E_MEASURED_G_PATHS}: path out={out}/in={in_dense} 重复"
            ));
        }
        seen[key] = true;
        let coeffs: Vec<f64> = spec_array(entry, "coeffs")?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .ok_or_else(|| format!("{E_MEASURED_G_PATHS}: g[{out}][{in_dense}] 含非数值"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if coeffs.len() as u32 != l_g {
            return Err(format!(
                "{E_MEASURED_G_PATHS}: g[{out}][{in_dense}] 系数 {} 个 != l_g={l_g}",
                coeffs.len()
            ));
        }
        let all_zero = coeffs.iter().all(|value| *value == 0.0);
        let mut path = GPathInput::new(
            out,
            in_dense,
            coeffs,
            out == in_dense,
            all_zero,
            String::new(),
        )
        .map_err(|error| format!("{E_MEASURED_G_PATHS}: g[{out}][{in_dense}] 非法：{error}"))?
        .with_source(in_dense);
        path.coeff_sha256 = path
            .coeffs_f32_sha256()
            .map_err(|error| format!("{E_MEASURED_G_PATHS}: g[{out}][{in_dense}] sha：{error}"))?;
        paths.push(path);
    }
    let solver_note = document
        .get("solver_note")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    Ok(InjectedGPaths {
        source_path: path.to_path_buf(),
        source_sha256,
        l_g,
        low_band_d,
        participants,
        paths,
        solver_note,
    })
}

fn parse_position(value: &JsonValue, base_dir: &Path) -> Result<MeasuredPositionSpec, String> {
    let label = spec_string(value, "label")?;
    if label.trim() != label || label.is_empty() || label.chars().count() > 64 {
        return spec_err(format!(
            "测点 label {label:?} 非法（非空、trim 后不变、<=64 字符）"
        ));
    }
    let mic_orientation = spec_string(value, "mic_orientation")?;
    let weight = match value.get("weight") {
        Some(weight) => weight
            .as_f64()
            .ok_or_else(|| format!("{E_MEASURED_SPEC}: 测点 {label} weight 必须是数字"))?,
        None => 1.0,
    };
    let mut channels = Vec::new();
    for channel in spec_array(value, "channels")? {
        let name = spec_string(channel, "name")?;
        if name.trim().is_empty() {
            return spec_err(format!("测点 {label} 有匿名通道（name 为空）"));
        }
        if channels
            .iter()
            .any(|existing: &MeasuredChannelFile| existing.name == name)
        {
            return spec_err(format!("测点 {label} 通道 {name} 重复"));
        }
        let analysis = resolve_relative(base_dir, &spec_string(channel, "analysis")?);
        let diag = channel
            .get("diag")
            .and_then(|value| value.as_str())
            .map(|value| resolve_relative(base_dir, value));
        channels.push(MeasuredChannelFile {
            name,
            analysis,
            diag,
        });
    }
    if channels.is_empty() {
        return spec_err(format!("测点 {label} channels 为空"));
    }
    Ok(MeasuredPositionSpec {
        label,
        mic_orientation,
        weight,
        channels,
    })
}

fn resolve_relative(base_dir: &Path, path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

// ---------------------------------------------------------------------------
// analysis / diag 解析
// ---------------------------------------------------------------------------

fn parse_analysis_file(path: &Path) -> Result<ParsedAnalysis, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "{E_MEASURED_FILE}: 读 analysis {} 失败：{error}",
            path.display()
        )
    })?;
    let sha256 = sha256_hex(&bytes);
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("{E_MEASURED_FILE}: analysis 非 UTF-8：{error}"))?;
    let document = json_parse(&text).map_err(|error| {
        format!(
            "{E_MEASURED_FILE}: analysis JSON 解析失败 {}（offset {}）：{error}",
            path.display(),
            error.offset
        )
    })?;
    let schema = document
        .get("schema")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if schema != "tspi-channel-measurement/v1" {
        return Err(format!(
            "{E_MEASURED_FILE}: {} schema={schema:?} 不是 tspi-channel-measurement/v1",
            path.display()
        ));
    }
    let channel = document
        .get("channel")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| format!("{E_MEASURED_FILE}: 缺 channel：{}", path.display()))?;
    let meta = document
        .get("meta")
        .ok_or_else(|| format!("{E_MEASURED_FILE}: 缺 meta：{}", path.display()))?;
    let sample_rate_hz =
        meta.get("sample_rate")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| format!("{E_MEASURED_FILE}: 缺 meta.sample_rate"))? as u32;
    let sweep_start_hz = meta
        .get("sweep_start_hz")
        .and_then(|value| value.as_f64())
        .unwrap_or(20.0);
    let sweep_end_hz = meta
        .get("sweep_end_hz")
        .and_then(|value| value.as_f64())
        .unwrap_or(20_000.0);
    let delay_samples = document
        .get("delay_samples")
        .and_then(|value| value.as_f64())
        .ok_or_else(|| format!("{E_MEASURED_FILE}: 缺 delay_samples"))?;
    let snr_band_db = document
        .get("snr_band_db")
        .and_then(|value| value.as_f64())
        .unwrap_or(f64::NAN);
    let polarity_confidence = document
        .get("polarity_confidence")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let fr = document
        .get("fr")
        .and_then(|value| value.as_array())
        .ok_or_else(|| format!("{E_MEASURED_FILE}: 缺 fr 数组：{}", path.display()))?;
    if fr.len() < 8 {
        return Err(format!(
            "{E_MEASURED_DATA}: {} fr 只有 {} 点（<8）",
            path.display(),
            fr.len()
        ));
    }
    let mut points = Vec::with_capacity(fr.len());
    let mut phases_deg = Vec::with_capacity(fr.len());
    let mut previous = f64::NEG_INFINITY;
    for point in fr {
        let freq_hz = point
            .get("freq_hz")
            .and_then(|value| value.as_f64())
            .ok_or_else(|| format!("{E_MEASURED_FILE}: fr[].freq_hz 缺失"))?;
        let db = point
            .get("db")
            .and_then(|value| value.as_f64())
            .ok_or_else(|| format!("{E_MEASURED_FILE}: fr[].db 缺失"))?;
        let phase_deg = point
            .get("phase_deg")
            .and_then(|value| value.as_f64())
            .ok_or_else(|| format!("{E_MEASURED_FILE}: fr[].phase_deg 缺失"))?;
        if !(freq_hz.is_finite() && freq_hz > 0.0 && freq_hz >= previous) {
            return Err(format!(
                "{E_MEASURED_DATA}: {} fr 频率非单调/非法（{freq_hz}）",
                path.display()
            ));
        }
        if !(db.is_finite() && phase_deg.is_finite()) {
            return Err(format!(
                "{E_MEASURED_DATA}: {} fr 幅度/相位非有限",
                path.display()
            ));
        }
        previous = freq_hz;
        points.push(CurvePoint::new(freq_hz, db));
        phases_deg.push(phase_deg);
    }
    Ok(ParsedAnalysis {
        channel,
        sample_rate_hz,
        sweep_start_hz,
        sweep_end_hz,
        points,
        phases_deg,
        delay_samples,
        snr_band_db,
        polarity_confidence,
        sha256,
    })
}

/// diag 文件给出的实测到达延迟（actual_start - nominal_start，samples）。
fn parse_diag_delay(path: &Path) -> Result<Option<f64>, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "{E_MEASURED_FILE}: 读 diag {} 失败：{error}",
            path.display()
        )
    })?;
    let document = json_parse(&text).map_err(|error| {
        format!(
            "{E_MEASURED_FILE}: diag JSON 解析失败 {}（offset {}）：{error}",
            path.display(),
            error.offset
        )
    })?;
    let nominal = document
        .get("nominal_start")
        .and_then(|value| value.as_f64());
    let actual = document
        .get("actual_start")
        .and_then(|value| value.as_f64());
    match (nominal, actual) {
        (Some(nominal), Some(actual)) => Ok(Some(actual - nominal)),
        _ => Ok(None),
    }
}

/// 从 analysis + diag 重建 T65 复响应点（`X=|X|e^{+jφ}`，含实测到达延迟）。
///
/// 推导：旧工具 `h = Y·conj(X)/|X|²` 在按互相关粗对齐的段上计算，其相位
/// `φ_tool = arg(R) - 2πf·frac/fs`（不含整数粗延迟）；T65 `aligned_segment` 口径要求
/// 相位含传播延迟 `+2πf·τ/fs`。取 τ = 实测 onset（samples），
/// `φ_t65 = 2πf·τ/fs - φ_tool`（共同 pre 偏移在通道间抵消）。
fn build_phase_points(
    analysis: &ParsedAnalysis,
    delay_samples: f64,
    fs: u32,
) -> Result<Vec<PhaseFitPoint>, String> {
    let fs_f = fs as f64;
    let mut points = Vec::with_capacity(analysis.points.len());
    for (curve, phase_deg) in analysis.points.iter().zip(analysis.phases_deg.iter()) {
        let freq = curve.freq_hz;
        let mag = 10f64.powf(curve.mag_db / 20.0);
        if !mag.is_finite() || mag <= 0.0 {
            continue;
        }
        let rotation = 2.0 * std::f64::consts::PI * freq * delay_samples / fs_f;
        let phase = rotation - phase_deg.to_radians();
        points.push(PhaseFitPoint::new(
            freq,
            mag * phase.cos(),
            mag * phase.sin(),
        ));
    }
    if points.len() < 8 {
        return Err(format!(
            "{E_MEASURED_DATA}: 复响应有效点 {} < 8（延迟重建后）",
            points.len()
        ));
    }
    Ok(points)
}

// ---------------------------------------------------------------------------
// 构造 FitWiringDesign
// ---------------------------------------------------------------------------

/// 由 measured spec 构造 FitWiringDesign（真实文件读取；失败不退化）。
///
/// 生产入口：等价于 `build_measured_design_opts(spec, &MeasuredBuildOptions::production())`，
/// 行为与历史完全一致（含 4.1.2 非 stereo 时 stereo_mask 拒绝）。
pub fn build_measured_design(spec: &MeasuredDesignSpec) -> Result<MeasuredDesign, String> {
    build_measured_design_opts(spec, &MeasuredBuildOptions::production())
}

// ---------------------------------------------------------------------------
// 多点构建内部数据（build / summary / 证据共用）
// ---------------------------------------------------------------------------

/// 单通道测量 + 实际用于相位重建的延迟来源（内部）。
#[derive(Clone, Debug)]
struct ChannelMeasurement {
    name: String,
    analysis: ParsedAnalysis,
    delay_samples: f64,
    delay_source: &'static str,
    delay_delta: f64,
}

/// 一个测点的全部通道测量（design 或独立 validation；内部）。
#[derive(Clone, Debug)]
struct PositionMeasurement {
    label: String,
    mic_orientation: String,
    weight: f64,
    design: bool,
    channels: Vec<ChannelMeasurement>,
}

impl PositionMeasurement {
    fn role_str(&self) -> &'static str {
        if self.design {
            "design"
        } else {
            "validation"
        }
    }
}

/// spec 内存一致性校验（文件解析已做一次；这里覆盖调用方手工构造的 spec）。
fn validate_spec_for_build(spec: &MeasuredDesignSpec) -> Result<(), String> {
    if spec.design_positions.is_empty() {
        return Err(format!(
            "{E_MEASURED_SPEC}: design_positions 为空（至少 1 个 design 测点）"
        ));
    }
    let proxy = spec.validation_positions.is_empty();
    if proxy {
        if spec.validation != VALIDATION_PROXY_SAME_AS_DESIGN {
            return Err(format!(
                "{E_MEASURED_SPEC}: 无 validation_positions[] 时 validation 必须为 {VALIDATION_PROXY_SAME_AS_DESIGN}（得到 {:?}）",
                spec.validation
            ));
        }
        if spec.design_positions.len() != 1 {
            return Err(format!(
                "{E_MEASURED_SPEC}: {} 个 design 测点但没有独立 validation_positions[]（多点不得用单点 proxy 冒充 holdout）",
                spec.design_positions.len()
            ));
        }
    } else if spec.validation != VALIDATION_INDEPENDENT_POSITIONS {
        return Err(format!(
            "{E_MEASURED_SPEC}: 有独立 validation_positions[] 时 validation 必须为 {VALIDATION_INDEPENDENT_POSITIONS}（得到 {:?}）",
            spec.validation
        ));
    }
    let mut labels: Vec<&str> = Vec::new();
    for position in spec
        .design_positions
        .iter()
        .chain(spec.validation_positions.iter())
    {
        if labels.contains(&position.label.as_str()) {
            return Err(format!(
                "{E_MEASURED_SPEC}: 测点 label {:?} 重复（design/validation 全 spec 唯一）",
                position.label
            ));
        }
        labels.push(position.label.as_str());
        if !(position.weight.is_finite() && position.weight > 0.0) {
            return Err(format!(
                "{E_MEASURED_SPEC}: 测点 {} weight={} 非法（须有限且 >0）",
                position.label, position.weight
            ));
        }
    }
    Ok(())
}

/// 读取一个测点的全部通道分析文件（通道集合必须先与 canonical 一致）。
fn read_position_measurements(
    position: &MeasuredPositionSpec,
    canonical_channels: &[String],
    fs: u32,
    design: bool,
) -> Result<PositionMeasurement, String> {
    if position.channels.len() != canonical_channels.len() {
        return Err(format!(
            "{E_MEASURED_DATA}: 测点 {} 通道数 {} != 首个 design 测点 {}（多点 spec 每个位置必须覆盖同一组通道）",
            position.label,
            position.channels.len(),
            canonical_channels.len()
        ));
    }
    let mut channels = Vec::with_capacity(canonical_channels.len());
    for name in canonical_channels {
        let channel = position
            .channels
            .iter()
            .find(|channel| &channel.name == name)
            .ok_or_else(|| {
                format!(
                    "{E_MEASURED_DATA}: 测点 {} 缺通道 {name}（多点 spec 每个位置必须覆盖同一组通道）",
                    position.label
                )
            })?;
        let analysis = parse_analysis_file(&channel.analysis)?;
        if analysis.sample_rate_hz != fs {
            return Err(format!(
                "{E_MEASURED_DATA}: {} / {} analysis fs={} != spec fs={fs}",
                position.label, channel.name, analysis.sample_rate_hz
            ));
        }
        let diag_delay = match channel.diag.as_ref() {
            Some(path) => parse_diag_delay(path)?,
            None => None,
        };
        let (delay_samples, delay_source, delay_delta) = match diag_delay {
            Some(diag_delay) => (
                diag_delay,
                DELAY_SOURCE_DIAG_ONSET,
                diag_delay - analysis.delay_samples,
            ),
            None => (analysis.delay_samples, DELAY_SOURCE_ANALYSIS, 0.0),
        };
        channels.push(ChannelMeasurement {
            name: channel.name.clone(),
            analysis,
            delay_samples,
            delay_source,
            delay_delta,
        });
    }
    Ok(PositionMeasurement {
        label: position.label.clone(),
        mic_orientation: position.mic_orientation.clone(),
        weight: position.weight,
        design,
        channels,
    })
}

/// 全部真实测量点的公共频带 [lo, hi]（含独立 validation；T64 网格取所有曲线交集）。
fn common_measured_band(positions: &[PositionMeasurement]) -> Result<(f64, f64), String> {
    let mut lo = 0.0_f64;
    let mut hi = f64::INFINITY;
    for position in positions {
        for channel in &position.channels {
            lo = lo.max(channel.analysis.sweep_start_hz.max(20.0));
            hi = hi.min(channel.analysis.sweep_end_hz.min(20_000.0));
        }
    }
    if !(hi > lo * 2.0) {
        return Err(format!(
            "{E_MEASURED_DATA}: 通道公共频带过窄（{lo}–{hi} Hz）；请检查 sweep_start/end"
        ));
    }
    Ok((lo, hi))
}

/// 测量文件内容摘要（legacy 单点口径保持历史字节：只含 channel:sha；多点含 position）。
fn measured_files_digest(
    spec: &MeasuredDesignSpec,
    positions: &[PositionMeasurement],
    canonical_channels: &[String],
) -> String {
    let legacy_single = spec.validation_positions.is_empty() && spec.design_positions.len() == 1;
    let mut labels: Vec<String> = Vec::new();
    for position in positions {
        for (index, name) in canonical_channels.iter().enumerate() {
            let sha = &position.channels[index].analysis.sha256;
            labels.push(if legacy_single {
                format!("{name}:{sha}")
            } else {
                format!("{}:{name}:{sha}", position.label)
            });
        }
    }
    labels.sort();
    sha256_hex(labels.join("\n").as_bytes())
}

/// 在曲线点上做对数频率线性插值（越界返回 None）。
fn interp_curve_db(points: &[CurvePoint], freq_hz: f64) -> Option<f64> {
    if points.len() < 2 || freq_hz <= 0.0 {
        return None;
    }
    if freq_hz < points[0].freq_hz || freq_hz > points[points.len() - 1].freq_hz {
        return None;
    }
    let mut low = 0usize;
    let mut high = points.len() - 1;
    while high - low > 1 {
        let mid = (low + high) / 2;
        if points[mid].freq_hz <= freq_hz {
            low = mid;
        } else {
            high = mid;
        }
    }
    let f0 = points[low].freq_hz.max(f64::MIN_POSITIVE);
    let f1 = points[high].freq_hz.max(f64::MIN_POSITIVE);
    let ratio = (freq_hz / f0).ln() / (f1 / f0).ln();
    let t = ratio.clamp(0.0, 1.0);
    Some(points[low].mag_db + t * (points[high].mag_db - points[low].mag_db))
}

/// 座位间 spread（design 位置逐频点 max-min；单位 dB）。
///
/// 输入是 T64 同口径的 PositionCurve（position/channel/points）。返回每个通道的
/// channel/design_positions/grid_points/max_spread_db/rms_spread_db/worst_freq_hz；
/// 少于 2 个 design 位置时为空（单点无座位间 spread 可言）。
fn seat_spread_report(
    curves: &[PositionCurve],
    design_labels: &[String],
) -> Vec<khaen_compat::ws::JsonValue> {
    use khaen_compat::ws::{json_object, JsonValue};
    if design_labels.len() < 2 {
        return Vec::new();
    }
    let mut channels: Vec<&str> = curves.iter().map(|curve| curve.channel.as_str()).collect();
    channels.sort_unstable();
    channels.dedup();
    let mut out = Vec::with_capacity(channels.len());
    let mut channel_index = 0usize;
    while channel_index < channels.len() {
        let channel = channels[channel_index];
        channel_index += 1;
        let mut positions: Vec<(&str, &Vec<CurvePoint>)> = Vec::new();
        let mut lo = f64::NEG_INFINITY;
        let mut hi = f64::INFINITY;
        for label in design_labels {
            if let Some(curve) = curves
                .iter()
                .find(|curve| &curve.position == label && curve.channel == channel)
            {
                if curve.points.len() < 2 {
                    continue;
                }
                lo = lo.max(curve.points[0].freq_hz);
                hi = hi.min(curve.points[curve.points.len() - 1].freq_hz);
                positions.push((curve.position.as_str(), &curve.points));
            }
        }
        if positions.len() < 2 || !(hi > lo * 1.01) {
            continue;
        }
        let grid_points = ((hi / lo).log2() * 24.0).ceil() as usize + 1;
        let mut max_spread = 0.0_f64;
        let mut worst_freq = lo;
        let mut sum_sq = 0.0_f64;
        let mut used = 0usize;
        let mut grid_index = 0usize;
        while grid_index < grid_points {
            let ratio = grid_index as f64 / (grid_points.max(2) - 1) as f64;
            grid_index += 1;
            let freq = lo * (hi / lo).powf(ratio);
            let mut low = f64::INFINITY;
            let mut high = f64::NEG_INFINITY;
            let mut complete = true;
            for (_, points) in &positions {
                match interp_curve_db(points, freq) {
                    Some(db) => {
                        low = low.min(db);
                        high = high.max(db);
                    }
                    None => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete {
                continue;
            }
            let spread = high - low;
            sum_sq += spread * spread;
            used += 1;
            if spread > max_spread {
                max_spread = spread;
                worst_freq = freq;
            }
        }
        let rms_spread = if used == 0 {
            0.0
        } else {
            (sum_sq / used as f64).sqrt()
        };
        out.push(json_object(vec![
            ("channel", JsonValue::String(channel.to_string())),
            ("design_positions", JsonValue::UInt(positions.len() as u64)),
            ("grid_points", JsonValue::UInt(used as u64)),
            ("max_spread_db", JsonValue::Number(max_spread)),
            ("rms_spread_db", JsonValue::Number(rms_spread)),
            ("worst_freq_hz", JsonValue::Number(worst_freq)),
        ]));
    }
    out
}

/// 同 [build_measured_design]，但允许显式开启实验旁路（layout 覆盖 / G 注入）。
///
/// **默认选项 = 生产行为**：`experiment` 全关时每个分支都与历史一致；只有显式
/// [ExperimentMode::single_point] 才会 (a) 允许非 stereo layout 传空 stereo_mask、
/// (b) 用注入 G 替换 identity baseline。
pub fn build_measured_design_opts(
    spec: &MeasuredDesignSpec,
    opts: &MeasuredBuildOptions,
) -> Result<MeasuredDesign, String> {
    validate_spec_for_build(spec)?;
    opts.experiment
        .validate()
        .map_err(|error| format!("{E_MEASURED_SPEC}: experiment 组合非法：{error}"))?;
    // 0) 生效 layout：生产恒为 spec.layout；只有单点实验允许显式覆盖到 4.1.2。
    let layout = match opts.layout_override.as_deref() {
        Some(value) => {
            if !opts.experiment.single_point {
                return Err(format!(
                    "{E_MEASURED_SPEC}: layout_override={value:?} 只允许在实验模式（single_point）下使用"
                ));
            }
            if value != EXPERIMENT_LAYOUT_4_1_2 {
                return Err(format!(
                    "{E_MEASURED_SPEC}: layout_override 只接受 {EXPERIMENT_LAYOUT_4_1_2:?}（实验旁路；得到 {value:?}）"
                ));
            }
            value.to_string()
        }
        None => spec.layout.clone(),
    };
    let stereo_strict = layout == "stereo-2.0";
    let fs = spec.sample_rate_hz;
    let proxy = spec.validation_positions.is_empty();
    // 1) 读全部真实测点（design + 独立 validation；legacy proxy 无独立文件）。
    let canonical_channels: Vec<String> = spec
        .primary_design_position()
        .channels
        .iter()
        .map(|channel| channel.name.clone())
        .collect();
    if canonical_channels.is_empty() {
        return Err(format!(
            "{E_MEASURED_SPEC}: design 测点 {} channels 为空",
            spec.primary_design_position().label
        ));
    }
    let mut measured_positions: Vec<PositionMeasurement> = Vec::new();
    for position in &spec.design_positions {
        measured_positions.push(read_position_measurements(
            position,
            &canonical_channels,
            fs,
            true,
        )?);
    }
    for position in &spec.validation_positions {
        measured_positions.push(read_position_measurements(
            position,
            &canonical_channels,
            fs,
            false,
        )?);
    }
    // 2) 公共频带 + 频带裁剪后的曲线（T64 输入；必须覆盖公共拟合区间）。
    //    独立 validation 曲线也进入 T64 网格（只用于报告/预测评估，不参与聚合拟合）。
    let (lo, hi) = common_measured_band(&measured_positions)?;
    let mut curves = Vec::new();
    for position in &measured_positions {
        for (index, name) in canonical_channels.iter().enumerate() {
            let points: Vec<CurvePoint> = position.channels[index]
                .analysis
                .points
                .iter()
                .filter(|point| point.freq_hz >= lo && point.freq_hz <= hi)
                .copied()
                .collect();
            if points.len() < 8 {
                return Err(format!(
                    "{E_MEASURED_DATA}: {}/{} 在 {lo}–{hi} Hz 只有 {} 曲线点（<8）",
                    position.label,
                    name,
                    points.len()
                ));
            }
            curves.push(PositionCurve::new(
                position.label.clone(),
                name.clone(),
                points,
            ));
        }
    }
    if proxy {
        // legacy 单点：同一真实测量复制成 V1_proxy（显式标注 proxy，不是独立 holdout）。
        let design = &measured_positions[0];
        for (index, name) in canonical_channels.iter().enumerate() {
            let points: Vec<CurvePoint> = design.channels[index]
                .analysis
                .points
                .iter()
                .filter(|point| point.freq_hz >= lo && point.freq_hz <= hi)
                .copied()
                .collect();
            curves.push(PositionCurve::new(
                VALIDATION_PROXY_POSITION_LABEL,
                name.clone(),
                points,
            ));
        }
    }
    // 3) 复响应（只给 design 测点；T65 明令拒绝 validation 测点进入相位拟合）。
    let phase_lo = 80.0_f64.max(lo);
    let phase_hi = 8_000.0_f64.min(hi).min(fs as f64 * 0.45);
    let mut phase_responses = Vec::new();
    for position in measured_positions.iter().filter(|position| position.design) {
        for channel in &position.channels {
            let points = build_phase_points(&channel.analysis, channel.delay_samples, fs)?;
            phase_responses.push(
                DePhasedResponse::from_aligned_segment(
                    position.label.clone(),
                    channel.name.clone(),
                    fs,
                    points,
                    CommonD0::zero(),
                )
                .map_err(|error| {
                    format!(
                        "{E_MEASURED_DATA}: {}/{} 复响应构造失败：{error}",
                        position.label, channel.name
                    )
                })?,
            );
        }
    }
    let mut phase_channel_bands = Vec::new();
    let mut phase_channel_indices = Vec::new();
    for (index, name) in canonical_channels.iter().enumerate() {
        phase_channel_bands.push(PhaseChannelBand::new(name.clone(), [phase_lo, phase_hi]));
        let dac = spec
            .dense_to_dac
            .get(index)
            .copied()
            .ok_or_else(|| format!("{E_MEASURED_DATA}: dense_to_dac 缺第 {index} 个通道"))?;
        phase_channel_indices.push(PhaseChannelIndex::new(name.clone(), dac as u32));
    }
    // 4) T63 plan：design 位置 + 独立 validation 位置（legacy = V1_proxy 单点代理）。
    let participant_names: Vec<String> = spec
        .dense_to_dac
        .iter()
        .enumerate()
        .map(|(index, _)| {
            spec.roles
                .iter()
                .find(|role| role.role_index == index as u16)
                .map(|role| role.role_id.clone())
                .unwrap_or_else(|| format!("CH{index}"))
        })
        .collect();
    let mut plan_positions = Vec::new();
    for position in &measured_positions {
        let built = if position.design {
            MultipointPosition::design(position.label.clone(), position.weight)
        } else {
            MultipointPosition::validation(position.label.clone(), position.weight)
        }
        .map_err(|error| {
            format!(
                "{E_MEASURED_SPEC}: {} 测点 {} 非法：{error}",
                position.role_str(),
                position.label
            )
        })?
        .with_mic_orientation(position.mic_orientation.clone());
        plan_positions.push(built);
    }
    if proxy {
        plan_positions.push(
            MultipointPosition::validation(VALIDATION_PROXY_POSITION_LABEL, 1.0)
                .map_err(|error| format!("{E_MEASURED_SPEC}: validation 代理测点非法：{error}"))?
                .with_mic_orientation("90deg"),
        );
    }
    let plan = MultipointPlan::new(
        spec.session_id.clone(),
        layout.clone(),
        fs,
        participant_names,
        CommonD0::zero(),
        "aligned_segment",
        plan_positions,
    )
    .map_err(|error| format!("{E_MEASURED_SPEC}: MultipointPlan 非法：{error}"))?;
    // 5) 目标（T11 内置 flat/tilt10；未知拒绝）。
    let (target, target_id) = match spec.target.as_str() {
        "flat" => (
            builtin_target(BuiltinTarget::Flat, &BuiltinOptions::new(fs as f64))
                .map_err(|error| format!("{E_MEASURED_SPEC}: builtin flat 失败：{error}"))?,
            "flat",
        ),
        "tilt10" => (
            builtin_target(BuiltinTarget::Tilt10, &BuiltinOptions::new(fs as f64))
                .map_err(|error| format!("{E_MEASURED_SPEC}: builtin tilt10 失败：{error}"))?,
            "tilt10",
        ),
        other => {
            return Err(format!(
                "{E_MEASURED_SPEC}: target={other:?} 只接受 flat|tilt10（F-15）"
            ))
        }
    };
    // 6) fit 选项：实测 pilot 不作为 T6 质量门签收，故不注入 T63 design 质量报告；
    //    target FIR 关闭（P=identity，D_i 由 T65 承载，避免 F-16 双扣）。
    let mut peq_options = PeqFitOptions::default();
    peq_options.target_fir.enabled = false;
    peq_options.require_design_quality = false;
    peq_options.require_t6_minimums = false;
    if let Some(boost) = spec.boost_cap_db {
        peq_options.boost_cap_db = boost;
    }
    if let Some(stages) = spec.max_total_stages {
        peq_options.max_total_stages = stages;
    }
    if let Some(trusted) = spec.low_band_trusted {
        peq_options.low_band_trusted = trusted;
    }
    let mut phase_options = PhaseFitOptions::default();
    phase_options.reference_position = Some(spec.primary_design_position().label.clone());
    if opts.experiment.is_active() {
        // 实验旁路：pilot 没有 T63 design scope 独立质量报告（design_quality=None），
        // T65 默认 require_design_quality=true 会以 E_PHF_QUALITY_MISSING **直接报错**
        // （不是可绕过的 checks 门）。只关掉"质量报告必需"，T65 checks 仍照算；
        // 实验模式下 checks 未全过会如实写进 gates_bypassed，绝不写成 PASS。
        phase_options.require_design_quality = false;
        // 实验旁路：真实单点数据到达延迟 667–1483 smp（13.9–30.9 ms），且复响应频带只有
        // 80–130 Hz（窄带 => 匹配滤波主瓣很宽）。默认 ±50 ms 搜索窗会把峰值判到边界并
        // 以 E_PHF_DELAY_SEARCH 硬报错（不是 checks 门、无法在 FitWiring 层绕过）。
        // 显式放宽到 ±200 ms（= 契约延迟上限，仍是"不猜"语义：边界命中照样报错）。
        phase_options.search_range_ms = crate::phase_fit::MAX_CONTRACT_DELAY_MS;
        // 实验旁路：真实单点带只有 80–130 Hz（带宽 50 Hz ⇒ 匹配滤波包络主瓣 ~20 ms，
        // 延迟估计本身多值；真实房间多径又破坏谱线性），T65 的置信度门按设计
        // 以 E_PHF_DELAY_LOW_CONFIDENCE / E_PHF_POLARITY_LOW_CONFIDENCE **硬报错**
        // （拒绝猜，不是 checks 门）。实验模式显式把两个置信度下限置 0：
        // 只是"允许低置信度结果进入非交付 plan"，实际置信度仍逐通道写进 T65 报告，
        // 且整个产物是 EXPERIMENT/NON_ACCEPTABLE。
        phase_options.delay_confidence_min = 0.0;
        phase_options.polarity_margin_min = 0.0;
    }
    // 7) T57 mapping（T54/T57 overlay 未解析：hash 绑定到真实测量内容，显式 provenance）。
    let measurement_digest = measured_files_digest(spec, &measured_positions, &canonical_channels);
    let mapping = MappingBinding::new(
        spec.layout.clone(),
        measurement_digest.clone(),
        sha256_hex(format!("tspi-measured-binding/v1\n{measurement_digest}").as_bytes()),
        sha256_hex(format!("tspi-measured-roles/v1\n{measurement_digest}").as_bytes()),
        sha256_hex(format!("tspi-measured-participants/v1\n{measurement_digest}").as_bytes()),
        spec.container_channels,
        spec.dense_to_dac.clone(),
        spec.roles.clone(),
        spec.unused_outputs.clone(),
        stereo_strict,
        // 生产语义不变：非 stereo layout 仍传 participants => MappingBinding 如实拒绝
        // （E_CA_MAPPING: stereo_mask 必须为空）。只有单点实验才允许空 mask。
        if stereo_strict {
            plan.participants.clone()
        } else if opts.experiment.single_point {
            Vec::new()
        } else {
            plan.participants.clone()
        },
    )
    .map_err(|error| format!("{E_MEASURED_SPEC}: MappingBinding 非法：{error}"))?;
    // 8) T54 modal artifact：pilot 数据无法产生 READY artifact（T53 需 T6 多点 holdout）；
    //    这里显式 INCOMPLETE + identity baseline G，T66 compile 会以 E_CA_ARTIFACT_STATUS
    //    拒绝（不伪造 READY）。
    let artifact = ArtifactBinding::from_parts(
        sha256_hex(format!("tspi-measured-no-artifact/v1\n{measurement_digest}").as_bytes()),
        sha256_hex(format!("tspi-measured-no-manifest/v1\n{measurement_digest}").as_bytes()),
        sha256_hex(format!("tspi-measured-no-wav/v1\n{measurement_digest}").as_bytes()),
        sha256_hex(format!("tspi-measured-no-solution/v1\n{measurement_digest}").as_bytes()),
        measurement_digest.clone(),
        target.target_sha256().to_string(),
        sha256_hex(format!("tspi-measured-no-baseline/v1\n{measurement_digest}").as_bytes()),
        "INCOMPLETE",
    );
    let m = spec.dense_to_dac.len();
    // 8b) 编译支撑：G 注入时以注入文件为准（l_g/low_band_d 自洽）；否则沿用 spec.compile。
    let mut compile = spec.compile;
    let g_source;
    let g_paths: Vec<GPathInput> = match opts.g_paths.as_ref() {
        Some(injected) => {
            if injected.participants.len() != m {
                return Err(format!(
                    "{E_MEASURED_G_PATHS}: 注入 participants {} != dense M={m}",
                    injected.participants.len()
                ));
            }
            if injected.participants != plan.participants {
                return Err(format!(
                    "{E_MEASURED_G_PATHS}: 注入 participants {:?} != plan.participants {:?}（dense 顺序必须一致）",
                    injected.participants, plan.participants
                ));
            }
            compile.l_g = injected.l_g;
            compile.low_band_d = injected.low_band_d;
            for path in &injected.paths {
                path.validate_lg(Some(compile.l_g))
                    .map_err(|error| format!("{E_MEASURED_G_PATHS}: 注入 G 校验失败：{error}"))?;
            }
            g_source = format!(
                "injected:{} (sha256 {}, solver_note {:?})",
                injected.source_path.display(),
                injected.source_sha256,
                injected.solver_note
            );
            injected.paths.clone()
        }
        None => {
            let pilot_note = if proxy {
                "measured single-position pilot".to_string()
            } else {
                format!(
                    "measured multipoint pilot design={} validation={}",
                    spec.design_positions.len(),
                    spec.validation_positions.len()
                )
            };
            g_source = format!("identity_baseline({pilot_note}; l_g={})", compile.l_g);
            identity_g_paths(m, compile.l_g, &measurement_digest)?
        }
    };
    let g_paths_sha256 = g_paths_canonical_sha256(&g_paths);
    let mut admission = AdmissionEnv::new();
    admission.sample_rate_hz = fs;
    admission.block_size = compile.block_size;
    admission.rt_workers = RT_WORKERS;
    admission.rt_privilege_ok = true;
    admission.clock_source_ok = true;
    admission.cpu_set_ok = true;
    // FitWiringDesign 的 measured=true 来源标签只接受 measured/board（防离线夹具误标）；
    // 更细的 provenance 写入 measured summary artifact。
    let design_source = "measured".to_string();
    let mut design = FitWiringDesign {
        plan,
        target,
        curves,
        channel_bands: spec.channel_bands.clone(),
        design_quality: None,
        peq_options,
        phase_responses,
        phase_channel_bands,
        phase_channel_indices,
        phase_options,
        mapping,
        artifact,
        g_paths,
        compile,
        crossover: None,
        admission,
        role_gains_db: Vec::new(),
        design_source,
        measured: true,
        zero_phase_delay: false,
        created_unix: 0,
    };
    // 5c) crossover P_post 接线（**仅实验路径**；生产默认 OFF = spec 无 crossover 节）。
    //     布局政策：只有 4.1.2（含 SUB1 五执行器）允许输出分频；非 4.1.2 fail-closed。
    let mut crossover_outcome: Option<CrossoverPPostOutcome> = None;
    if let Some(config) = spec.crossover.clone() {
        if !opts.experiment.single_point {
            return Err(format!(
                "{E_MEASURED_SPEC}: {E_MEASURED_CROSSOVER}: crossover 进入 plan 目前仅实验路径（需 --experiment-single-point；生产验收 T3/真实 ALSA/holdout 未完成）"
            ));
        }
        let outcome = apply_measured_crossover_experiment(&mut design, config, opts.experiment)
            .map_err(|error| format!("{E_MEASURED_SPEC}: {error}"))?;
        crossover_outcome = Some(outcome);
    }
    design
        .validate()
        .map_err(|error| format!("{E_MEASURED_SPEC}: FitWiringDesign 校验失败：{error}"))?;
    let crossover_json = match (design.crossover.as_ref(), crossover_outcome.as_ref()) {
        (Some(wiring), Some(outcome)) => Some(json_object(vec![
            ("config", wiring.config.to_json()),
            ("summary", wiring.summary_json()),
            (
                "outcome",
                json_object(vec![
                    ("taps", JsonValue::UInt(outcome.taps as u64)),
                    ("composed_taps", JsonValue::UInt(outcome.composed_taps as u64)),
                    ("l_p", JsonValue::UInt(outcome.l_p as u64)),
                    (
                        "zero_phase_delay",
                        JsonValue::Bool(outcome.zero_phase_delay),
                    ),
                    ("assignments", JsonValue::UInt(outcome.assignments as u64)),
                ]),
            ),
            (
                "note",
                JsonValue::String(
                    "EXPERIMENT: crossover P_post（T39 设计 ⊛ T64 target）进入 plan；产物 EXPERIMENT/NON_ACCEPTABLE；真实 ALSA apply/T3 measured/holdout/T6 全部 NOT_RUN"
                        .to_string(),
                ),
            ),
        ])),
        _ => None,
    };
    let summary_json = measured_summary_json(
        spec,
        &target_id,
        &measurement_digest,
        &measured_positions,
        lo,
        hi,
        &layout,
        opts,
        &g_source,
        &g_paths_sha256,
        crossover_json,
    );
    Ok(MeasuredDesign {
        design,
        summary_json,
        experiment: opts.experiment,
        effective_layout: layout,
        g_source,
        g_paths_sha256,
    })
}

/// identity baseline G（历史行为；对角线 1、非对角 0、长度 = L_G）。
fn identity_g_paths(
    m: usize,
    l_g: u32,
    measurement_digest: &str,
) -> Result<Vec<GPathInput>, String> {
    let mut g_paths = Vec::with_capacity(m * m);
    for out_dense in 0..m {
        for in_dense in 0..m {
            let diag = out_dense == in_dense;
            let mut coeffs = vec![0.0_f64; l_g as usize];
            if diag {
                coeffs[0] = 1.0;
            }
            let path = GPathInput::new(
                out_dense as u16,
                in_dense as u16,
                coeffs,
                diag,
                false,
                sha256_hex(
                    format!(
                        "tspi-measured-identity-g/v1\n{measurement_digest}\n{out_dense}\n{in_dense}"
                    )
                    .as_bytes(),
                ),
            )
            .map_err(|error| format!("{E_MEASURED_SPEC}: GPathInput 非法：{error}"))?
            .with_source(in_dense as u16);
            g_paths.push(path);
        }
    }
    Ok(g_paths)
}

/// G 路径表 canonical sha256（dense 全覆盖；证据/envelope 绑定用）。
pub fn g_paths_canonical_sha256(paths: &[GPathInput]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(paths.len() * 96 + 64);
    text.push_str("tspi-cal-g-paths-canonical/v1\n");
    for path in paths {
        let _ = writeln!(
            text,
            "{}:{}:{:?}:{}:{}",
            path.out_dense, path.in_dense, path.diag_baseline, path.zero, path.coeff_sha256
        );
    }
    sha256_hex(text.as_bytes())
}

/// legacy summary 的逐通道分析证据（首 design 测点；保持历史 JSON 结构）。
fn summary_channels_json(channels: &[ChannelMeasurement]) -> Vec<JsonValue> {
    channels
        .iter()
        .map(|channel| {
            let analysis = &channel.analysis;
            json_object(vec![
                ("name", JsonValue::String(channel.name.clone())),
                ("channel", JsonValue::UInt(analysis.channel)),
                (
                    "analysis_sha256",
                    JsonValue::String(analysis.sha256.clone()),
                ),
                (
                    "delay_samples_used",
                    JsonValue::Number(channel.delay_samples),
                ),
                (
                    "delay_source",
                    JsonValue::String(channel.delay_source.to_string()),
                ),
                (
                    "delay_diag_minus_analysis_samples",
                    JsonValue::Number(channel.delay_delta),
                ),
                (
                    "delay_inconsistent",
                    JsonValue::Bool(channel.delay_delta.abs() > 480.0),
                ),
                (
                    "snr_band_db",
                    if analysis.snr_band_db.is_finite() {
                        JsonValue::Number(analysis.snr_band_db)
                    } else {
                        JsonValue::Null
                    },
                ),
                (
                    "snr_below_t2_25db",
                    JsonValue::Bool(!(analysis.snr_band_db >= 25.0)),
                ),
                (
                    "polarity_confidence",
                    JsonValue::String(analysis.polarity_confidence.clone()),
                ),
            ])
        })
        .collect()
}

/// summary 的逐测点证据（含 role/weight/channels）。
fn summary_positions_json(positions: &[PositionMeasurement]) -> Vec<JsonValue> {
    positions
        .iter()
        .map(|position| {
            json_object(vec![
                ("label", JsonValue::String(position.label.clone())),
                ("role", JsonValue::String(position.role_str().to_string())),
                ("weight", JsonValue::Number(position.weight)),
                (
                    "mic_orientation",
                    JsonValue::String(position.mic_orientation.clone()),
                ),
                (
                    "channels",
                    JsonValue::Array(summary_channels_json(&position.channels)),
                ),
            ])
        })
        .collect()
}

fn measured_summary_json(
    spec: &MeasuredDesignSpec,
    target_id: &str,
    measurement_digest: &str,
    positions: &[PositionMeasurement],
    lo: f64,
    hi: f64,
    effective_layout: &str,
    opts: &MeasuredBuildOptions,
    g_source: &str,
    g_paths_sha256: &str,
    crossover_json: Option<JsonValue>,
) -> String {
    use khaen_compat::ws::{json_encode, json_object};
    let primary = positions
        .iter()
        .find(|position| position.design)
        .expect("validated spec has a design position");
    let channels = summary_channels_json(&primary.channels);
    let proxy = spec.validation_positions.is_empty();
    let design_labels: Vec<String> = spec
        .design_positions
        .iter()
        .map(|position| position.label.clone())
        .collect();
    // 座位间 spread 与 T64 曲线同口径（频带裁剪后的点；deterministic，不读第二份文件）。
    let mut curve_points: Vec<PositionCurve> = Vec::new();
    for position in positions.iter().filter(|position| position.design) {
        for channel in &position.channels {
            let points: Vec<CurvePoint> = channel
                .analysis
                .points
                .iter()
                .filter(|point| point.freq_hz >= lo && point.freq_hz <= hi)
                .copied()
                .collect();
            curve_points.push(PositionCurve::new(
                position.label.clone(),
                channel.name.clone(),
                points,
            ));
        }
    }
    let seat_spread = seat_spread_report(&curve_points, &design_labels);
    let t6_minimums_met = design_labels.len() >= 6 && spec.validation_positions.len() >= 3;
    let validation_labels: Vec<String> = spec
        .validation_positions
        .iter()
        .map(|position| position.label.clone())
        .collect();
    let sum_design_weights: f64 = spec
        .design_positions
        .iter()
        .map(|position| position.weight)
        .sum();
    let multipoint_section = json_object(vec![
        (
            "schema_version",
            JsonValue::String(if spec.schema == MEASURED_SPEC_SCHEMA_V2 {
                "v2".to_string()
            } else {
                "v1".to_string()
            }),
        ),
        ("design_count", JsonValue::UInt(design_labels.len() as u64)),
        (
            "validation_count",
            JsonValue::UInt(validation_labels.len() as u64),
        ),
        (
            "validation_mode",
            JsonValue::String(if proxy {
                VALIDATION_PROXY_SAME_AS_DESIGN.to_string()
            } else {
                VALIDATION_INDEPENDENT_POSITIONS.to_string()
            }),
        ),
        (
            "design_positions",
            JsonValue::Array(
                design_labels
                    .iter()
                    .map(|label| JsonValue::String(label.clone()))
                    .collect(),
            ),
        ),
        (
            "validation_positions",
            JsonValue::Array(
                validation_labels
                    .iter()
                    .map(|label| JsonValue::String(label.clone()))
                    .collect(),
            ),
        ),
        ("proxy_is_real_holdout", JsonValue::Bool(false)),
        ("design_weight_sum", JsonValue::Number(sum_design_weights)),
        (
            "t6_minimums",
            json_object(vec![
                ("design_required", JsonValue::UInt(6)),
                ("validation_required", JsonValue::UInt(3)),
                ("design_count", JsonValue::UInt(design_labels.len() as u64)),
                (
                    "validation_count",
                    JsonValue::UInt(validation_labels.len() as u64),
                ),
                ("points_met", JsonValue::Bool(t6_minimums_met)),
                (
                    "status",
                    JsonValue::String(if t6_minimums_met {
                        "POINTS_MET_NOT_ACCEPTED".to_string()
                    } else {
                        "POINTS_BELOW_T6_MINIMUM".to_string()
                    }),
                ),
                (
                    "note",
                    JsonValue::String(
                        "点数满足也不等于 T6 验收：T6/T68 需真实多点窗口 + 两轮复现 + T54 READY artifact，本链仍 NOT_RUN"
                            .to_string(),
                    ),
                ),
            ]),
        ),
        (
            "fit_aggregation",
            json_object(vec![
                ("design_only", JsonValue::Bool(true)),
                ("validation_in_fit", JsonValue::Bool(false)),
                (
                    "modal",
                    JsonValue::String(
                        "per-frequency weighted linear-amplitude mean over design positions (default weight 1; cross-seat spread > 6 dB downweights deviating seats; modal prominence uses max across design positions)"
                            .to_string(),
                    ),
                ),
                (
                    "wide",
                    JsonValue::String(
                        "per-frequency weighted power mean over design positions, then 1/3-octave moving average"
                            .to_string(),
                    ),
                ),
                (
                    "per_position_residuals",
                    JsonValue::String(
                        "T64 report fit/design-report.json channels[].modal_residual_per_position (design + validation; validation is report-only)"
                            .to_string(),
                    ),
                ),
            ]),
        ),
        ("seat_spread", JsonValue::Array(seat_spread)),
        (
            "holdout",
            json_object(vec![
                (
                    "mode",
                    JsonValue::String(if proxy {
                        "single_point_same_as_design_proxy".to_string()
                    } else {
                        "independent_validation_positions".to_string()
                    }),
                ),
                (
                    "positions",
                    JsonValue::Array(if proxy {
                        vec![JsonValue::String(VALIDATION_PROXY_POSITION_LABEL.to_string())]
                    } else {
                        validation_labels
                            .iter()
                            .map(|label| JsonValue::String(label.clone()))
                            .collect()
                    }),
                ),
                (
                    "evaluation",
                    JsonValue::String(
                        "PREDICTED_FROM_MEASURED_CURVES_AT_VALIDATION_POSITIONS".to_string(),
                    ),
                ),
                (
                    "remeasure_after_apply",
                    JsonValue::String("NOT_RUN".to_string()),
                ),
                ("participates_in_fit", JsonValue::Bool(false)),
                (
                    "note",
                    JsonValue::String(
                        "holdout 目前是 measured-at-validation-positions 的 predicted 评估（用拟合出的 P 作用到 validation 位置实测曲线），不是 apply 后真实 remeasure；真实 holdout remeasure/两轮复现仍 NOT_RUN"
                            .to_string(),
                    ),
                ),
            ]),
        ),
    ]);
    let mut fields = vec![
        (
            "schema",
            JsonValue::String(MEASURED_SUMMARY_SCHEMA.to_string()),
        ),
        ("session_id", JsonValue::String(spec.session_id.clone())),
        ("layout", JsonValue::String(spec.layout.clone())),
        (
            "effective_layout",
            JsonValue::String(effective_layout.to_string()),
        ),
        (
            "design_source",
            JsonValue::String(format!("measured:{}", spec.provenance)),
        ),
        ("measured", JsonValue::Bool(true)),
        ("simulated", JsonValue::Bool(false)),
        ("target", JsonValue::String(target_id.to_string())),
        (
            "measurement_sha256",
            JsonValue::String(measurement_digest.to_string()),
        ),
        (
            "fit_band_hz",
            JsonValue::Array(vec![JsonValue::Number(lo), JsonValue::Number(hi)]),
        ),
        (
            "design_position",
            JsonValue::String(primary.label.clone()),
        ),
        (
            "validation",
            JsonValue::String(spec.validation.clone()),
        ),
        ("channels", JsonValue::Array(channels)),
        (
            "positions",
            JsonValue::Array(summary_positions_json(positions)),
        ),
        (
            "design_positions",
            JsonValue::Array(
                design_labels
                    .iter()
                    .map(|label| JsonValue::String(label.clone()))
                    .collect(),
            ),
        ),
        (
            "validation_positions",
            JsonValue::Array(
                validation_labels
                    .iter()
                    .map(|label| JsonValue::String(label.clone()))
                    .collect(),
            ),
        ),
        ("multipoint", multipoint_section),
        (
            "fit_options",
            json_object(vec![
                (
                    "boost_cap_db",
                    match spec.boost_cap_db {
                        Some(value) => JsonValue::Number(value),
                        None => JsonValue::Null,
                    },
                ),
                (
                    "max_total_stages",
                    match spec.max_total_stages {
                        Some(value) => JsonValue::UInt(value as u64),
                        None => JsonValue::Null,
                    },
                ),
                (
                    "low_band_trusted",
                    match spec.low_band_trusted {
                        Some(value) => JsonValue::Bool(value),
                        None => JsonValue::Null,
                    },
                ),
            ]),
        ),
        (
            "modal_artifact",
            match spec.modal_artifact.as_ref() {
                Some(path) => JsonValue::String(path.to_string_lossy().into_owned()),
                None => JsonValue::Null,
            },
        ),
        (
            "modal_artifact_status",
            JsonValue::String("INCOMPLETE".to_string()),
        ),
        ("g_source", JsonValue::String(g_source.to_string())),
        (
            "g_paths_sha256",
            JsonValue::String(g_paths_sha256.to_string()),
        ),
        (
            "phase_quality_gate",
            JsonValue::String(
                if opts.experiment.is_active() {
                    "disabled(experiment: pilot 无 T63 design scope 独立质量报告；T65 checks 仍照算)"
                } else {
                    "require_design_quality=true(生产默认)"
                }
                .to_string(),
            ),
        ),
        (
            "g_paths_count",
            match opts.g_paths.as_ref() {
                Some(injected) => JsonValue::UInt(injected.paths.len() as u64),
                None => JsonValue::Null,
            },
        ),
        (
            "experiment",
            if opts.experiment.is_active() {
                json_object(vec![
                    (
                        "verdict",
                        JsonValue::String(EXPERIMENT_VERDICT.to_string()),
                    ),
                    (
                        "acceptability",
                        JsonValue::String(EXPERIMENT_ACCEPTABILITY.to_string()),
                    ),
                    ("applied", JsonValue::Bool(false)),
                    ("certifiable", JsonValue::Bool(false)),
                    (
                        "scope",
                        JsonValue::String(opts.experiment.scope().to_string()),
                    ),
                    (
                        "single_point",
                        JsonValue::Bool(opts.experiment.single_point),
                    ),
                    (
                        "multipoint_pilot",
                        JsonValue::Bool(opts.experiment.multipoint_pilot),
                    ),
                    (
                        "allow_incomplete_artifact",
                        JsonValue::Bool(opts.experiment.allow_incomplete_artifact),
                    ),
                    (
                        "layout_override",
                        match opts.layout_override.as_ref() {
                            Some(value) => JsonValue::String(value.clone()),
                            None => JsonValue::Null,
                        },
                    ),
                    ("g_source", JsonValue::String(g_source.to_string())),
                    (
                        "g_paths_sha256",
                        JsonValue::String(g_paths_sha256.to_string()),
                    ),
                    (
                        "note",
                        JsonValue::String(format!(
                            "实验旁路：pilot 设计（{}）；plan 为 EXPERIMENT/NON_ACCEPTABLE，不得写成 READY/PASS；scope={}",
                            if proxy {
                                "单测点 + same_as_design 代理"
                            } else {
                                "多点 design + 独立 validation"
                            },
                            opts.experiment.scope()
                        )),
                    ),
                ])
            } else {
                JsonValue::Null
            },
        ),
        (
            "not_run",
            JsonValue::String(if proxy {
                "独立 holdout / T3 应用后复测 / T6 质量门 / T68 / T69：NOT_RUN（pilot 单测点真实测量仅用于接通 fit 输入）"
                    .to_string()
            } else {
                "真实多点测量窗口 / holdout apply 后 remeasure / T3 复测 / T6 质量门 / T68 / T69：NOT_RUN（本 spec 的点位数据为 measured/pilot；holdout 只是 measured-at-validation-positions 的 predicted 评估）"
                    .to_string()
            }),
        ),
    ];
    // crossover 节：仅在 spec 配置分频时出现（无配置的 measured summary 字节不变）。
    if let Some(crossover) = crossover_json {
        fields.push(("crossover", crossover));
    }
    let document = json_object(fields);
    match json_encode(&document) {
        Ok(text) => text,
        Err(error) => format!(
            "{{\"schema\":\"{MEASURED_SUMMARY_SCHEMA}\",\"error\":\"json encode: {error}\"}}"
        ),
    }
}

// ---------------------------------------------------------------------------
// 多点证据：design/validation 逐位置残差 + 座位间 spread + holdout 口径
// ---------------------------------------------------------------------------

/// 由 measured 设计与 T64 报告生成多点证据 JSON（schema [MEASURED_MULTIPOINT_SCHEMA]）。
///
/// 明确口径：
/// - 拟合只使用 plan 的 design 测点（T64 加权平均/降权语义在 fit_aggregation 写明）；
/// - validation/holdout 测点只出现在报告与 predicted 评估里（modal_residual_per_position
///   的 role=validation 条目），不参与拟合；
/// - holdout.evaluation=PREDICTED_FROM_MEASURED_CURVES_AT_VALIDATION_POSITIONS：
///   用拟合出的 P 作用到 validation 位置实测曲线的预测残差，不是 apply 后 remeasure。
pub fn multipoint_evidence_json(design: &FitWiringDesign, peq: &PeqFitReport) -> String {
    use khaen_compat::ws::{json_encode, json_object};
    let plan = &design.plan;
    let positions: Vec<JsonValue> = plan
        .positions
        .iter()
        .map(|position| {
            json_object(vec![
                ("label", JsonValue::String(position.label.clone())),
                ("role", JsonValue::String(position.role_str().to_string())),
                ("weight", JsonValue::Number(position.weight)),
                (
                    "mic_orientation",
                    match position.mic_orientation.as_ref() {
                        Some(value) => JsonValue::String(value.clone()),
                        None => JsonValue::Null,
                    },
                ),
            ])
        })
        .collect();
    let design_labels: Vec<String> = plan
        .design_positions()
        .map(|position| position.label.clone())
        .collect();
    let validation_labels: Vec<String> = plan
        .validation_positions()
        .map(|position| position.label.clone())
        .collect();
    let proxy =
        validation_labels.len() == 1 && validation_labels[0] == VALIDATION_PROXY_POSITION_LABEL;
    let seat_spread = seat_spread_report(&design.curves, &design_labels);
    let channels: Vec<JsonValue> = plan
        .participants
        .iter()
        .map(|name| {
            let report_channel = peq.channels.iter().find(|channel| &channel.name == name);
            let residuals: Vec<JsonValue> = report_channel
                .map(|channel| {
                    channel
                        .modal_residual_per_position
                        .iter()
                        .map(|residual| {
                            json_object(vec![
                                ("label", JsonValue::String(residual.label.clone())),
                                ("role", JsonValue::String(residual.role.clone())),
                                ("weight", JsonValue::Number(residual.weight)),
                                (
                                    "modal_rms_before_db",
                                    JsonValue::Number(residual.modal_rms_before_db),
                                ),
                                (
                                    "modal_rms_after_peq_db",
                                    JsonValue::Number(residual.modal_rms_after_peq_db),
                                ),
                                (
                                    "modal_rms_after_p_db",
                                    JsonValue::Number(residual.modal_rms_after_p_db),
                                ),
                                (
                                    "modal_max_after_p_db",
                                    JsonValue::Number(residual.modal_max_after_p_db),
                                ),
                                ("rule_2a_pass", JsonValue::Bool(residual.rule_2a_pass)),
                                ("rule_2b_pass", JsonValue::Bool(residual.rule_2b_pass)),
                            ])
                        })
                        .collect()
                })
                .unwrap_or_default();
            json_object(vec![
                ("name", JsonValue::String(name.clone())),
                (
                    "design_rule_2a_pass",
                    JsonValue::Bool(
                        report_channel
                            .map(|channel| channel.rule_2a_pass)
                            .unwrap_or(false),
                    ),
                ),
                (
                    "design_rule_2b_pass",
                    JsonValue::Bool(
                        report_channel
                            .map(|channel| channel.rule_2b_pass)
                            .unwrap_or(false),
                    ),
                ),
                (
                    "holdout_rule_2a_pass",
                    JsonValue::Bool(
                        report_channel
                            .map(|channel| channel.holdout_rule_2a_pass)
                            .unwrap_or(false),
                    ),
                ),
                (
                    "holdout_rule_2b_pass",
                    JsonValue::Bool(
                        report_channel
                            .map(|channel| channel.holdout_rule_2b_pass)
                            .unwrap_or(false),
                    ),
                ),
                (
                    "holdout_rule_2a_peak_over_target_db",
                    JsonValue::Number(
                        report_channel
                            .map(|channel| channel.holdout_rule_2a_peak_over_target_db)
                            .unwrap_or(f64::NAN),
                    ),
                ),
                ("per_position_residuals", JsonValue::Array(residuals)),
            ])
        })
        .collect();
    let t6_points_met = plan.require_t6_minimums().is_ok();
    let document = json_object(vec![
        (
            "schema",
            JsonValue::String(MEASURED_MULTIPOINT_SCHEMA.to_string()),
        ),
        ("session_id", JsonValue::String(plan.session_id.clone())),
        ("layout", JsonValue::String(plan.layout.clone())),
        (
            "sample_rate_hz",
            JsonValue::UInt(plan.sample_rate_hz as u64),
        ),
        (
            "design_source",
            JsonValue::String(design.design_source.clone()),
        ),
        ("measured", JsonValue::Bool(design.measured)),
        ("positions", JsonValue::Array(positions)),
        (
            "counts",
            json_object(vec![
                ("design", JsonValue::UInt(design_labels.len() as u64)),
                (
                    "validation",
                    JsonValue::UInt(validation_labels.len() as u64),
                ),
                ("total", JsonValue::UInt(plan.total_count() as u64)),
            ]),
        ),
        (
            "t6_minimums",
            json_object(vec![
                ("design_required", JsonValue::UInt(6)),
                ("validation_required", JsonValue::UInt(3)),
                ("points_met", JsonValue::Bool(t6_points_met)),
                (
                    "status",
                    JsonValue::String(if t6_points_met {
                        "POINTS_MET_NOT_ACCEPTED".to_string()
                    } else {
                        "POINTS_BELOW_T6_MINIMUM".to_string()
                    }),
                ),
            ]),
        ),
        (
            "fit_aggregation",
            json_object(vec![
                ("design_only", JsonValue::Bool(true)),
                ("validation_in_fit", JsonValue::Bool(false)),
                (
                    "modal",
                    JsonValue::String(
                        "per-frequency weighted linear-amplitude mean over design positions (default weight 1; cross-seat spread > 6 dB downweights deviating seats; modal prominence uses max across design positions)"
                            .to_string(),
                    ),
                ),
                (
                    "wide",
                    JsonValue::String(
                        "per-frequency weighted power mean over design positions, then 1/3-octave moving average"
                            .to_string(),
                    ),
                ),
            ]),
        ),
        ("seat_spread", JsonValue::Array(seat_spread)),
        ("channels", JsonValue::Array(channels)),
        (
            "holdout",
            json_object(vec![
                (
                    "mode",
                    JsonValue::String(if proxy {
                        "single_point_same_as_design_proxy".to_string()
                    } else {
                        "independent_validation_positions".to_string()
                    }),
                ),
                (
                    "positions",
                    JsonValue::Array(
                        validation_labels
                            .iter()
                            .map(|label| JsonValue::String(label.clone()))
                            .collect(),
                    ),
                ),
                (
                    "evaluation",
                    JsonValue::String(
                        "PREDICTED_FROM_MEASURED_CURVES_AT_VALIDATION_POSITIONS".to_string(),
                    ),
                ),
                (
                    "remeasure_after_apply",
                    JsonValue::String("NOT_RUN".to_string()),
                ),
                ("participates_in_fit", JsonValue::Bool(false)),
                (
                    "note",
                    JsonValue::String(
                        "holdout 是 measured-at-validation-positions 的 predicted 评估（P 作用到 validation 位置实测曲线），不是 apply 后真实 remeasure；真实 holdout remeasure/T3/T6/T68 仍 NOT_RUN"
                            .to_string(),
                    ),
                ),
            ]),
        ),
        (
            "checks",
            json_object(vec![
                (
                    "all_rule_2a_pass",
                    JsonValue::Bool(peq.checks.all_rule_2a_pass),
                ),
                (
                    "all_rule_2b_pass",
                    JsonValue::Bool(peq.checks.all_rule_2b_pass),
                ),
                (
                    "all_headroom_pass",
                    JsonValue::Bool(peq.checks.all_headroom_pass),
                ),
                (
                    "all_boost_limits_pass",
                    JsonValue::Bool(peq.checks.all_boost_limits_pass),
                ),
            ]),
        ),
        (
            "measurement_sha256",
            JsonValue::String(peq.measurement_sha256.clone()),
        ),
        ("p_sha256", JsonValue::String(peq.p_sha256.clone())),
        (
            "not_run",
            JsonValue::Array(vec![
                JsonValue::String(
                    "真实多点测量窗口（design>=2 + holdout>=1 的 board 采集）".to_string(),
                ),
                JsonValue::String("apply 后 holdout remeasure / T3 复测".to_string()),
                JsonValue::String(
                    "T6 质量门（6 design + 3 holdout；本报告只给 pilot 点数）".to_string(),
                ),
                JsonValue::String("T68/T69 两轮复现与独立验收".to_string()),
                JsonValue::String(
                    "T54 READY modal artifact（pilot 路径恒 INCOMPLETE）".to_string(),
                ),
            ]),
        ),
        (
            "limits",
            JsonValue::Array(vec![
                JsonValue::String(
                    "validation/holdout 只做报告与 predicted 评估，不参与任何调参/拟合（F-39）"
                        .to_string(),
                ),
                JsonValue::String(
                    "T64 聚合是 design 位置逐频加权平均（模态幅度平均 + 宽带功率平均），不是按座位分别拟合"
                        .to_string(),
                ),
                JsonValue::String(
                    "本报告由合成/pilot 输入生成时为 EXPERIMENT/NON_ACCEPTABLE，不得写成 PASS/CERTIFIED"
                        .to_string(),
                ),
            ]),
        ),
    ]);
    match json_encode(&document) {
        Ok(text) => text,
        Err(error) => format!(
            "{{\"schema\":\"{MEASURED_MULTIPOINT_SCHEMA}\",\"error\":\"json encode: {error}\"}}"
        ),
    }
}

#[cfg(test)]
mod measured_design_tests {
    use super::*;
    use crate::peq_fit::{fit_peq, PeqFitOutput, PeqFitRequest};
    use khaen_compat::ws::json_parse;
    use khaen_modal_design::ingest::PositionRole;

    // -----------------------------------------------------------------------
    // 夹具：合成旧工具 analysis JSON（1/24 oct 20–20k；phase 已对齐、delay 声明）
    // -----------------------------------------------------------------------

    fn test_dir(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "measured-multipoint-{tag}-pid{}",
                std::process::id()
            ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn bump(freq: f64, center: f64, gain_db: f64, oct_width: f64) -> f64 {
        let distance = (freq / center).log2() / oct_width;
        gain_db * (-0.5 * distance * distance).exp()
    }

    fn analysis_json<F: Fn(f64) -> f64>(channel: u64, sr: u32, delay: f64, shape: F) -> String {
        let mut fr = String::new();
        let mut index = 0;
        loop {
            let freq = 20.0 * 2f64.powf(index as f64 / 24.0);
            if freq > 20_000.0 {
                break;
            }
            if !fr.is_empty() {
                fr.push(',');
            }
            fr.push_str(&format!(
                "{{\"freq_hz\":{freq:.6},\"db\":{:.6},\"phase_deg\":0.0}}",
                shape(freq)
            ));
            index += 1;
        }
        format!(
            "{{\"schema\":\"tspi-channel-measurement/v1\",\"channel\":{channel},\"delay_samples\":{delay:.6},\"snr_band_db\":31.0,\"polarity_confidence\":\"high\",\"meta\":{{\"sample_rate\":{sr},\"sweep_start_hz\":20.0,\"sweep_end_hz\":20000.0}},\"fr\":[{fr}]}}"
        )
    }

    fn write_json(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write fixture");
        path
    }

    fn design_a_shape(freq: f64) -> f64 {
        bump(freq, 100.0, 6.0, 0.08) + bump(freq, 160.0, 4.0, 0.10) + bump(freq, 1_200.0, -1.5, 0.5)
    }

    fn design_b_shape(freq: f64) -> f64 {
        0.8 + bump(freq, 100.0, 5.2, 0.08)
            + bump(freq, 160.0, 3.6, 0.10)
            + bump(freq, 300.0, 5.0, 0.06)
    }

    fn validation_shape(freq: f64) -> f64 {
        -0.5 + bump(freq, 100.0, 2.0, 0.10) + bump(freq, 1_000.0, -4.0, 0.30)
    }

    fn write_position(
        dir: &Path,
        prefix: &str,
        fl_shape: fn(f64) -> f64,
        fr_shape: fn(f64) -> f64,
    ) -> (PathBuf, PathBuf) {
        let fl = write_json(
            dir,
            &format!("{prefix}-FL.analysis.json"),
            &analysis_json(0, 48_000, 96.0, fl_shape),
        );
        let fr = write_json(
            dir,
            &format!("{prefix}-FR.analysis.json"),
            &analysis_json(1, 48_000, 120.0, fr_shape),
        );
        (fl, fr)
    }

    fn position_spec_json(label: &str, fl: &Path, fr: &Path, weight: Option<f64>) -> String {
        let weight = weight
            .map(|value| format!(",\"weight\":{value}"))
            .unwrap_or_default();
        format!(
            "{{\"label\":\"{label}\",\"mic_orientation\":\"90deg\"{weight},\"channels\":[{{\"name\":\"FL\",\"analysis\":\"{}\"}},{{\"name\":\"FR\",\"analysis\":\"{}\"}}]}}",
            fl.display().to_string().replace('\\', "/"),
            fr.display().to_string().replace('\\', "/")
        )
    }

    /// v2 spec：2 design (D1/D2) + 1 独立 validation (V1)；FL 96 / FR 120 samples 到达。
    fn multipoint_workspace(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).expect("create dir");
        let (d1_fl, d1_fr) = write_position(dir, "D1", design_a_shape, design_a_shape);
        let (d2_fl, d2_fr) = write_position(dir, "D2", design_b_shape, design_b_shape);
        let (v1_fl, v1_fr) = write_position(dir, "V1", validation_shape, validation_shape);
        let spec = format!(
            r#"{{
  "schema": "tspi-cal-measured-design/v2",
  "session_id": "multipoint-unit-test",
  "layout": "stereo-2.0",
  "sample_rate_hz": 48000,
  "target": "flat",
  "validation": "independent_validation_positions",
  "provenance": "unit_test_synthetic_multipoint_only",
  "design_positions": [
    {},
    {}
  ],
  "validation_positions": [
    {}
  ],
  "channel_bands": [
    {{"channel": "FL", "band_hz": [80.0, 16000.0]}},
    {{"channel": "FR", "band_hz": [80.0, 16000.0]}}
  ],
  "dense_to_dac": [0, 1],
  "roles": [
    {{"role_index": 0, "role_id": "FL", "dac": 0, "outputs": [0]}},
    {{"role_index": 1, "role_id": "FR", "dac": 1, "outputs": [1]}}
  ],
  "container_channels": 8,
  "unused_outputs": [2, 3, 4, 5, 6, 7],
  "compile": {{"block_size": 128, "n_in": 2, "container_channels": 8, "low_band_d": 0, "l_g": 8, "l_p": 8}},
  "modal_artifact": null
}}"#,
            position_spec_json("D1", &d1_fl, &d1_fr, Some(1.0)),
            position_spec_json("D2", &d2_fl, &d2_fr, Some(0.8)),
            position_spec_json("V1", &v1_fl, &v1_fr, None)
        );
        write_json(dir, "spec-v2.json", &spec)
    }

    /// v1 legacy spec：单 design + same_as_design proxy（历史文件；构建行为必须不变）。
    fn single_point_v1_workspace(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).expect("create dir");
        let (fl, fr) = write_position(dir, "D1", design_a_shape, design_a_shape);
        let spec = format!(
            r#"{{
  "schema": "tspi-cal-measured-design/v1",
  "session_id": "single-point-legacy-unit-test",
  "layout": "stereo-2.0",
  "sample_rate_hz": 48000,
  "target": "flat",
  "validation": "pilot_single_position_same_as_design_proxy",
  "provenance": "unit_test_legacy_v1_only",
  "design_position": {},
  "channel_bands": [
    {{"channel": "FL", "band_hz": [80.0, 16000.0]}},
    {{"channel": "FR", "band_hz": [80.0, 16000.0]}}
  ],
  "dense_to_dac": [0, 1],
  "roles": [
    {{"role_index": 0, "role_id": "FL", "dac": 0, "outputs": [0]}},
    {{"role_index": 1, "role_id": "FR", "dac": 1, "outputs": [1]}}
  ],
  "container_channels": 8,
  "unused_outputs": [2, 3, 4, 5, 6, 7],
  "compile": {{"block_size": 128, "n_in": 2, "container_channels": 8, "low_band_d": 0, "l_g": 8, "l_p": 8}},
  "modal_artifact": null
}}"#,
            position_spec_json("D1", &fl, &fr, None)
        );
        write_json(dir, "spec-v1.json", &spec)
    }

    fn build_from(path: &Path) -> MeasuredDesign {
        let spec = load_measured_spec(path).expect("load spec");
        build_measured_design(&spec).expect("build measured design")
    }

    fn run_t64_fit(design: &FitWiringDesign) -> PeqFitOutput {
        let mut request = PeqFitRequest::new(&design.plan, &design.target, &design.curves)
            .with_options(design.peq_options.clone())
            .with_created_unix(design.created_unix);
        if !design.channel_bands.is_empty() {
            request = request.with_channel_bands(&design.channel_bands);
        }
        fit_peq(&request).expect("T64 fit")
    }

    // -----------------------------------------------------------------------
    // 1) spec v2 解析
    // -----------------------------------------------------------------------

    #[test]
    fn t224_measured_spec_v2_parses_multipoint_positions() {
        let dir = test_dir("parse-v2");
        let path = multipoint_workspace(&dir);
        let spec = load_measured_spec(&path).expect("v2 spec");
        assert_eq!(spec.schema, MEASURED_SPEC_SCHEMA_V2);
        assert_eq!(spec.design_positions.len(), 2);
        assert_eq!(spec.design_positions[0].label, "D1");
        assert_eq!(spec.design_positions[1].label, "D2");
        assert_eq!(spec.design_positions[1].weight, 0.8);
        assert_eq!(spec.validation_positions.len(), 1);
        assert_eq!(spec.validation_positions[0].label, "V1");
        assert_eq!(spec.validation_positions[0].weight, 1.0);
        assert_eq!(spec.validation, VALIDATION_INDEPENDENT_POSITIONS);
        assert!(!spec.is_validation_proxy());
        assert_eq!(spec.measured_position_count(), 3);
        assert_eq!(spec.design_positions[0].channels.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 2) v1 向后兼容（单点 proxy 行为不变）
    // -----------------------------------------------------------------------

    #[test]
    fn t224_measured_spec_v1_still_loads_proxy_single_point() {
        let dir = test_dir("v1-compat");
        let path = single_point_v1_workspace(&dir);
        let spec = load_measured_spec(&path).expect("v1 spec");
        assert_eq!(spec.schema, MEASURED_SPEC_SCHEMA_V1);
        assert_eq!(spec.design_positions.len(), 1);
        assert!(spec.validation_positions.is_empty());
        assert!(spec.is_validation_proxy());
        assert_eq!(spec.validation, VALIDATION_PROXY_SAME_AS_DESIGN);
        let measured = build_measured_design(&spec).expect("v1 build");
        // 单 point：D1 x FL/FR + V1_proxy x FL/FR = 4 curves；phase 只 2 条。
        assert_eq!(measured.design.curves.len(), 4);
        assert_eq!(measured.design.phase_responses.len(), 2);
        assert_eq!(measured.design.plan.design_count(), 1);
        assert_eq!(measured.design.plan.validation_count(), 1);
        assert!(measured
            .design
            .plan
            .position(VALIDATION_PROXY_POSITION_LABEL)
            .is_some());
        assert_eq!(
            measured
                .design
                .plan
                .position(VALIDATION_PROXY_POSITION_LABEL)
                .unwrap()
                .role,
            PositionRole::Validation
        );
        assert!(measured
            .design
            .curves
            .iter()
            .any(|curve| curve.position == VALIDATION_PROXY_POSITION_LABEL));
        assert!(measured
            .summary_json
            .contains(VALIDATION_PROXY_SAME_AS_DESIGN));
        assert!(measured.summary_json.contains("\"multipoint\""));
        assert!(measured.summary_json.contains("\"design_count\":1"));
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 3) 非法 spec 拒绝（v1/v2 混用、多点 proxy、重复 label、坏 weight）
    // -----------------------------------------------------------------------

    #[test]
    fn t224_measured_spec_v2_rejects_illegal_position_configs() {
        let dir = test_dir("reject");
        let path = multipoint_workspace(&dir);
        let valid = fs::read_to_string(&path).expect("read spec");

        // a) design_position 与 design_positions 同时给 -> 歧义拒绝
        let ambiguous = valid.replace(
            "\"design_positions\": [",
            "\"design_position\": {}, \"design_positions\": [",
        );
        fs::write(&path, ambiguous).unwrap();
        assert!(load_measured_spec(&path)
            .unwrap_err()
            .contains(E_MEASURED_SPEC));

        // b) 多点 design 但删掉 validation_positions -> proxy 冒充 holdout 拒绝
        let proxy = valid.replace(
            "\"validation\": \"independent_validation_positions\",",
            "\"validation\": \"pilot_single_position_same_as_design_proxy\",",
        );
        // 通过把 validation_positions 数组清空模拟（保持 JSON 合法：删除该键）。
        let start = proxy
            .find("  \"validation_positions\": [")
            .expect("validation key");
        let tail = &proxy[start..];
        let end = tail.find("  ],").expect("array end") + start + "  ],\n".len();
        let no_validation = format!("{}{}", &proxy[..start], &proxy[end..]);
        fs::write(&path, no_validation).unwrap();
        let error = load_measured_spec(&path).unwrap_err();
        assert!(error.contains(E_MEASURED_SPEC), "{error}");
        assert!(error.contains("validation"), "{error}");

        // c) 重复 label（V1 改成 D1）
        let duplicate = valid.replace("\"label\":\"V1\"", "\"label\":\"D1\"");
        fs::write(&path, duplicate).unwrap();
        assert!(load_measured_spec(&path).unwrap_err().contains("重复"));

        // d) 非法 weight（0）
        let bad_weight = valid.replace(
            "\"mic_orientation\":\"90deg\",\"weight\":0.8",
            "\"mic_orientation\":\"90deg\",\"weight\":0.0",
        );
        fs::write(&path, bad_weight).unwrap();
        assert!(load_measured_spec(&path)
            .unwrap_err()
            .contains(E_MEASURED_SPEC));

        // e) v1 schema 却给 design_positions[] -> 拒绝
        let v1_mixed = valid.replace("tspi-cal-measured-design/v2", "tspi-cal-measured-design/v1");
        fs::write(&path, v1_mixed).unwrap();
        assert!(load_measured_spec(&path).unwrap_err().contains("v1 不支持"));
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 4) 多点构建：plan 角色标记 + phase 只覆盖 design
    // -----------------------------------------------------------------------

    #[test]
    fn t224_multipoint_build_marks_plan_roles_and_phase_design_only() {
        let dir = test_dir("roles");
        let path = multipoint_workspace(&dir);
        let measured = build_from(&path);
        let plan = &measured.design.plan;
        assert_eq!(plan.design_count(), 2);
        assert_eq!(plan.validation_count(), 1);
        assert_eq!(plan.total_count(), 3);
        assert_eq!(plan.position("D1").unwrap().role, PositionRole::Design);
        assert_eq!(plan.position("D2").unwrap().role, PositionRole::Design);
        assert_eq!(plan.position("V1").unwrap().role, PositionRole::Validation);
        // 6 条曲线 = 3 位置 x FL/FR；不得出现 proxy。
        assert_eq!(measured.design.curves.len(), 6);
        assert!(!measured
            .design
            .curves
            .iter()
            .any(|curve| curve.position == VALIDATION_PROXY_POSITION_LABEL));
        // phase responses = 2 design x 2 通道；validation 不得进入 T65。
        assert_eq!(measured.design.phase_responses.len(), 4);
        for response in &measured.design.phase_responses {
            assert!(
                response.position() == "D1" || response.position() == "D2",
                "unexpected phase position {}",
                response.position()
            );
        }
        assert_eq!(
            measured.design.phase_options.reference_position.as_deref(),
            Some("D1")
        );
        // T63 plan 的 validation 权重与 spec 一致。
        assert!((plan.position("D2").unwrap().weight - 0.8).abs() < 1e-12);
        assert!(!measured.design.artifact.is_ready());
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 5) validation 不参与 fit（改 validation 数据 -> P 不变，仅报告变）
    // -----------------------------------------------------------------------

    #[test]
    fn t224_multipoint_validation_does_not_affect_t64_fit() {
        let dir = test_dir("holdout-fit");
        let path = multipoint_workspace(&dir);
        let first = build_from(&path);
        let fit_first = run_t64_fit(&first.design);

        // 改 validation 测点数据（-4 dB dip 加深到 -9 dB）；design 文件完全不动。
        let v1_fl = dir.join("V1-FL.analysis.json");
        let v1_fr = dir.join("V1-FR.analysis.json");
        // 模态带（默认 20–200 Hz）内的 100 Hz 峰从 +2 dB 改成 -6 dB：validation 残差必须变。
        let altered =
            |freq: f64| -0.5 + bump(freq, 100.0, -6.0, 0.10) + bump(freq, 1_000.0, -4.0, 0.30);
        fs::write(&v1_fl, analysis_json(0, 48_000, 96.0, altered)).unwrap();
        fs::write(&v1_fr, analysis_json(1, 48_000, 120.0, altered)).unwrap();
        let second = build_from(&path);
        let fit_second = run_t64_fit(&second.design);

        // 拟合结果（P 系数/段表/p_sha）不得受 validation 数据影响。
        assert_eq!(fit_first.report.p_sha256, fit_second.report.p_sha256);
        let bands_first: Vec<_> = fit_first
            .report
            .channels
            .iter()
            .map(|channel| channel.bands.clone())
            .collect();
        let bands_second: Vec<_> = fit_second
            .report
            .channels
            .iter()
            .map(|channel| channel.bands.clone())
            .collect();
        assert_eq!(bands_first, bands_second);
        // 但 measurement digest / validation 残差必须变化（数据被读入，只用于报告）。
        assert_ne!(
            fit_first.report.measurement_sha256,
            fit_second.report.measurement_sha256
        );
        let v1_first = fit_first.report.channels[0]
            .modal_residual_per_position
            .iter()
            .find(|residual| residual.label == "V1")
            .expect("V1 residual reported");
        let v1_second = fit_second.report.channels[0]
            .modal_residual_per_position
            .iter()
            .find(|residual| residual.label == "V1")
            .expect("V1 residual reported");
        assert_eq!(v1_first.role, "validation");
        assert!(
            (v1_first.modal_rms_before_db - v1_second.modal_rms_before_db).abs() > 0.1,
            "validation residual should change when holdout data changes"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 6) 确定性 + 逐位置残差角色
    // -----------------------------------------------------------------------

    #[test]
    fn t224_multipoint_t64_fit_deterministic_and_roles_reported() {
        let dir = test_dir("determinism");
        let path = multipoint_workspace(&dir);
        let first = build_from(&path);
        let second = build_from(&path);
        assert_eq!(first.summary_json, second.summary_json);
        let fit_first = run_t64_fit(&first.design);
        let fit_second = run_t64_fit(&second.design);
        assert_eq!(
            fit_first.report.to_json(),
            fit_second.report.to_json(),
            "same inputs must produce identical T64 report"
        );
        for channel in &fit_first.report.channels {
            assert_eq!(channel.modal_residual_per_position.len(), 3);
            let roles: Vec<&str> = channel
                .modal_residual_per_position
                .iter()
                .map(|residual| residual.role.as_str())
                .collect();
            assert!(roles.contains(&"design"), "{roles:?}");
            assert!(roles.contains(&"validation"), "{roles:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 7) 多点证据 JSON：座位 spread + holdout predicted 口径 + T6 缺口
    // -----------------------------------------------------------------------

    #[test]
    fn t224_multipoint_evidence_reports_spread_and_holdout_predicted() {
        let dir = test_dir("evidence");
        let path = multipoint_workspace(&dir);
        let measured = build_from(&path);
        let fit = run_t64_fit(&measured.design);
        let evidence = multipoint_evidence_json(&measured.design, &fit.report);
        let document = json_parse(&evidence).expect("evidence json");
        assert_eq!(
            document.get("schema").and_then(|value| value.as_str()),
            Some(MEASURED_MULTIPOINT_SCHEMA)
        );
        assert_eq!(
            document
                .get("counts")
                .and_then(|value| value.get("design"))
                .and_then(|value| value.as_u64()),
            Some(2)
        );
        assert_eq!(
            document
                .get("counts")
                .and_then(|value| value.get("validation"))
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        let holdout = document.get("holdout").expect("holdout section");
        assert_eq!(
            holdout.get("evaluation").and_then(|value| value.as_str()),
            Some("PREDICTED_FROM_MEASURED_CURVES_AT_VALIDATION_POSITIONS")
        );
        assert_eq!(
            holdout
                .get("remeasure_after_apply")
                .and_then(|value| value.as_str()),
            Some("NOT_RUN")
        );
        assert_eq!(
            holdout
                .get("participates_in_fit")
                .and_then(|value| value.as_bool()),
            Some(false)
        );
        assert_eq!(
            holdout.get("mode").and_then(|value| value.as_str()),
            Some("independent_validation_positions")
        );
        // 座位间 spread：FL/FR 两条 design curve -> 至少 1 个通道、max_spread > 0。
        let spread = document
            .get("seat_spread")
            .and_then(|value| value.as_array())
            .expect("seat spread array");
        assert!(!spread.is_empty());
        assert!(spread.iter().any(|entry| entry
            .get("max_spread_db")
            .and_then(|value| value.as_f64())
            .map(|value| value > 0.0)
            .unwrap_or(false)));
        // 每通道逐位置残差 = 3（D1/D2/V1），V1 必须是 validation。
        let channels = document
            .get("channels")
            .and_then(|value| value.as_array())
            .expect("channels array");
        for channel in channels {
            let residuals = channel
                .get("per_position_residuals")
                .and_then(|value| value.as_array())
                .expect("residuals");
            assert_eq!(residuals.len(), 3);
            assert!(residuals.iter().any(|residual| residual
                .get("role")
                .and_then(|value| value.as_str())
                == Some("validation")));
        }
        // T6 点是 2+1，明确低于 6+3，不得写 PASS。
        assert_eq!(
            document
                .get("t6_minimums")
                .and_then(|value| value.get("points_met"))
                .and_then(|value| value.as_bool()),
            Some(false)
        );
        assert!(evidence.contains("NOT_RUN"));
        assert!(!evidence.contains("\"PASS\""));
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 8) measured summary：多点/座位 spread/T6 缺口（fitting 前证据）
    // -----------------------------------------------------------------------

    #[test]
    fn t224_multipoint_summary_reports_spread_and_t6_gap() {
        let dir = test_dir("summary");
        let path = multipoint_workspace(&dir);
        let measured = build_from(&path);
        let document = json_parse(&measured.summary_json).expect("summary json");
        let multipoint = document.get("multipoint").expect("multipoint section");
        assert_eq!(
            multipoint
                .get("design_count")
                .and_then(|value| value.as_u64()),
            Some(2)
        );
        assert_eq!(
            multipoint
                .get("validation_count")
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        assert_eq!(
            multipoint
                .get("validation_mode")
                .and_then(|value| value.as_str()),
            Some(VALIDATION_INDEPENDENT_POSITIONS)
        );
        assert_eq!(
            multipoint
                .get("t6_minimums")
                .and_then(|value| value.get("points_met"))
                .and_then(|value| value.as_bool()),
            Some(false)
        );
        let spread = multipoint
            .get("seat_spread")
            .and_then(|value| value.as_array())
            .expect("seat spread");
        assert_eq!(spread.len(), 2, "FL/FR should both have seat spread");
        let validation_positions = document
            .get("validation_positions")
            .and_then(|value| value.as_array())
            .expect("validation positions");
        assert_eq!(validation_positions.len(), 1);
        assert_eq!(validation_positions[0].as_str(), Some("V1"));
        assert_eq!(
            document
                .get("design_positions")
                .and_then(|value| value.as_array())
                .map(|values| values.len()),
            Some(2)
        );
        assert!(measured.summary_json.contains("NOT_RUN"));
        assert!(!measured
            .summary_json
            .contains(VALIDATION_PROXY_SAME_AS_DESIGN));
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 9) 多点 spec 通道集合不一致 -> 稳定拒绝
    // -----------------------------------------------------------------------

    #[test]
    fn t224_multipoint_rejects_channel_set_mismatch() {
        let dir = test_dir("channel-set");
        let path = multipoint_workspace(&dir);
        let text = fs::read_to_string(&path).expect("read spec");
        // 把 D2 的 FR 通道改名为 SL：通道集合与首个 design 测点不一致。
        let before = text.find("\"label\":\"D2\"").expect("D2 label");
        let after = text[before..]
            .find("\"name\":\"FR\"")
            .expect("D2 FR channel")
            + before;
        let mut broken = text.clone();
        broken.replace_range(after..after + "\"name\":\"FR\"".len(), "\"name\":\"SL\"");
        fs::write(&path, broken).unwrap();
        let spec = load_measured_spec(&path).expect("parse still ok");
        let error = build_measured_design(&spec).expect_err("channel mismatch must fail");
        assert!(error.contains(E_MEASURED_DATA), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 10) legacy 单点回归：summary/plan/curves 与历史结构一致（不出现 proxy=holdout 谎称）
    // -----------------------------------------------------------------------

    #[test]
    fn t224_single_point_legacy_summary_is_bit_stable_across_builds() {
        let dir = test_dir("legacy-stable");
        let path = single_point_v1_workspace(&dir);
        let first = build_from(&path);
        let second = build_from(&path);
        assert_eq!(first.summary_json, second.summary_json);
        assert_eq!(first.design.curves.len(), 4);
        let document = json_parse(&first.summary_json).expect("summary json");
        let multipoint = document.get("multipoint").expect("multipoint");
        assert_eq!(
            multipoint
                .get("validation_mode")
                .and_then(|value| value.as_str()),
            Some(VALIDATION_PROXY_SAME_AS_DESIGN)
        );
        assert_eq!(
            multipoint
                .get("proxy_is_real_holdout")
                .and_then(|value| value.as_bool()),
            Some(false)
        );
        assert_eq!(
            multipoint
                .get("seat_spread")
                .and_then(|value| value.as_array())
                .map(|values| values.len()),
            Some(0),
            "single point has no seat-to-seat spread"
        );
        let holdout = multipoint.get("holdout").expect("holdout");
        assert_eq!(
            holdout
                .get("remeasure_after_apply")
                .and_then(|value| value.as_str()),
            Some("NOT_RUN")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
