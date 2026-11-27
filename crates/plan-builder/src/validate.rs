//! fail-closed 校验：把组装器的输入变成合法 Plan，或返回稳定错误码。
//!
//! 校验顺序固定（config -> content -> crossover -> matrix -> protection -> routing -> lane 链），
//! 因此同一份非法输入总返回同一个错误（确定性错误，不随实现细节漂移）。
//!
//! 只校验 ABI v3 能表达的结构性约束与 host 图编译要求的引用/链序不变式；
//! 内存/延迟/系数预算等准入策略归 khaen_host::admission，不在这里重复。

use khaen_core::plan::{
    ContentSection, CrossoverSection, MatrixSection, Plan, ProtectionSection, RoutingSection,
};
use khaen_core::rt::{BLOCK_SIZES, SAMPLE_RATES_HZ};

use crate::{
    BuildStage, PlanBuildError, PlanBuildErrorCode, PlanBuilder, PlanConfig, MAX_CROSSOVER_FILTERS,
    MAX_CROSSOVER_TAPS, MAX_FOLDS, MAX_MATRIX_COEFFICIENTS, MAX_PEQ_SECTIONS,
    MAX_PROTECTION_ENTRIES, MAX_ROUTES,
};

fn err(code: PlanBuildErrorCode, stage: BuildStage, detail: impl Into<String>) -> PlanBuildError {
    PlanBuildError::new(code, stage, detail)
}

/// 校验方案级配置。
pub(crate) fn validate_config(config: PlanConfig) -> Result<(), PlanBuildError> {
    if !SAMPLE_RATES_HZ.contains(&config.sample_rate) {
        return Err(err(
            PlanBuildErrorCode::UnsupportedSampleRate,
            BuildStage::Config,
            format!(
                "sample_rate {} Hz is not supported (supported: {SAMPLE_RATES_HZ:?})",
                config.sample_rate
            ),
        ));
    }
    let supported_block_size = u16::try_from(config.block_size)
        .map(|block_size| BLOCK_SIZES.contains(&block_size))
        .unwrap_or(false);
    if !supported_block_size {
        return Err(err(
            PlanBuildErrorCode::UnsupportedBlockSize,
            BuildStage::Config,
            format!(
                "block_size {} is not supported (supported: {BLOCK_SIZES:?})",
                config.block_size
            ),
        ));
    }
    if config.input_channels == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroChannelCount,
            BuildStage::Config,
            "input_channels must be positive",
        ));
    }
    if config.output_channels == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroChannelCount,
            BuildStage::Config,
            "output_channels must be positive",
        ));
    }
    Ok(())
}

fn validate_content(content: &ContentSection) -> Result<(), PlanBuildError> {
    if content.in_channels == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroChannelCount,
            BuildStage::Content,
            "content.in_channels = 0",
        ));
    }
    if content.out_lanes == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroLaneCount,
            BuildStage::Content,
            "content.out_lanes = 0",
        ));
    }
    if content.folds.len() > MAX_FOLDS {
        return Err(err(
            PlanBuildErrorCode::TooManyFolds,
            BuildStage::Content,
            format!("{} folds, ABI v3 limit {MAX_FOLDS}", content.folds.len()),
        ));
    }
    for fold in &content.folds {
        if fold.src >= content.in_channels {
            return Err(err(
                PlanBuildErrorCode::FoldSourceOutOfRange,
                BuildStage::Content,
                format!(
                    "fold source {} out of range (in_channels {})",
                    fold.src, content.in_channels
                ),
            ));
        }
        if fold.dst >= content.out_lanes {
            return Err(err(
                PlanBuildErrorCode::FoldDestinationOutOfRange,
                BuildStage::Content,
                format!(
                    "fold destination {} out of range (out_lanes {})",
                    fold.dst, content.out_lanes
                ),
            ));
        }
        if !fold.gain.is_finite() {
            return Err(err(
                PlanBuildErrorCode::NonFiniteGain,
                BuildStage::Content,
                format!(
                    "fold {} -> {} has non-finite gain {}",
                    fold.src, fold.dst, fold.gain
                ),
            ));
        }
    }
    Ok(())
}

