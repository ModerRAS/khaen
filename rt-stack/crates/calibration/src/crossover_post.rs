//! [NONRT] crossover（T39 输出 FIR 分频）接入 measured P_post / T66 plan 的接线层。
//!
//! 背景：T39 交付了 LR2/LR4/LR6/LR8 与 BW1..BW8 输出 FIR 分频**设计库**
//!（<code>khaen-designer::crossover</code>），core plan ABI 也有
//! <code>FirSegmentKind::Crossover</code> / <code>DIAG_FLAG_CROSSOVER</code> 组合语义；但
//! measured 设计 -> FitP -> P_post -> T66 plan 的生产链此前**只承载 T64 目标曲线 FIR**，
//! 没有任何路径把分频设计接进 P_post（缺口设计见
//! <code>evidence/calib-chain-20261121/crossover-gap-20261122/CROSSOVER-DESIGN.md</code>）。
//!
//! 本模块补齐"配置 -> T39 设计 -> 每输出支路 -> P_post 段组合"这一段：
//!
//! - measured spec 的 <code>"crossover"</code> 节（**缺省 = None = 行为完全不变**）；
//! - T39 <code>design_strict</code>（INFEASIBLE 一律 fail-closed，不静默放行）；
//! - <code>plan_outputs</code> 的显式赋值（**无默认 HP**：未赋值输出 = 直通）；
//! - 与 T64 目标 FIR 的规范段序卷积：<code>crossover → target</code>（core
//!   <code>FirSegmentKind::rank</code>），返回卷积核 + 段来源，供 FitWiring 写
//!   <code>RoleCompileInput.fir_segments</code> / diag flags / D7 证据。
//!
//! # 诚实边界
//!
//! - 本模块纯离线（无设备/无声/无 ALSA），只做数值设计与内存组合。
//! - 接入 plan 的路径当前只在**实验**（EXPERIMENT/NON_ACCEPTABLE）下开放
//!  （见 [crate::fit_wiring::apply_measured_crossover_experiment]）；生产验收
//!  （T3/真实 ALSA/holdout/多点）**NOT_RUN**，不得声称已通过。
//! - 4.1.2 布局政策：带 SUB1 的五执行器布局是分频接线的实验载体；非 4.1.2 布局
//!   fail-closed（[CROSSOVER_LAYOUT_REQUIRED]）。

use std::fmt::Write as _;

use khaen_compat::ws::JsonValue;
use khaen_core::nodes::{compose_fir_segments, FirSegment, FirSegmentKind};
use khaen_designer::crossover::{
    design_strict, plan_outputs, Branch, CrossoverDesign, CrossoverError, CrossoverMethod,
    CrossoverSpec, CrossoverType, OutputAssignment, OutputPlan, ValidatedCrossoverSpec,
    REGULAR_PROFILE_TAPS_CAP,
};

use crate::job::sha256_hex;

/// crossover 接线证据 schema（measured summary / compile report / wiring summary 引用）。
pub const CROSSOVER_WIRING_SCHEMA: &str = "tspi-cal-crossover-wiring/v1";
/// crossover config 进入 plan 的稳定错误码（包装 T39 <code>E_FIR_XOVER_*</code>/<code>E_FIR_TAPS</code>）。
pub const E_MEASURED_CROSSOVER: &str = "E_WIRING_MEASURED_CROSSOVER";
/// 本接线（P_post 分频）只允许的实验布局（4.1.2 五执行器，含 SUB1）。
pub const CROSSOVER_LAYOUT_REQUIRED: &str = "4.1.2";
/// plan 常规 profile 的单支 taps 上限（T39 REGULAR_PROFILE_TAPS_CAP；extended 不在本次接线内）。
pub const CROSSOVER_TAPS_PLAN_MAX: usize = REGULAR_PROFILE_TAPS_CAP;
/// 规范构造方法（T39 Lead 裁决 2026-11-17）。
pub const CROSSOVER_METHOD_ANALYTIC_PHASE: &str = "analytic_phase";
/// 对照方法（仅量化差异；进入 plan 会被 T39 判据拒收）。
pub const CROSSOVER_METHOD_LINEAR_PHASE_MAGNITUDE: &str = "linear_phase_magnitude";
/// 默认设计衰减（T39）。
pub const CROSSOVER_DEFAULT_ATTENUATION_DB: f64 = 80.0;

