//! 组装错误的稳定错误码与错误类型。
//!
//! 调用方按 [`PlanBuildErrorCode`] match，不解析字符串；`detail` 只给人看。
//! `as_str()` 给出稳定的小写下划线编码，用于日志与控制面序列化（同一编码不随文案变化）。

use core::fmt;

/// 出错的组装阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuildStage {
    /// 方案级配置（块大小 / 采样率 / 通道数）。
    Config,
    /// content 区段（输入通道到 lane 的布局与折叠）。
    Content,
    /// crossover 区段（每 lane 的分频滤波器）。
    Crossover,
    /// matrix 区段（跨 lane 满矩阵）。
    Matrix,
    /// protection 区段（每 lane 延迟 / 极性 / 电平 / PEQ）。
    Protection,
    /// routing 区段（lane 到 DAC 通道的路由）。
    Routing,
}

impl BuildStage {
    /// 稳定的小写下划线编码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Content => "content",
            Self::Crossover => "crossover",
            Self::Matrix => "matrix",
            Self::Protection => "protection",
            Self::Routing => "routing",
        }
    }
}

impl fmt::Display for BuildStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 稳定的组装错误码。
///
/// 每个错误码只对应一类可判定的失败；调用方可以据此决定回滚、提示或拒绝提交。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlanBuildErrorCode {
    /// 采样率不在运行时契约支持的速率里（`khaen_core::rt::SAMPLE_RATES_HZ`）。
    UnsupportedSampleRate,
    /// 块大小不在运行时契约支持的块大小里（`khaen_core::rt::BLOCK_SIZES`）。
    UnsupportedBlockSize,
    /// 通道数为 0（config.channels / content.in_channels / routing.dac_channels）。
    ZeroChannelCount,
    /// lane 数为 0（content.out_lanes / crossover.lanes / protection.lanes）。
    ZeroLaneCount,
    /// 方案没有 matrix 区段；host 图执行器必须要有矩阵。
    MissingMatrix,
    /// 通道数与运行时配置不一致（content.in_channels、content 缺省推导、routing.dac_channels）。
    ChannelCountMismatch,
    /// 相邻区段的 lane 数不衔接（crossover / matrix / protection）。
    LaneCountMismatch,
    /// 折叠表项数超过 ABI v3 的 u16 计数上限。
    TooManyFolds,
    /// folding 的源通道越界。
    FoldSourceOutOfRange,
    /// folding 的目标 lane 越界。
    FoldDestinationOutOfRange,
    /// folding 增益不是有限数（NaN / ±inf）。
    NonFiniteGain,
    /// 分频滤波器数量超过 ABI v3 的 u16 计数上限。
    TooManyCrossoverFilters,
    /// 分频滤波器的目标 lane 越界。
    CrossoverLaneOutOfRange,
    /// 同一 lane 出现多个分频滤波器（host 禁止）。
    DuplicateCrossoverLane,
    /// 分频滤波器没有任何抽头。
    EmptyCrossoverFilter,
    /// 分频滤波器的抽头数超过 ABI v3 解码器上限。
    CrossoverTapsTooLong,
    /// 矩阵行或列为 0。
    MatrixDimensionsZero,
    /// 矩阵每格抽头数为 0。
    MatrixTapsZero,
    /// 矩阵系数数量与 `rows * cols * taps` 不符。
    MatrixCoefficientCountMismatch,
    /// 矩阵系数数量（`rows * cols * taps`）超过 ABI v3 解码器上限。
    MatrixCoefficientsTooMany,
    /// 矩阵抽取或插值因子为 0。
    MatrixRateZero,
    /// 保护项数量超过 ABI v3 的 u16 计数上限。
    TooManyProtectionEntries,
    /// 保护项的目标 lane 越界。
    ProtectionLaneOutOfRange,
    /// 同一 lane 出现多条保护项（host 禁止）。
    DuplicateProtectionLane,
    /// 极性不是 +1 或 -1。
    InvalidPolarity,
    /// 保护电平（dB）不是有限数。
    NonFiniteGainDb,
    /// 单 lane PEQ 段数超过 ABI v3 的 u16 计数上限。
    TooManyPeqSections,
    /// 系数（分频抽头 / PEQ 系数）不是有限数。
    NonFiniteCoefficient,
    /// 路由表项数超过 ABI v3 的 u16 计数上限。
    TooManyRoutes,
    /// 路由引用了不存在的 lane。
    RouteLaneOutOfRange,
    /// 路由引用了不存在的 DAC 通道。
    RouteDacOutOfRange,
    /// 同一 DAC 通道被多条路由占用（host 禁止）。
    DuplicateRouteDac,
}