fn validate_crossover(crossover: &CrossoverSection) -> Result<(), PlanBuildError> {
    if crossover.lanes == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroLaneCount,
            BuildStage::Crossover,
            "crossover.lanes = 0",
        ));
    }
    if crossover.filters.len() > MAX_CROSSOVER_FILTERS {
        return Err(err(
            PlanBuildErrorCode::TooManyCrossoverFilters,
            BuildStage::Crossover,
            format!(
                "{} crossover filters, ABI v3 limit {MAX_CROSSOVER_FILTERS}",
                crossover.filters.len()
            ),
        ));
    }
    let mut seen = vec![false; usize::from(crossover.lanes)];
    for filter in &crossover.filters {
        let lane = usize::from(filter.lane);
        if lane >= seen.len() {
            return Err(err(
                PlanBuildErrorCode::CrossoverLaneOutOfRange,
                BuildStage::Crossover,
                format!(
                    "crossover filter lane {} out of range (lanes {})",
                    filter.lane, crossover.lanes
                ),
            ));
        }
        if seen[lane] {
            return Err(err(
                PlanBuildErrorCode::DuplicateCrossoverLane,
                BuildStage::Crossover,
                format!("duplicate crossover filter for lane {}", filter.lane),
            ));
        }
        seen[lane] = true;
        if filter.coeffs.is_empty() {
            return Err(err(
                PlanBuildErrorCode::EmptyCrossoverFilter,
                BuildStage::Crossover,
                format!("crossover filter for lane {} has no taps", filter.lane),
            ));
        }
        if filter.coeffs.len() > MAX_CROSSOVER_TAPS {
            return Err(err(
                PlanBuildErrorCode::CrossoverTapsTooLong,
                BuildStage::Crossover,
                format!(
                    "crossover filter for lane {} has {} taps, ABI v3 decoder limit {MAX_CROSSOVER_TAPS}",
                    filter.lane,
                    filter.coeffs.len()
                ),
            ));
        }
        for (tap, coeff) in filter.coeffs.iter().enumerate() {
            if !coeff.is_finite() {
                return Err(err(
                    PlanBuildErrorCode::NonFiniteCoefficient,
                    BuildStage::Crossover,
                    format!(
                        "crossover filter for lane {} tap {tap} is not finite ({coeff})",
                        filter.lane
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_matrix(matrix: &MatrixSection) -> Result<(), PlanBuildError> {
    if matrix.rows == 0 || matrix.cols == 0 {
        return Err(err(
            PlanBuildErrorCode::MatrixDimensionsZero,
            BuildStage::Matrix,
            format!(
                "matrix dimensions {}x{} must both be positive",
                matrix.rows, matrix.cols
            ),
        ));
    }
    if matrix.taps == 0 {
        return Err(err(
            PlanBuildErrorCode::MatrixTapsZero,
            BuildStage::Matrix,
            "matrix.taps = 0",
        ));
    }
    if matrix.decimation == 0 || matrix.interpolation == 0 {
        return Err(err(
            PlanBuildErrorCode::MatrixRateZero,
            BuildStage::Matrix,
            format!(
                "matrix decimation {} / interpolation {} must both be non-zero",
                matrix.decimation, matrix.interpolation
            ),
        ));
    }
    let expected = u64::from(matrix.rows) * u64::from(matrix.cols) * u64::from(matrix.taps);
    if expected > MAX_MATRIX_COEFFICIENTS as u64 {
        return Err(err(
            PlanBuildErrorCode::MatrixCoefficientsTooMany,
            BuildStage::Matrix,
            format!(
                "matrix declares {expected} coefficients (rows*cols*taps), ABI v3 decoder limit {MAX_MATRIX_COEFFICIENTS}"
            ),
        ));
    }
    if matrix.coeffs.len() as u64 != expected {
        return Err(err(
            PlanBuildErrorCode::MatrixCoefficientCountMismatch,
            BuildStage::Matrix,
            format!(
                "{} matrix coefficients, expected rows*cols*taps = {expected}",
                matrix.coeffs.len()
            ),
        ));
    }
    for (index, coeff) in matrix.coeffs.iter().enumerate() {
        if !coeff.is_finite() {
            return Err(err(
                PlanBuildErrorCode::NonFiniteCoefficient,
                BuildStage::Matrix,
                format!("matrix coefficient {index} is not finite ({coeff})"),
            ));
        }
    }
    Ok(())
}

fn validate_protection(protection: &ProtectionSection) -> Result<(), PlanBuildError> {
    if protection.lanes == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroLaneCount,
            BuildStage::Protection,
            "protection.lanes = 0",
        ));
    }
    if protection.entries.len() > MAX_PROTECTION_ENTRIES {
        return Err(err(
            PlanBuildErrorCode::TooManyProtectionEntries,
            BuildStage::Protection,
            format!(
                "{} protection entries, ABI v3 limit {MAX_PROTECTION_ENTRIES}",
                protection.entries.len()
            ),
        ));
    }
    let mut seen = vec![false; usize::from(protection.lanes)];
    for entry in &protection.entries {
        let lane = usize::from(entry.lane);
        if lane >= seen.len() {
            return Err(err(
                PlanBuildErrorCode::ProtectionLaneOutOfRange,
                BuildStage::Protection,
                format!(
                    "protection entry lane {} out of range (lanes {})",
                    entry.lane, protection.lanes
                ),
            ));
        }
        if seen[lane] {
            return Err(err(
                PlanBuildErrorCode::DuplicateProtectionLane,
                BuildStage::Protection,
                format!("duplicate protection entry for lane {}", entry.lane),
            ));
        }
        seen[lane] = true;
        if entry.polarity != 1 && entry.polarity != -1 {
            return Err(err(
                PlanBuildErrorCode::InvalidPolarity,
                BuildStage::Protection,
                format!(
                    "protection lane {} polarity {} must be +1 or -1",
                    entry.lane, entry.polarity
                ),
            ));
        }
        if !entry.gain_db.is_finite() {
            return Err(err(
                PlanBuildErrorCode::NonFiniteGainDb,
                BuildStage::Protection,
                format!(
                    "protection lane {} gain_db is not finite ({})",
                    entry.lane, entry.gain_db
                ),
            ));
        }
        if entry.peq.len() > MAX_PEQ_SECTIONS {
            return Err(err(
                PlanBuildErrorCode::TooManyPeqSections,
                BuildStage::Protection,
                format!(
                    "protection lane {} has {} PEQ sections, ABI v3 limit {MAX_PEQ_SECTIONS}",
                    entry.lane,
                    entry.peq.len()
                ),
            ));
        }
        for (section, peq) in entry.peq.iter().enumerate() {
            for (name, coeff) in [
                ("b0", peq.b0),
                ("b1", peq.b1),
                ("b2", peq.b2),
                ("a1", peq.a1),
                ("a2", peq.a2),
            ] {
                if !coeff.is_finite() {
                    return Err(err(
                        PlanBuildErrorCode::NonFiniteCoefficient,
                        BuildStage::Protection,
                        format!(
                            "protection lane {} PEQ section {section} coeff {name} is not finite ({coeff})",
                            entry.lane
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_routing(routing: &RoutingSection, lanes: u16) -> Result<(), PlanBuildError> {
    if routing.dac_channels == 0 {
        return Err(err(
            PlanBuildErrorCode::ZeroChannelCount,
            BuildStage::Routing,
            "routing.dac_channels = 0",
        ));
    }
    if routing.routes.len() > MAX_ROUTES {
        return Err(err(
            PlanBuildErrorCode::TooManyRoutes,
            BuildStage::Routing,
            format!("{} routes, ABI v3 limit {MAX_ROUTES}", routing.routes.len()),
        ));
    }
    let mut assigned = vec![false; usize::from(routing.dac_channels)];
    for &(lane, dac) in &routing.routes {
        if lane >= lanes {
            return Err(err(
                PlanBuildErrorCode::RouteLaneOutOfRange,
                BuildStage::Routing,
                format!("route lane {lane} out of range (lanes {lanes})"),
            ));
        }
        let dac_index = usize::from(dac);
        if dac_index >= assigned.len() {
            return Err(err(
                PlanBuildErrorCode::RouteDacOutOfRange,
                BuildStage::Routing,
                format!(
                    "route dac {dac} out of range (dac_channels {})",
                    routing.dac_channels
                ),
            ));
        }
        if assigned[dac_index] {
            return Err(err(
                PlanBuildErrorCode::DuplicateRouteDac,
                BuildStage::Routing,
                format!("duplicate route target dac {dac}"),
            ));
        }
        assigned[dac_index] = true;
    }
    Ok(())
}

/// lane 链校验：与 khaen_host::graph::PlanGraph::compile 的一致性检查同构，
/// 保证 build 产出的方案不会在 host 编译期因段间不衔接被拒。
fn validate_chain(builder: &PlanBuilder, matrix: &MatrixSection) -> Result<(), PlanBuildError> {
    let input_channels = usize::from(builder.config().input_channels);
    let output_channels = usize::from(builder.config().output_channels);

    let lanes_after_content: u16 = match builder.content() {
        Some(content) => {
            let declared = usize::from(content.in_channels);
            if declared != input_channels {
                return Err(err(
                    PlanBuildErrorCode::ChannelCountMismatch,
                    BuildStage::Content,
                    format!(
                        "runtime input_channels = {input_channels}, content.in_channels = {declared}"
                    ),
                ));
            }
            content.out_lanes
        }
        None => {
            // content 缺席时 host 用 crossover.lanes（再缺则 matrix.cols）当 lane 数；
            // 输入宽度与 lane 数已经是两个独立的量，不再要求它等于输入通道数。
            match builder.crossover() {
                Some(crossover) => crossover.lanes,
                None => matrix.cols,
            }
        }
    };

    let lanes_after_crossover = match builder.crossover() {
        Some(crossover) => {
            if crossover.lanes != lanes_after_content {
                return Err(err(
                    PlanBuildErrorCode::LaneCountMismatch,
                    BuildStage::Crossover,
                    format!(
                        "crossover.lanes = {}, upstream lanes from content = {lanes_after_content}",
                        crossover.lanes
                    ),
                ));
            }
            lanes_after_content
        }
        None => lanes_after_content,
    };

    if matrix.cols != lanes_after_crossover {
        return Err(err(
            PlanBuildErrorCode::LaneCountMismatch,
            BuildStage::Matrix,
            format!(
                "matrix.cols = {}, upstream lanes = {lanes_after_crossover}",
                matrix.cols
            ),
        ));
    }

    if let Some(protection) = builder.protection() {
        if protection.lanes != matrix.rows {
            return Err(err(
                PlanBuildErrorCode::LaneCountMismatch,
                BuildStage::Protection,
                format!(
                    "protection.lanes = {}, matrix rows = {}",
                    protection.lanes, matrix.rows
                ),
            ));
        }
    }

    let out_channels = match builder.routing() {
        Some(routing) => usize::from(routing.dac_channels),
        None => usize::from(matrix.rows),
    };
    if out_channels != output_channels {
        return Err(err(
            PlanBuildErrorCode::ChannelCountMismatch,
            BuildStage::Routing,
            format!(
                "runtime output_channels = {output_channels}, plan output channels = {out_channels} (routing.dac_channels when present, otherwise matrix.rows)"
            ),
        ));
    }
    Ok(())
}

/// 校验并组装成 Plan。
pub(crate) fn assemble(builder: &PlanBuilder) -> Result<Plan, PlanBuildError> {
    validate_config(builder.config())?;
    if let Some(content) = builder.content() {
        validate_content(content)?;
    }
    if let Some(crossover) = builder.crossover() {
        validate_crossover(crossover)?;
    }
    let matrix = builder.matrix().ok_or_else(|| {
        err(
            PlanBuildErrorCode::MissingMatrix,
            BuildStage::Matrix,
            "plan has no matrix section; the host runtime graph requires one",
        )
    })?;
    validate_matrix(matrix)?;
    if let Some(protection) = builder.protection() {
        validate_protection(protection)?;
    }
    if let Some(routing) = builder.routing() {
        validate_routing(routing, matrix.rows)?;
    }
    validate_chain(builder, matrix)?;

    Ok(Plan {
        block_size: builder.config().block_size,
        sample_rate: builder.config().sample_rate,
        content: builder.content().cloned(),
        crossover: builder.crossover().cloned(),
        matrix: builder.matrix().cloned(),
        protection: builder.protection().cloned(),
        routing: builder.routing().cloned(),
    })
}