fn cross_err(message: impl Into<String>) -> String {
    format!("{E_MEASURED_CROSSOVER}: {}", message.into())
}

fn wrap_t39(error: CrossoverError) -> String {
    cross_err(format!("T39 {}: {}", error.code(), error.detail()))
}

/// 布局政策：只有 4.1.2（FL/FR/SL/SR/SUB1）被视为可承载输出分频的布局。
pub fn layout_accepts_crossover(layout: &str) -> bool {
    layout.trim() == CROSSOVER_LAYOUT_REQUIRED
}

/// 一个显式输出赋值（输出下标 -> 支路）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossoverAssignmentSpec {
    /// 输出通道下标（0-based 容器槽位，与 plan role.out_channel 同域）。
    pub output: u16,
    /// "lowpass" | "highpass"。
    pub branch: String,
}

/// measured spec 的 <code>"crossover"</code> 节（缺省 None = 关闭）。
#[derive(Clone, Debug, PartialEq)]
pub struct CrossoverSpecConfig {
    /// T39 类型字符串（LR2/LR4/LR6/LR8/BW1..BW8；大小写不敏感）。
    pub kind: String,
    /// 交叉点（Hz；T39 参数域 [20, 0.2·fs] 生产档）。
    pub fc_hz: f64,
    /// 两支支撑（[8, 4096]；plan 常规 profile）。
    pub taps: usize,
    /// 设计方法（默认 analytic_phase）。
    pub method: String,
    /// 窗设计衰减（dB；T39 只登记/校验）。
    pub attenuation_db: f64,
    /// 显式输出赋值（**必须非空**：未赋值的输出直通，不是默认 HP）。
    pub assignments: Vec<CrossoverAssignmentSpec>,
}

impl CrossoverSpecConfig {
    /// 解析 spec 的 <code>"crossover"</code> 节；缺省/显式 null = None（关闭）。
    pub fn parse_section(document: &JsonValue) -> Result<Option<Self>, String> {
        let Some(value) = document.get("crossover") else {
            return Ok(None);
        };
        if matches!(value, JsonValue::Null) {
            return Ok(None);
        }
        Self::parse(value).map(Some)
    }