impl PlanBuildErrorCode {
    /// 稳定的小写下划线编码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedSampleRate => "unsupported_sample_rate",
            Self::UnsupportedBlockSize => "unsupported_block_size",
            Self::ZeroChannelCount => "zero_channel_count",
            Self::ZeroLaneCount => "zero_lane_count",
            Self::MissingMatrix => "missing_matrix",
            Self::ChannelCountMismatch => "channel_count_mismatch",
            Self::LaneCountMismatch => "lane_count_mismatch",
            Self::TooManyFolds => "too_many_folds",
            Self::FoldSourceOutOfRange => "fold_source_out_of_range",
            Self::FoldDestinationOutOfRange => "fold_destination_out_of_range",
            Self::NonFiniteGain => "non_finite_gain",
            Self::TooManyCrossoverFilters => "too_many_crossover_filters",
            Self::CrossoverLaneOutOfRange => "crossover_lane_out_of_range",
            Self::DuplicateCrossoverLane => "duplicate_crossover_lane",
            Self::EmptyCrossoverFilter => "empty_crossover_filter",
            Self::CrossoverTapsTooLong => "crossover_taps_too_long",
            Self::MatrixDimensionsZero => "matrix_dimensions_zero",
            Self::MatrixTapsZero => "matrix_taps_zero",
            Self::MatrixCoefficientCountMismatch => "matrix_coefficient_count_mismatch",
            Self::MatrixCoefficientsTooMany => "matrix_coefficients_too_many",
            Self::MatrixRateZero => "matrix_rate_zero",
            Self::TooManyProtectionEntries => "too_many_protection_entries",
            Self::ProtectionLaneOutOfRange => "protection_lane_out_of_range",
            Self::DuplicateProtectionLane => "duplicate_protection_lane",
            Self::InvalidPolarity => "invalid_polarity",
            Self::NonFiniteGainDb => "non_finite_gain_db",
            Self::TooManyPeqSections => "too_many_peq_sections",
            Self::NonFiniteCoefficient => "non_finite_coefficient",
            Self::TooManyRoutes => "too_many_routes",
            Self::RouteLaneOutOfRange => "route_lane_out_of_range",
            Self::RouteDacOutOfRange => "route_dac_out_of_range",
            Self::DuplicateRouteDac => "duplicate_route_dac",
        }
    }
}

impl fmt::Display for PlanBuildErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 方案组装错误：稳定错误码 + 出错阶段 + 人读细节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBuildError {
    code: PlanBuildErrorCode,
    stage: BuildStage,
    detail: String,
}

impl PlanBuildError {
    pub(crate) fn new(
        code: PlanBuildErrorCode,
        stage: BuildStage,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            stage,
            detail: detail.into(),
        }
    }

    /// 稳定错误码（调用方按它 match）。
    pub fn code(&self) -> PlanBuildErrorCode {
        self.code
    }

    /// 出错的组装阶段。
    pub fn stage(&self) -> BuildStage {
        self.stage
    }

    /// 人读细节（含实测值与上限；不保证跨版本稳定）。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for PlanBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "plan build failed: {} (stage {}): {}",
            self.code.as_str(),
            self.stage.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for PlanBuildError {}
