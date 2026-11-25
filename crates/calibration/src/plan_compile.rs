//! 方案编译（T66 的 v2 部分）：把设计/校准产物编译成 plan ABI v3 字节，并给出可复现摘要。
//!
//! 输入是生产链路里已有的 `khaen_designer::modal_peq::PeqDesign`（每通道对模态频段的
//! RBJ biquad 链）；输出是 **合法的方案字节 + sha256 + graph_id**，可直接交给 daemon 的
//! prepare（再准入/图编译/块边界 commit）。
//!
//! 职责边界：
//! - 只做“设计 → 方案”的确定性组装与边界校验；不做测量、不做拟合、不碰实时路径；
//! - 校验复用 `khaen_plan_builder`（它负责 ABI 上限与 lane 链一致性），本模块不重复实现；
//! - 产物不含时间戳/主机名/随机数：同一输入永远得到同一字节与同一 sha（可复现）。
//!
//! lane 约定（与 host 的图结构一致）：设计里的 `ChannelPeq::index` 就是目标 lane 号，
//! 必须互不重复且落在 `[0, channels)`；没有对应通道的 lane 得到恒等（无 PEQ）保护项。

use std::fmt;

use khaen_core::plan::{PeqSection, Plan, ProtectionEntry, ProtectionSection};
use khaen_core::rt::{BLOCK_SIZES, SAMPLE_RATES_HZ};
use khaen_designer::modal_peq::PeqDesign;
use khaen_plan_builder::{PlanBuilder, PlanConfig};

use crate::job::sha256_hex;

/// 方案字节在 job artifacts_dir 下的相对路径（与 calibration job 的 artifact 语义一致）。
pub const PLAN_ARTIFACT_PATH: &str = "plan/plan.bin";

/// 稳定错误码（可被 REST / manifest 直接匹配，不要改字符串）。
pub mod error {
    /// 编译选项非法（块长/速率不在运行时集合、通道数为 0、gain/delay 长度不匹配）。
    pub const BAD_OPTIONS: &str = "bad_options";
    /// 设计里没有任何通道。
    pub const EMPTY_DESIGN: &str = "empty_design";
    /// 设计的采样率与编译选项不一致。
    pub const SAMPLE_RATE_MISMATCH: &str = "sample_rate_mismatch";
    /// 通道 index（= lane）越界。
    pub const LANE_OUT_OF_RANGE: &str = "lane_out_of_range";
    /// 两个通道映射到同一个 lane。
    pub const DUPLICATE_LANE: &str = "duplicate_lane";
    /// biquad 未归一化（a0 必须为 1）。
    pub const NON_NORMALIZED_BIQUAD: &str = "non_normalized_biquad";
    /// 系数含 NaN/Infinity 或超出 f32 范围。
    pub const NON_FINITE_COEFFICIENT: &str = "non_finite_coefficient";
    /// plan-builder 的组装校验拒绝（错误码来自它，message 带细节）。
    pub const BUILD_REJECTED: &str = "build_rejected";
    /// 组装出的方案无法编码（ABI 边界，正常不可达）。
    pub const ENCODE_FAILED: &str = "encode_failed";
}

/// 方案编译错误：稳定码 + 给人看的说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCompileError {
    code: &'static str,
    message: String,
}

impl PlanCompileError {
    /// 新建错误。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 给人看的说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PlanCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PlanCompileError {}

/// 编译选项（与运行时配置对齐；块长/速率必须是运行时能力集合的成员）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanCompileOptions {
    /// 采样率（Hz）；必须等于设计的 `fs`。
    pub sample_rate: u32,
    /// 每块帧数。
    pub block_size: u32,
    /// 运行时通道数（输入通道数 = DAC 通道数 = lane 数，host 约束）。
    pub channels: u16,
    /// 每 lane 的静态增益（dB）；空 = 全 0。
    pub lane_gain_db: Vec<f32>,
    /// 每 lane 的延迟（样本）；空 = 全 0。
    pub lane_delay: Vec<u32>,
}

impl Default for PlanCompileOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            block_size: 128,
            channels: 2,
            lane_gain_db: Vec::new(),
            lane_delay: Vec::new(),
        }
    }
}

impl PlanCompileOptions {
    fn validate(&self) -> Result<(), PlanCompileError> {
        if self.channels == 0 {
            return Err(PlanCompileError::new(
                error::BAD_OPTIONS,
                "channels must be positive",
            ));
        }
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate) {
            return Err(PlanCompileError::new(
                error::BAD_OPTIONS,
                format!(
                    "sample_rate {} is not a runtime rate: {:?}",
                    self.sample_rate, SAMPLE_RATES_HZ
                ),
            ));
        }
        let block_size_supported =
            u16::try_from(self.block_size).is_ok_and(|size| BLOCK_SIZES.contains(&size));
        if !block_size_supported {
            return Err(PlanCompileError::new(
                error::BAD_OPTIONS,
                format!(
                    "block_size {} is not an ABI block size: {:?}",
                    self.block_size, BLOCK_SIZES
                ),
            ));
        }
        let channels = usize::from(self.channels);
        if !self.lane_gain_db.is_empty() && self.lane_gain_db.len() != channels {
            return Err(PlanCompileError::new(
                error::BAD_OPTIONS,
                format!(
                    "lane_gain_db has {} entries, expected 0 or {channels}",
                    self.lane_gain_db.len()
                ),
            ));
        }
        if !self.lane_delay.is_empty() && self.lane_delay.len() != channels {
            return Err(PlanCompileError::new(
                error::BAD_OPTIONS,
                format!(
                    "lane_delay has {} entries, expected 0 or {channels}",
                    self.lane_delay.len()
                ),
            ));
        }
        for (lane, gain) in self.lane_gain_db.iter().enumerate() {
            if !gain.is_finite() {
                return Err(PlanCompileError::new(
                    error::NON_FINITE_COEFFICIENT,
                    format!("lane_gain_db[{lane}] is not finite"),
                ));
            }
        }
        Ok(())
    }
}