    /// 严格解析（未知 method/branch、缺字段一律拒绝；不猜）。
    pub fn parse(value: &JsonValue) -> Result<Self, String> {
        let kind = value
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cross_err("crossover.type 缺字段/非字符串"))?
            .to_string();
        if CrossoverType::parse(&kind).is_err() {
            // 保留 T39 的稳定错误码文本（未知 type 不降级）。
            return Err(wrap_t39(CrossoverType::parse(&kind).unwrap_err()));
        }
        let fc_hz = value
            .get("freq_hz")
            .or_else(|| value.get("fc_hz"))
            .and_then(|v| v.as_f64())
            .ok_or_else(|| cross_err("crossover.freq_hz 缺字段/非数值"))?;
        let taps_u64 = value
            .get("taps")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| cross_err("crossover.taps 缺字段/非整数"))?;
        let taps = usize::try_from(taps_u64)
            .map_err(|_| cross_err(format!("crossover.taps={taps_u64} 无法转 usize")))?;
        let method = match value.get("method").and_then(|v| v.as_str()) {
            None => CROSSOVER_METHOD_ANALYTIC_PHASE.to_string(),
            Some(text) => {
                let normalized = text.trim().to_ascii_lowercase();
                if normalized == CROSSOVER_METHOD_ANALYTIC_PHASE
                    || normalized == CROSSOVER_METHOD_LINEAR_PHASE_MAGNITUDE
                {
                    normalized
                } else {
                    return Err(cross_err(format!(
                        "crossover.method={text:?} 不支持（analytic_phase|linear_phase_magnitude）"
                    )));
                }
            }
        };
        let attenuation_db = match value.get("attenuation_db") {
            None => CROSSOVER_DEFAULT_ATTENUATION_DB,
            Some(item) => item
                .as_f64()
                .ok_or_else(|| cross_err("crossover.attenuation_db 非数值"))?,
        };
        if let Some(flag) = value.get("experimental").and_then(|v| v.as_bool()) {
            if flag {
                return Err(cross_err(
                    "crossover.experimental=true 不支持：fc>0.2·fs 的实验档不进入 P_post/plan（T39 design_strict 口径）",
                ));
            }
        }
        let assignments_value = value
            .get("assignments")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                cross_err("crossover.assignments 缺字段/非数组（必须显式赋值，无默认 HP）")
            })?;
        if assignments_value.is_empty() {
            return Err(cross_err(
                "crossover.assignments 为空：分频必须显式来自配置（未赋值输出 = 直通，不是默认 HP；空配置无意义）",
            ));
        }
        let mut assignments = Vec::with_capacity(assignments_value.len());
        for (index, entry) in assignments_value.iter().enumerate() {
            let output = entry
                .get("output")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    cross_err(format!(
                        "crossover.assignments[{index}].output 缺字段/非整数"
                    ))
                })?;
            let output = u16::try_from(output).map_err(|_| {
                cross_err(format!("crossover.assignments[{index}].output 越界 u16"))
            })?;
            let branch = entry
                .get("branch")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    cross_err(format!(
                        "crossover.assignments[{index}].branch 缺字段/非字符串"
                    ))
                })?;
            let branch = branch.trim().to_ascii_lowercase();
            if branch != Branch::Lowpass.as_str() && branch != Branch::Highpass.as_str() {
                return Err(cross_err(format!(
                    "crossover.assignments[{index}].branch={branch:?} 不支持（lowpass|highpass；bypass 用“不赋值”表达）"
                )));
            }
            assignments.push(CrossoverAssignmentSpec { output, branch });
        }
        Ok(CrossoverSpecConfig {
            kind,
            fc_hz,
            taps,
            method,
            attenuation_db,
            assignments,
        })
    }

    /// 参数/布局自洽校验（T39 参数域 + plan 常规 profile + 容器槽位）。
    pub fn validate(&self, fs: u32, container_channels: u16) -> Result<(), String> {
        if fs != 48_000 && fs != 96_000 {
            return Err(cross_err(format!(
                "fs={fs} 不是 P1 runtime 采样率（48000/96000）"
            )));
        }
        let kind = CrossoverType::parse(&self.kind).map_err(wrap_t39)?;
        let method = self.t39_method()?;
        let _ = kind;
        let _ = method;
        if self.taps < khaen_designer::crossover::MIN_TAPS || self.taps > CROSSOVER_TAPS_PLAN_MAX
        {
            return Err(cross_err(format!(
                "crossover.taps={} 越界（{}..={CROSSOVER_TAPS_PLAN_MAX}；extended taps 不在本次 plan 接线内）",
                self.taps,
                khaen_designer::crossover::MIN_TAPS
            )));
        }
        if !self.fc_hz.is_finite() {
            return Err(cross_err("crossover.freq_hz 非有限数"));
        }
        if container_channels == 0 {
            return Err(cross_err("container_channels=0，无法承载分频输出赋值"));
        }
        let mut seen = Vec::with_capacity(self.assignments.len());
        for (index, assignment) in self.assignments.iter().enumerate() {
            if assignment.output >= container_channels {
                return Err(cross_err(format!(
                    "crossover.assignments[{index}].output={} 越界（container_channels={container_channels}）",
                    assignment.output
                )));
            }
            if seen.contains(&assignment.output) {
                return Err(cross_err(format!(
                    "crossover.assignments[{index}].output={} 重复赋值（每条输出最多一个 branch）",
                    assignment.output
                )));
            }
            seen.push(assignment.output);
        }
        // fc 生产档/参数域交给 T39 spec.validate（错误码 E_FIR_BAND/E_FIR_XOVER_FC_*）。
        let spec = self.t39_spec(fs as f64)?;
        spec.validate().map_err(wrap_t39)?;
        Ok(())
    }

    /// 规范 JSON（证据/summary 用；字段顺序固定）。
    pub fn to_json(&self) -> JsonValue {
        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                json_object_str([
                    ("output", JsonValue::UInt(assignment.output as u64)),
                    ("branch", JsonValue::String(assignment.branch.clone())),
                ])
            })
            .collect::<Vec<_>>();
        JsonValue::Object(vec![
            ("type".to_string(), JsonValue::String(self.kind.clone())),
            ("freq_hz".to_string(), JsonValue::Number(self.fc_hz)),
            ("taps".to_string(), JsonValue::UInt(self.taps as u64)),
            ("method".to_string(), JsonValue::String(self.method.clone())),
            (
                "attenuation_db".to_string(),
                JsonValue::Number(self.attenuation_db),
            ),
            ("assignments".to_string(), JsonValue::Array(assignments)),
        ])
    }

    fn t39_method(&self) -> Result<CrossoverMethod, String> {
        match self.method.as_str() {
            CROSSOVER_METHOD_ANALYTIC_PHASE => Ok(CrossoverMethod::AnalyticPhase),
            CROSSOVER_METHOD_LINEAR_PHASE_MAGNITUDE => Ok(CrossoverMethod::LinearPhaseMagnitude),
            other => Err(cross_err(format!(
                "crossover.method={other:?} 不支持（analytic_phase|linear_phase_magnitude）"
            ))),
        }
    }

    fn t39_spec(&self, fs: f64) -> Result<CrossoverSpec, String> {
        let kind = CrossoverType::parse(&self.kind).map_err(wrap_t39)?;
        Ok(CrossoverSpec::new(fs, kind, self.fc_hz, self.taps)
            .with_method(self.t39_method()?)
            .with_attenuation(self.attenuation_db))
    }

    /// 每支 taps（供 P_post 预算计算）。
    pub fn branch_taps(&self) -> usize {
        self.taps
    }
}

