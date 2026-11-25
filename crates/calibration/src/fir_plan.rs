//! mimo 单点 FIR -> plan ABI v3：每 lane 一条 crossover FIR，其余段恒等。
//!
//! 输入是 mimo 单点求解产出的时域冲激响应（FIR CSV 里的 taps）。本模块把它编译成可下发、
//! 可预测、可回滚的方案文件：
//!
//! - 内容布局 / 矩阵 / 保护 / 路由 = 恒等（保护项全 0 dB、无 PEQ）；
//! - crossover = 每 lane 一段 FIR（未提供的 lane 保持恒等直通）；
//! - 输出确定性 bytes / sha256 / graph_id：同输入逐字节相同。
//!
//! 这样 mimo 的质量门/权威加权/安全钳位结果第一次成为**可部署**的方案，而不再是只活在
//! 报告里的 CSV。诚实边界：本模块只负责「FIR 系数 -> 方案」；FIR 的来源（合成场景或真机
//! 测量）与真机下发/复测属于 T42 设备侧流程（NOT_RUN）。

use khaen_core::plan::{CrossoverFilter, Plan, MAX_CROSSOVER_TAPS};
use khaen_core::rt::{BLOCK_SIZES, SAMPLE_RATES_HZ};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

/// 输入错误：lane 引用/重复、taps 为空或非有限、taps 超 ABI 上限。
pub const BAD_INPUT: &str = "E_FIR_PLAN_INPUT";
/// 配置错误：块长/采样率/通道数非法。
pub const CONFIG: &str = "E_FIR_PLAN_CONFIG";
/// 方案组装错误（PlanBuilder 拒绝）。
pub const BUILD: &str = "E_FIR_PLAN_BUILD";
/// 方案编码错误（ABI 边界，正常不可达）。
pub const ENCODE: &str = "E_FIR_PLAN_ENCODE";

/// FIR 编译错误（稳定码 + 人读消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirPlanError {
    code: &'static str,
    message: String,
}

impl FirPlanError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for FirPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FirPlanError {}

/// 一条 lane 的 FIR。
#[derive(Clone, Debug, PartialEq)]
pub struct LaneFir {
    /// lane 序号（< channels）。
    pub lane: u16,
    /// 时域抽头（因果冲激响应）。
    pub taps: Vec<f64>,
}

/// 编译配置（与运行时能力对齐）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirPlanConfig {
    /// 每块帧数（必须是 khaen_core::rt::BLOCK_SIZES 的成员）。
    pub block_size: u32,
    /// 采样率（Hz，必须是 48000 / 96000）。
    pub sample_rate: u32,
    /// 运行时通道数（= lane 数，必须 >= 提供的 FIR 条数）。
    pub channels: u16,
}

impl FirPlanConfig {
    /// 显式构造。
    pub const fn new(block_size: u32, sample_rate: u32, channels: u16) -> Self {
        Self {
            block_size,
            sample_rate,
            channels,
        }
    }
}

/// 编译产物（方案 + 字节 + 摘要）。
#[derive(Clone, Debug)]
pub struct CompiledFirPlan {
    plan: Plan,
    bytes: Vec<u8>,
    sha256: String,
    graph_id: String,
}

impl CompiledFirPlan {
    /// 解码后的规范方案。
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// 方案文件字节。
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 方案文件 sha256（十六进制）。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// 方案 graph_id（十六进制）。
    pub fn graph_id(&self) -> &str {
        &self.graph_id
    }
}

/// 把每 lane 的 FIR 编译成方案：crossover 承载 FIR，其余段恒等。
pub fn compile_fir_plan(
    lanes: &[LaneFir],
    config: &FirPlanConfig,
) -> Result<CompiledFirPlan, FirPlanError> {
    validate_config(config)?;
    if lanes.is_empty() {
        return Err(FirPlanError::new(
            BAD_INPUT,
            "at least one lane FIR is required",
        ));
    }
    let channels = usize::from(config.channels);
    if lanes.len() > channels {
        return Err(FirPlanError::new(
            CONFIG,
            format!(
                "{} lane FIRs cannot fit into {channels} channels",
                lanes.len()
            ),
        ));
    }
    let mut seen = vec![false; channels];
    let mut filters = Vec::with_capacity(lanes.len());
    for fir in lanes {
        let lane = usize::from(fir.lane);
        if lane >= channels {
            return Err(FirPlanError::new(
                BAD_INPUT,
                format!("lane {} is outside 0..{channels}", fir.lane),
            ));
        }
        if seen[lane] {
            return Err(FirPlanError::new(
                BAD_INPUT,
                format!("duplicate FIR for lane {}", fir.lane),
            ));
        }
        seen[lane] = true;
        if fir.taps.is_empty() {
            return Err(FirPlanError::new(
                BAD_INPUT,
                format!("lane {} FIR has no taps", fir.lane),
            ));
        }
        if fir.taps.len() > MAX_CROSSOVER_TAPS as usize {
            return Err(FirPlanError::new(
                BAD_INPUT,
                format!(
                    "lane {} FIR has {} taps (ABI v3 limit {})",
                    fir.lane,
                    fir.taps.len(),
                    MAX_CROSSOVER_TAPS
                ),
            ));
        }
        for (index, &tap) in fir.taps.iter().enumerate() {
            if !tap.is_finite() || tap.abs() > f64::from(f32::MAX) {
                return Err(FirPlanError::new(
                    BAD_INPUT,
                    format!(
                        "lane {} tap {index} is not representable as f32 ({tap})",
                        fir.lane
                    ),
                ));
            }
        }
        filters.push(CrossoverFilter {
            lane: fir.lane,
            coeffs: fir.taps.iter().map(|&tap| tap as f32).collect(),
        });
    }
    let plan = PlanBuilder::new(PlanConfig::new(
        config.block_size,
        config.sample_rate,
        config.channels,
    ))
    .set_content_identity(config.channels)
    .set_crossover_filters(config.channels, filters)
    .set_matrix_identity(config.channels)
    .set_protection_identity(config.channels)
    .set_routing_identity(config.channels)
    .build()
    .map_err(|err| FirPlanError::new(BUILD, format!("{} ({})", err, err.code().as_str())))?;
    let bytes = plan
        .encode_checked()
        .map_err(|err| FirPlanError::new(ENCODE, format!("{err}")))?;
    let sha256 = khaen_core::sha256::sha256_hex(&bytes);
    let decoded =
        Plan::decode(&bytes).map_err(|err| FirPlanError::new(ENCODE, format!("{err}")))?;
    let graph_id = decoded.graph_id_hex();
    Ok(CompiledFirPlan {
        plan: decoded,
        bytes,
        sha256,
        graph_id,
    })
}

fn validate_config(config: &FirPlanConfig) -> Result<(), FirPlanError> {
    let block_ok = u16::try_from(config.block_size)
        .map(|block_size| BLOCK_SIZES.contains(&block_size))
        .unwrap_or(false);
    if !block_ok {
        return Err(FirPlanError::new(
            CONFIG,
            format!(
                "block_size {} is not a supported block size ({BLOCK_SIZES:?})",
                config.block_size
            ),
        ));
    }
    if !SAMPLE_RATES_HZ.contains(&config.sample_rate) {
        return Err(FirPlanError::new(
            CONFIG,
            format!(
                "sample_rate {} is not supported ({SAMPLE_RATES_HZ:?})",
                config.sample_rate
            ),
        ));
    }
    if config.channels == 0 {
        return Err(FirPlanError::new(CONFIG, "channels must be >= 1"));
    }
    Ok(())
}