/// 编译结果：合法方案 + 可直接落盘/交给 daemon 的字节与摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledPlan {
    /// 组装好的方案（未经 ABI 编码前的结构）。
    pub plan: Plan,
    /// ABI v3 字节（`Plan::encode_checked` 产物，可复现）。
    pub bytes: Vec<u8>,
    /// `bytes` 的 sha256（64 位小写十六进制）。
    pub sha256: String,
    /// 方案 graph_id（ABI v3 契约：sha256("khaen-plan/v3"‖body) 的十六进制）。
    pub graph_id: String,
}

impl CompiledPlan {
    /// artifact 相对路径（写 job artifacts_dir 时使用）。
    pub fn artifact_path(&self) -> &'static str {
        PLAN_ARTIFACT_PATH
    }
}

/// 把一个模态 PEQ 设计编译成 plan ABI v3 字节。
///
/// 确定性：同一 `design` + 同一 `options` 永远产出同一 `bytes` / `sha256` / `graph_id`。
pub fn compile_peq_design(
    design: &PeqDesign,
    options: &PlanCompileOptions,
) -> Result<CompiledPlan, PlanCompileError> {
    options.validate()?;
    if design.channels.is_empty() {
        return Err(PlanCompileError::new(
            error::EMPTY_DESIGN,
            "design has no channels",
        ));
    }
    if design.fs != options.sample_rate {
        return Err(PlanCompileError::new(
            error::SAMPLE_RATE_MISMATCH,
            format!(
                "design fs {} does not match plan sample_rate {}",
                design.fs, options.sample_rate
            ),
        ));
    }

    let channels = usize::from(options.channels);
    let mut channel_by_lane: Vec<Option<usize>> = vec![None; channels];
    for (position, channel) in design.channels.iter().enumerate() {
        let lane = usize::try_from(channel.index).map_err(|_| {
            PlanCompileError::new(
                error::LANE_OUT_OF_RANGE,
                format!(
                    "channel[{position}] index {} does not fit in usize",
                    channel.index
                ),
            )
        })?;
        if lane >= channels {
            return Err(PlanCompileError::new(
                error::LANE_OUT_OF_RANGE,
                format!(
                    "channel[{}] {} maps to lane {lane}, only {channels} lanes exist",
                    position, channel.name
                ),
            ));
        }
        if channel_by_lane[lane].is_some() {
            return Err(PlanCompileError::new(
                error::DUPLICATE_LANE,
                format!("two design channels map to lane {lane}"),
            ));
        }
        channel_by_lane[lane] = Some(position);
    }

    let mut entries = Vec::with_capacity(channels);
    for lane in 0..channels {
        let mut peq = Vec::new();
        if let Some(position) = channel_by_lane[lane] {
            for (band_index, band) in design.channels[position].bands.iter().enumerate() {
                let label = format!("lane {lane} band {band_index}");
                let values = [band.b[0], band.b[1], band.b[2], band.a[1], band.a[2]];
                for (coefficient_index, value) in values.iter().enumerate() {
                    if !value.is_finite() {
                        return Err(PlanCompileError::new(
                            error::NON_FINITE_COEFFICIENT,
                            format!("{label} coefficient {coefficient_index} is not finite"),
                        ));
                    }
                    if value.abs() > f64::from(f32::MAX) {
                        return Err(PlanCompileError::new(
                            error::NON_FINITE_COEFFICIENT,
                            format!("{label} coefficient {coefficient_index} does not fit in f32",),
                        ));
                    }
                }
                if (band.a[0] - 1.0).abs() > 1e-12 {
                    return Err(PlanCompileError::new(
                        error::NON_NORMALIZED_BIQUAD,
                        format!("{label} a0 = {} (must be 1)", band.a[0]),
                    ));
                }
                peq.push(PeqSection {
                    b0: band.b[0] as f32,
                    b1: band.b[1] as f32,
                    b2: band.b[2] as f32,
                    a1: band.a[1] as f32,
                    a2: band.a[2] as f32,
                });
            }
        }
        let gain_db = options.lane_gain_db.get(lane).copied().unwrap_or(0.0);
        let delay = options.lane_delay.get(lane).copied().unwrap_or(0);
        entries.push(ProtectionEntry {
            lane: lane as u16,
            delay,
            polarity: 1,
            gain_db,
            peq,
        });
    }

    let builder = PlanBuilder::new(PlanConfig {
        block_size: options.block_size,
        sample_rate: options.sample_rate,
        channels: options.channels,
    })
    .set_content_identity(options.channels)
    .set_crossover_identity(options.channels)
    .set_matrix_identity(options.channels)
    .set_protection(ProtectionSection {
        lanes: options.channels,
        entries,
    })
    .set_routing_identity(options.channels);
    let plan = builder.build().map_err(|err| {
        PlanCompileError::new(
            error::BUILD_REJECTED,
            format!("{} ({}, stage {:?})", err, err.code().as_str(), err.stage()),
        )
    })?;
    let bytes = plan.encode_checked().map_err(|err| {
        PlanCompileError::new(
            error::ENCODE_FAILED,
            format!("plan cannot be encoded: {err}"),
        )
    })?;
    let sha256 = sha256_hex(&bytes);
    let graph_id = plan.graph_id_hex();
    Ok(CompiledPlan {
        plan,
        bytes,
        sha256,
        graph_id,
    })
}