/// 接线后的分频设计（T39 产物 + 显式输出赋值）。
#[derive(Clone, Debug)]
pub struct CrossoverWiring {
    /// 原始配置。
    pub config: CrossoverSpecConfig,
    /// T39 校验后的设计参数。
    pub validated: ValidatedCrossoverSpec,
    /// T39 设计产物（系数 + 判据 + 固有延迟）。
    pub design: CrossoverDesign,
    /// 每输出编译结果（None = 直通）。
    pub output_plan: OutputPlan,
    /// 显式赋值（output, branch）——与 output_plan 同源。
    pub assignments: Vec<(u16, Branch)>,
    /// 设计报告 canonical sha256（D7 证据绑定）。
    pub design_report_sha256: String,
    /// 配置 canonical sha256（证据）。
    pub config_sha256: String,
}

impl CrossoverWiring {
    /// 由配置构造：T39 <code>design_strict</code> -> 判据必须全过 -> 显式输出赋值。
    pub fn build(
        config: &CrossoverSpecConfig,
        fs: u32,
        container_channels: u16,
    ) -> Result<Self, String> {
        config.validate(fs, container_channels)?;
        let spec = config.t39_spec(fs as f64)?;
        let validated = spec.validate().map_err(wrap_t39)?;
        let design = design_strict(&spec).map_err(wrap_t39)?;
        let assignments: Vec<(u16, Branch)> = config
            .assignments
            .iter()
            .map(|assignment| {
                let branch = if assignment.branch == Branch::Lowpass.as_str() {
                    Branch::Lowpass
                } else {
                    Branch::Highpass
                };
                (assignment.output, branch)
            })
            .collect();
        let output_assignments: Vec<OutputAssignment> = assignments
            .iter()
            .map(|(output, branch)| match branch {
                Branch::Lowpass => OutputAssignment::lowpass(*output as usize),
                Branch::Highpass => OutputAssignment::highpass(*output as usize),
            })
            .collect();
        let output_plan = plan_outputs(&design, container_channels as usize, &output_assignments)
            .map_err(wrap_t39)?;
        let design_report_sha256 = design_report_sha256(&design);
        let config_sha256 = sha256_hex(canonical_json_text(&config.to_json()).as_bytes());
        Ok(CrossoverWiring {
            config: config.clone(),
            validated,
            design,
            output_plan,
            assignments,
            design_report_sha256,
            config_sha256,
        })
    }

    /// 输出槽位对应的支路（None = 直通 / 未赋值）。
    pub fn branch_for_output(&self, output: u16) -> Option<Branch> {
        self.assignments
            .iter()
            .find(|(slot, _)| *slot == output)
            .map(|(_, branch)| *branch)
    }

    /// 输出槽位的支路名（证据用）。
    pub fn branch_name_for_output(&self, output: u16) -> Option<&'static str> {
        self.branch_for_output(output).map(Branch::as_str)
    }

    /// 输出槽位的分频 FIR 系数（None = 直通）。
    pub fn kernel_for_output(&self, output: u16) -> Option<&[f64]> {
        self.output_plan.filter(output as usize)
    }

    /// 判据是否全过（INFEASIBLE 时 build 已拒收；此处只做证据回读）。
    pub fn normative_pass(&self) -> bool {
        self.design.accepted()
    }

    /// 失败判据 id（证据）。
    pub fn failed_criteria(&self) -> Vec<String> {
        self.design
            .failed_criteria()
            .iter()
            .map(|id| (*id).to_string())
            .collect()
    }

    /// 两支声明体延迟（samples；相对 D 恒 0）。
    pub fn declared_delay_samples(&self) -> f64 {
        self.design.declared_delay_samples()
    }

    /// 证据 summary JSON（measured summary / wiring summary 引用）。
    pub fn summary_json(&self) -> JsonValue {
        let assignments = self
            .assignments
            .iter()
            .map(|(output, branch)| {
                json_object_str([
                    ("output", JsonValue::UInt(*output as u64)),
                    ("branch", JsonValue::String(branch.as_str().to_string())),
                    (
                        "taps",
                        JsonValue::UInt(self.design.coeffs(*branch).len() as u64),
                    ),
                    ("bypass", JsonValue::Bool(false)),
                ])
            })
            .collect::<Vec<_>>();
        let criteria = self
            .design
            .criteria
            .criteria
            .iter()
            .map(|criterion| {
                json_object_str([
                    ("id", JsonValue::String(criterion.id.to_string())),
                    ("pass", JsonValue::Bool(criterion.pass)),
                    (
                        "measured",
                        match criterion.measured {
                            Some(value) => JsonValue::Number(value),
                            None => JsonValue::Null,
                        },
                    ),
                    (
                        "limit",
                        match criterion.limit {
                            Some(value) => JsonValue::Number(value),
                            None => JsonValue::Null,
                        },
                    ),
                    ("unit", JsonValue::String(criterion.unit.to_string())),
                ])
            })
            .collect::<Vec<_>>();
        let warnings = self
            .design
            .warnings
            .iter()
            .map(|warning| JsonValue::String(warning.clone()))
            .collect::<Vec<_>>();
        JsonValue::Object(vec![
            (
                "schema".to_string(),
                JsonValue::String(CROSSOVER_WIRING_SCHEMA.to_string()),
            ),
            ("config".to_string(), self.config.to_json()),
            (
                "config_sha256".to_string(),
                JsonValue::String(self.config_sha256.clone()),
            ),
            (
                "design_report_sha256".to_string(),
                JsonValue::String(self.design_report_sha256.clone()),
            ),
            (
                "kind".to_string(),
                JsonValue::String(self.design.kind.as_str().to_string()),
            ),
            (
                "method".to_string(),
                JsonValue::String(self.design.method.as_str().to_string()),
            ),
            (
                "fc_tier".to_string(),
                JsonValue::String(self.design.fc_tier.to_string()),
            ),
            (
                "fc_hz".to_string(),
                JsonValue::Number(self.design.spec.fc_hz),
            ),
            (
                "taps".to_string(),
                JsonValue::UInt(self.design.spec.taps as u64),
            ),
            (
                "declared_delay_samples".to_string(),
                JsonValue::Number(self.design.declared_delay_samples()),
            ),
            (
                "relative_delay_samples".to_string(),
                JsonValue::Number(self.design.merge.relative_delay_samples),
            ),
            (
                "relative_delay_is_integer".to_string(),
                JsonValue::Bool(self.design.merge.relative_delay_is_integer),
            ),
            (
                "family_relative_polarity_deg".to_string(),
                JsonValue::Number(self.design.merge.family_relative_polarity_deg),
            ),
            (
                "polarity_source".to_string(),
                JsonValue::String(self.design.merge.polarity_source.to_string()),
            ),
            (
                "merge_flat_deviation_db".to_string(),
                JsonValue::Number(self.design.merge.flat_max_deviation_db),
            ),
            (
                "merge_flat_limit_db".to_string(),
                JsonValue::Number(self.design.merge.flat_limit_db),
            ),
            (
                "merge_flat_pass".to_string(),
                JsonValue::Bool(self.design.merge.flat_pass),
            ),
            (
                "lowpass_max_abs_h".to_string(),
                JsonValue::Number(self.design.lowpass.max_abs_h),
            ),
            (
                "highpass_max_abs_h".to_string(),
                JsonValue::Number(self.design.highpass.max_abs_h),
            ),
            (
                "lowpass_sum_abs_h".to_string(),
                JsonValue::Number(self.design.lowpass.sum_abs_h),
            ),
            (
                "highpass_sum_abs_h".to_string(),
                JsonValue::Number(self.design.highpass.sum_abs_h),
            ),
            (
                "normative_pass".to_string(),
                JsonValue::Bool(self.normative_pass()),
            ),
            ("assignments".to_string(), JsonValue::Array(assignments)),
            ("criteria".to_string(), JsonValue::Array(criteria)),
            ("warnings".to_string(), JsonValue::Array(warnings)),
        ])
    }
}

/// 段组合结果：卷积核 + 段来源（规范序）。
#[derive(Clone, Debug, PartialEq)]
pub struct ComposedPPost {
    /// 组合后的 P_post 系数（crossover ⊛ target）。
    pub kernel: Vec<f64>,
    /// 段来源（空 = 未组合，kernel 即单段/identity）。
    pub segments: Vec<FirSegmentKind>,
}

/// 按 core 规范段序（crossover → target）组合一个 role 的 P_post。
///
/// - crossover=None：返回 target 原样（无 target 时 identity），segments 为空 => 旧行为逐位不变。
/// - target=<code>[1.0]</code>（T64 目标 FIR 关闭）：不生成 Target 段。
pub fn compose_role_p_post(
    crossover: Option<&[f64]>,
    target: Option<&[f64]>,
) -> Result<ComposedPPost, String> {
    let target_segment = target.filter(|coeffs| !(coeffs.len() == 1 && coeffs[0] == 1.0));
    let mut segments: Vec<FirSegment> = Vec::with_capacity(2);
    if let Some(coeffs) = crossover {
        segments.push(FirSegment::new(
            FirSegmentKind::Crossover,
            coeffs.to_vec(),
            0,
            0,
        ));
    }
    if let Some(coeffs) = target_segment {
        segments.push(FirSegment::new(
            FirSegmentKind::Target,
            coeffs.to_vec(),
            0,
            0,
        ));
    }
    let kernel = compose_segments(&segments)?;
    Ok(ComposedPPost {
        kernel,
        segments: segments.iter().map(|segment| segment.kind).collect(),
    })
}

/// 规范段序卷积（core <code>compose_fir_segments</code>；空段 = identity）。
pub fn compose_segments(segments: &[FirSegment]) -> Result<Vec<f64>, String> {
    let mut previous_rank: Option<u8> = None;
    let mut seen: Vec<FirSegmentKind> = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        if segment.coefficients.is_empty() {
            return Err(cross_err(format!("segments[{index}] 系数为空")));
        }
        if segment.coefficients.iter().any(|value| !value.is_finite()) {
            return Err(cross_err(format!("segments[{index}] 系数含非有限值")));
        }
        if segment.taps as usize != segment.coefficients.len() {
            return Err(cross_err(format!(
                "segments[{index}].taps={} 与系数长度 {} 不一致",
                segment.taps,
                segment.coefficients.len()
            )));
        }
        if seen.contains(&segment.kind) {
            return Err(cross_err(format!(
                "段 {} 重复（每 role 每段类型最多一个）",
                segment.kind.schema_name()
            )));
        }
        let rank = segment.kind.rank();
        if let Some(previous) = previous_rank {
            if rank < previous {
                return Err(cross_err(format!(
                    "段序非法：{} 必须在规范序 crossover→target→phase 内非降（core nodes FirSegmentKind::rank）",
                    segment.kind.schema_name()
                )));
            }
        }
        previous_rank = Some(rank);
        seen.push(segment.kind);
    }
    compose_fir_segments(segments).map_err(|status| {
        cross_err(format!(
            "core compose_fir_segments 失败（status {status:?}）：段序/系数非法"
        ))
    })
}

/// 组合核 canonical sha256（D7 证据/回归用；f64 17 位有效数字，确定性）。
pub fn kernel_sha256(kernel: &[f64]) -> String {
    let mut canonical = String::with_capacity(kernel.len() * 26 + 64);
    canonical.push_str("tspi-cal-p-post-kernel/v1\n");
    let _ = writeln!(canonical, "taps={}", kernel.len());
    for value in kernel {
        let _ = writeln!(canonical, "{value:.17e}");
    }
    sha256_hex(canonical.as_bytes())
}

/// T39 设计报告 canonical sha256（判据/合并/延迟/系数摘要绑定）。
pub fn design_report_sha256(design: &CrossoverDesign) -> String {
    let mut text = String::with_capacity(4096);
    text.push_str("tspi-cal-crossover-report/v1\n");
    let _ = writeln!(
        text,
        "kind={} method={} fs={} fc={:.17e} taps={} fc_tier={} experimental={} attenuation={:.17e}",
        design.kind.as_str(),
        design.method.as_str(),
        design.spec.fs,
        design.spec.fc_hz,
        design.spec.taps,
        design.fc_tier,
        design.spec.experimental,
        design.spec.attenuation_db
    );
    let _ = writeln!(
        text,
        "declared_delay={:.17e} relative_delay={:.17e} relative_integer={} polarity_lp={} polarity_hp={} polarity_source={} family_rel={:.17e}",
        design.declared_delay_samples(),
        design.merge.relative_delay_samples,
        design.merge.relative_delay_is_integer,
        design.merge.polarity.lowpass.as_str(),
        design.merge.polarity.highpass.as_str(),
        design.merge.polarity_source,
        design.merge.family_relative_polarity_deg
    );
    let _ = writeln!(
        text,
        "merge_flat={} merge_dev={:.17e} merge_limit={:.17e} analytic_flat={} analytic_dev={:.17e} analytic_phase_fc={:.17e} measured_phase_fc={:.17e}",
        design.merge.flat_pass,
        design.merge.flat_max_deviation_db,
        design.merge.flat_limit_db,
        design.merge.analytic_sum_flat,
        design.merge.analytic_sum_max_deviation_db,
        design.merge.analytic_relative_phase_at_fc_deg,
        design.merge.measured_relative_phase_at_fc_deg
    );
    for branch in [&design.lowpass, &design.highpass] {
        let _ = writeln!(
            text,
            "branch {} taps={} polarity={} max_abs_h={:.17e} sum_abs_h={:.17e} dc_gain_db={:.17e} nyquist_db={:.17e} mag_err={:.17e} phase_err={:.17e}",
            branch.branch.as_str(),
            branch.taps,
            branch.applied_polarity.as_str(),
            branch.max_abs_h,
            branch.sum_abs_h,
            branch.dc_gain_db,
            branch.nyquist_mag_db,
            branch.magnitude.max_abs_err_db,
            branch.phase.max_abs_err_deg
        );
        let _ = writeln!(
            text,
            "branch {} coeff_sha256={}",
            branch.branch.as_str(),
            kernel_sha256(&branch.coefficients)
        );
    }
    for criterion in &design.criteria.criteria {
        let _ = writeln!(
            text,
            "criterion {} pass={} measured={:?} limit={:?} unit={}",
            criterion.id, criterion.pass, criterion.measured, criterion.limit, criterion.unit
        );
    }
    for warning in &design.warnings {
        let _ = writeln!(text, "warning={warning}");
    }
    sha256_hex(text.as_bytes())
}

fn canonical_json_text(value: &JsonValue) -> String {
    match json_encode_str(value) {
        Ok(text) => text,
        Err(error) => format!("{{\"error\":\"{error}\"}}"),
    }
}

// ---------------------------------------------------------------------------
// JSON 小工具（本模块内固定字段顺序；避免依赖调用方包装）
// ---------------------------------------------------------------------------

/// 固定字段顺序的对象（元组数组 -> json_object）。
fn json_object_str<const N: usize>(pairs: [(&'static str, JsonValue); N]) -> JsonValue {
    khaen_compat::ws::json_object(pairs.into_iter().collect())
}

/// JSON 编码为字符串（错误转人读文本）。
fn json_encode_str(value: &JsonValue) -> Result<String, String> {
    khaen_compat::ws::json_encode(value).map_err(|error| error.to_string())
}
