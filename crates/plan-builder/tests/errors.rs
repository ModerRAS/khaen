//! 错误路径：每个稳定错误码至少一条构造，锁住 fail-closed 行为。

use std::collections::HashSet;

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection,
    ProtectionEntry, ProtectionSection, RoutingSection,
};
use khaen_plan_builder::{
    BuildStage, PlanBuildErrorCode, PlanBuilder, PlanConfig, MAX_CROSSOVER_FILTERS,
    MAX_CROSSOVER_TAPS, MAX_FOLDS, MAX_PEQ_SECTIONS, MAX_PROTECTION_ENTRIES, MAX_ROUTES,
};

fn config() -> PlanConfig {
    PlanConfig::new(128, 96_000, 2)
}

fn identity() -> PlanBuilder {
    PlanBuilder::identity(config())
}

fn identity_entry(lane: u16) -> ProtectionEntry {
    ProtectionEntry {
        lane,
        delay: 0,
        polarity: 1,
        gain_db: 0.0,
        peq: Vec::new(),
    }
}

fn assert_error(builder: &PlanBuilder, code: PlanBuildErrorCode, stage: BuildStage) {
    let error = builder.build().expect_err("expected build error");
    assert_eq!(error.code(), code, "unexpected error: {error}");
    assert_eq!(error.stage(), stage, "unexpected stage for {error}");
    // build_bytes 不产出半成品字节，返回同一个错误。
    let bytes_error = builder
        .build_bytes()
        .expect_err("expected build_bytes error");
    assert_eq!(bytes_error, error);
    // 稳定编码可被调用方匹配/打印。
    assert!(error.to_string().contains(error.code().as_str()));
}

fn with_content(builder: PlanBuilder, edit: impl FnOnce(&mut ContentSection)) -> PlanBuilder {
    let mut content = builder.content().cloned().expect("identity content");
    edit(&mut content);
    builder.set_content(content)
}

fn with_crossover(builder: PlanBuilder, edit: impl FnOnce(&mut CrossoverSection)) -> PlanBuilder {
    let mut crossover = builder.crossover().cloned().expect("identity crossover");
    edit(&mut crossover);
    builder.set_crossover(crossover)
}

fn with_matrix(builder: PlanBuilder, edit: impl FnOnce(&mut MatrixSection)) -> PlanBuilder {
    let mut matrix = builder.matrix().cloned().expect("identity matrix");
    edit(&mut matrix);
    builder.set_matrix(matrix)
}

fn with_protection(builder: PlanBuilder, edit: impl FnOnce(&mut ProtectionSection)) -> PlanBuilder {
    let mut protection = builder.protection().cloned().expect("identity protection");
    edit(&mut protection);
    builder.set_protection(protection)
}

fn with_routing(builder: PlanBuilder, edit: impl FnOnce(&mut RoutingSection)) -> PlanBuilder {
    let mut routing = builder.routing().cloned().expect("identity routing");
    edit(&mut routing);
    builder.set_routing(routing)
}

// ---------- config ----------

#[test]
fn unsupported_sample_rate_is_rejected() {
    let builder = PlanBuilder::identity(PlanConfig::new(128, 44_100, 2));
    assert_error(
        &builder,
        PlanBuildErrorCode::UnsupportedSampleRate,
        BuildStage::Config,
    );
    let error = PlanConfig::new(128, 44_100, 2)
        .validate()
        .expect_err("config validate error");
    assert_eq!(error.code(), PlanBuildErrorCode::UnsupportedSampleRate);
}

#[test]
fn unsupported_block_size_is_rejected() {
    for block_size in [0u32, 100, 256] {
        let builder = PlanBuilder::identity(PlanConfig::new(block_size, 96_000, 2));
        assert_error(
            &builder,
            PlanBuildErrorCode::UnsupportedBlockSize,
            BuildStage::Config,
        );
    }
}

#[test]
fn zero_channels_is_rejected() {
    let builder = PlanBuilder::identity(PlanConfig::new(128, 96_000, 0));
    assert_error(
        &builder,
        PlanBuildErrorCode::ZeroChannelCount,
        BuildStage::Config,
    );
}

// ---------- content ----------

#[test]
fn zero_content_channels_is_rejected() {
    let builder = with_content(identity(), |content| content.in_channels = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::ZeroChannelCount,
        BuildStage::Content,
    );
}

#[test]
fn zero_content_lanes_is_rejected() {
    let builder = with_content(identity(), |content| content.out_lanes = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::ZeroLaneCount,
        BuildStage::Content,
    );
}

#[test]
fn abi_limits_are_enforced_for_folds() {
    let builder = with_content(identity(), |content| {
        content.folds = vec![
            Fold {
                src: 0,
                dst: 0,
                gain: 1.0,
            };
            MAX_FOLDS + 1
        ];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::TooManyFolds,
        BuildStage::Content,
    );
}

#[test]
fn fold_source_out_of_range_is_rejected() {
    let builder = with_content(identity(), |content| {
        content.folds.push(Fold {
            src: 2,
            dst: 0,
            gain: 1.0,
        });
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::FoldSourceOutOfRange,
        BuildStage::Content,
    );
}

#[test]
fn fold_destination_out_of_range_is_rejected() {
    let builder = with_content(identity(), |content| {
        content.folds.push(Fold {
            src: 0,
            dst: 2,
            gain: 1.0,
        });
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::FoldDestinationOutOfRange,
        BuildStage::Content,
    );
}

#[test]
fn non_finite_fold_gain_is_rejected() {
    let builder = with_content(identity(), |content| content.folds[0].gain = f32::NAN);
    assert_error(
        &builder,
        PlanBuildErrorCode::NonFiniteGain,
        BuildStage::Content,
    );
}

// ---------- crossover ----------

#[test]
fn zero_crossover_lanes_is_rejected() {
    let builder = with_crossover(identity(), |crossover| crossover.lanes = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::ZeroLaneCount,
        BuildStage::Crossover,
    );
}

#[test]
fn abi_limits_are_enforced_for_crossover_filters() {
    let builder = with_crossover(identity(), |crossover| {
        crossover.filters = vec![
            CrossoverFilter {
                lane: 0,
                coeffs: vec![1.0],
            };
            MAX_CROSSOVER_FILTERS + 1
        ];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::TooManyCrossoverFilters,
        BuildStage::Crossover,
    );
}

#[test]
fn crossover_lane_out_of_range_is_rejected() {
    let builder = with_crossover(identity(), |crossover| {
        crossover.filters.push(CrossoverFilter {
            lane: 2,
            coeffs: vec![1.0],
        });
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::CrossoverLaneOutOfRange,
        BuildStage::Crossover,
    );
}

#[test]
fn duplicate_crossover_lane_is_rejected() {
    let builder = with_crossover(identity(), |crossover| {
        crossover.filters.push(CrossoverFilter {
            lane: 0,
            coeffs: vec![1.0],
        });
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::DuplicateCrossoverLane,
        BuildStage::Crossover,
    );
}

#[test]
fn empty_crossover_filter_is_rejected() {
    let builder = with_crossover(identity(), |crossover| crossover.filters[0].coeffs.clear());
    assert_error(
        &builder,
        PlanBuildErrorCode::EmptyCrossoverFilter,
        BuildStage::Crossover,
    );
}

#[test]
fn crossover_taps_over_abi_limit_are_rejected() {
    let builder = with_crossover(identity(), |crossover| {
        crossover.filters[0].coeffs = vec![0.0f32; MAX_CROSSOVER_TAPS + 1];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::CrossoverTapsTooLong,
        BuildStage::Crossover,
    );
}

#[test]
fn non_finite_crossover_coefficient_is_rejected() {
    let builder = with_crossover(identity(), |crossover| {
        crossover.filters[0].coeffs = vec![f32::INFINITY];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::NonFiniteCoefficient,
        BuildStage::Crossover,
    );
}

// ---------- matrix ----------

#[test]
fn missing_matrix_is_rejected() {
    let builder = PlanBuilder::new(config());
    assert_error(
        &builder,
        PlanBuildErrorCode::MissingMatrix,
        BuildStage::Matrix,
    );
}

#[test]
fn matrix_dimension_error_is_rejected() {
    let builder = with_matrix(identity(), |matrix| matrix.rows = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixDimensionsZero,
        BuildStage::Matrix,
    );
    let builder = with_matrix(identity(), |matrix| matrix.cols = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixDimensionsZero,
        BuildStage::Matrix,
    );
}

#[test]
fn zero_matrix_taps_is_rejected() {
    let builder = with_matrix(identity(), |matrix| matrix.taps = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixTapsZero,
        BuildStage::Matrix,
    );
}

#[test]
fn matrix_coefficient_count_mismatch_is_rejected() {
    let builder = with_matrix(identity(), |matrix| {
        matrix.coeffs.pop();
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixCoefficientCountMismatch,
        BuildStage::Matrix,
    );
}

#[test]
fn matrix_coefficients_over_abi_limit_are_rejected() {
    let builder = with_matrix(identity(), |matrix| {
        // rows*cols*taps = 65535 * 65 > 1 << 22；还没到系数个数比较就先报超限。
        matrix.rows = u16::MAX;
        matrix.cols = 65;
        matrix.taps = 1;
        matrix.coeffs.clear();
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixCoefficientsTooMany,
        BuildStage::Matrix,
    );
}

#[test]
fn zero_matrix_rate_is_rejected() {
    let builder = with_matrix(identity(), |matrix| matrix.decimation = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixRateZero,
        BuildStage::Matrix,
    );
    let builder = with_matrix(identity(), |matrix| matrix.interpolation = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::MatrixRateZero,
        BuildStage::Matrix,
    );
}

#[test]
fn non_finite_matrix_coefficient_is_rejected() {
    let builder = with_matrix(identity(), |matrix| matrix.coeffs[1] = f32::NAN);
    assert_error(
        &builder,
        PlanBuildErrorCode::NonFiniteCoefficient,
        BuildStage::Matrix,
    );
}

// ---------- protection ----------

#[test]
fn zero_protection_lanes_is_rejected() {
    let builder = with_protection(identity(), |protection| protection.lanes = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::ZeroLaneCount,
        BuildStage::Protection,
    );
}

#[test]
fn abi_limits_are_enforced_for_protection_entries() {
    let builder = with_protection(identity(), |protection| {
        protection.entries = vec![identity_entry(0); MAX_PROTECTION_ENTRIES + 1];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::TooManyProtectionEntries,
        BuildStage::Protection,
    );
}

#[test]
fn protection_lane_out_of_range_is_rejected() {
    let builder = with_protection(identity(), |protection| {
        protection.entries.push(identity_entry(2));
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::ProtectionLaneOutOfRange,
        BuildStage::Protection,
    );
}

#[test]
fn duplicate_protection_lane_is_rejected() {
    let builder = with_protection(identity(), |protection| {
        protection.entries.push(identity_entry(0));
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::DuplicateProtectionLane,
        BuildStage::Protection,
    );
}

#[test]
fn invalid_polarity_is_rejected() {
    let builder = with_protection(identity(), |protection| {
        protection.entries[0].polarity = 0;
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::InvalidPolarity,
        BuildStage::Protection,
    );
}

#[test]
fn non_finite_protection_gain_is_rejected() {
    let builder = with_protection(identity(), |protection| {
        protection.entries[0].gain_db = f32::NAN;
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::NonFiniteGainDb,
        BuildStage::Protection,
    );
}

#[test]
fn abi_limits_are_enforced_for_peq_sections() {
    let identity_peq = PeqSection {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
    let builder = with_protection(identity(), |protection| {
        protection.entries[0].peq = vec![identity_peq; MAX_PEQ_SECTIONS + 1];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::TooManyPeqSections,
        BuildStage::Protection,
    );
}

#[test]
fn non_finite_peq_coefficient_is_rejected() {
    let builder = with_protection(identity(), |protection| {
        protection.entries[0].peq.push(PeqSection {
            b0: 1.0,
            b1: 0.0,
            b2: f32::NAN,
            a1: 0.0,
            a2: 0.0,
        });
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::NonFiniteCoefficient,
        BuildStage::Protection,
    );
}

// ---------- routing ----------

#[test]
fn zero_dac_channels_is_rejected() {
    let builder = with_routing(identity(), |routing| routing.dac_channels = 0);
    assert_error(
        &builder,
        PlanBuildErrorCode::ZeroChannelCount,
        BuildStage::Routing,
    );
}

#[test]
fn abi_limits_are_enforced_for_routes() {
    let builder = with_routing(identity(), |routing| {
        routing.routes = vec![(0, 0); MAX_ROUTES + 1];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::TooManyRoutes,
        BuildStage::Routing,
    );
}

#[test]
fn routing_referencing_missing_lane_is_rejected() {
    let builder = with_routing(identity(), |routing| routing.routes.push((2, 0)));
    assert_error(
        &builder,
        PlanBuildErrorCode::RouteLaneOutOfRange,
        BuildStage::Routing,
    );
}

#[test]
fn routing_referencing_missing_dac_is_rejected() {
    let builder = with_routing(identity(), |routing| routing.routes.push((0, 2)));
    assert_error(
        &builder,
        PlanBuildErrorCode::RouteDacOutOfRange,
        BuildStage::Routing,
    );
}

#[test]
fn duplicate_route_target_is_rejected() {
    let builder = with_routing(identity(), |routing| routing.routes.push((1, 0)));
    assert_error(
        &builder,
        PlanBuildErrorCode::DuplicateRouteDac,
        BuildStage::Routing,
    );
}

// ---------- 段间链序 ----------

#[test]
fn channel_count_mismatch_is_rejected() {
    let builder = with_content(identity(), |content| content.in_channels = 3);
    assert_error(
        &builder,
        PlanBuildErrorCode::ChannelCountMismatch,
        BuildStage::Content,
    );
    let builder = with_routing(identity(), |routing| routing.dac_channels = 3);
    assert_error(
        &builder,
        PlanBuildErrorCode::ChannelCountMismatch,
        BuildStage::Routing,
    );
}

#[test]
fn lane_count_mismatch_is_rejected() {
    let builder = with_crossover(identity(), |crossover| crossover.lanes = 3);
    assert_error(
        &builder,
        PlanBuildErrorCode::LaneCountMismatch,
        BuildStage::Crossover,
    );
    let builder = with_matrix(identity(), |matrix| {
        matrix.cols = 3;
        matrix.coeffs = vec![0.0f32; 6];
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::LaneCountMismatch,
        BuildStage::Matrix,
    );
    let builder = with_protection(identity(), |protection| protection.lanes = 3);
    assert_error(
        &builder,
        PlanBuildErrorCode::LaneCountMismatch,
        BuildStage::Protection,
    );
}

#[test]
fn content_absent_derived_lanes_must_match_channels() {
    let builder = PlanBuilder::new(config()).set_matrix(MatrixSection {
        rows: 3,
        cols: 3,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    });
    assert_error(
        &builder,
        PlanBuildErrorCode::ChannelCountMismatch,
        BuildStage::Content,
    );
}

#[test]
fn routing_absent_output_channels_must_match_channels() {
    let builder = PlanBuilder::new(config())
        .set_content_identity(2)
        .set_matrix_cells(3, 2, 1, 1, 1, vec![0.0f32; 6]);
    assert_error(
        &builder,
        PlanBuildErrorCode::ChannelCountMismatch,
        BuildStage::Routing,
    );
}

// ---------- 错误码表与 Display ----------

#[test]
fn every_error_code_has_a_unique_stable_string() {
    let codes = [
        PlanBuildErrorCode::UnsupportedSampleRate,
        PlanBuildErrorCode::UnsupportedBlockSize,
        PlanBuildErrorCode::ZeroChannelCount,
        PlanBuildErrorCode::ZeroLaneCount,
        PlanBuildErrorCode::MissingMatrix,
        PlanBuildErrorCode::ChannelCountMismatch,
        PlanBuildErrorCode::LaneCountMismatch,
        PlanBuildErrorCode::TooManyFolds,
        PlanBuildErrorCode::FoldSourceOutOfRange,
        PlanBuildErrorCode::FoldDestinationOutOfRange,
        PlanBuildErrorCode::NonFiniteGain,
        PlanBuildErrorCode::TooManyCrossoverFilters,
        PlanBuildErrorCode::CrossoverLaneOutOfRange,
        PlanBuildErrorCode::DuplicateCrossoverLane,
        PlanBuildErrorCode::EmptyCrossoverFilter,
        PlanBuildErrorCode::CrossoverTapsTooLong,
        PlanBuildErrorCode::MatrixDimensionsZero,
        PlanBuildErrorCode::MatrixTapsZero,
        PlanBuildErrorCode::MatrixCoefficientCountMismatch,
        PlanBuildErrorCode::MatrixCoefficientsTooMany,
        PlanBuildErrorCode::MatrixRateZero,
        PlanBuildErrorCode::TooManyProtectionEntries,
        PlanBuildErrorCode::ProtectionLaneOutOfRange,
        PlanBuildErrorCode::DuplicateProtectionLane,
        PlanBuildErrorCode::InvalidPolarity,
        PlanBuildErrorCode::NonFiniteGainDb,
        PlanBuildErrorCode::TooManyPeqSections,
        PlanBuildErrorCode::NonFiniteCoefficient,
        PlanBuildErrorCode::TooManyRoutes,
        PlanBuildErrorCode::RouteLaneOutOfRange,
        PlanBuildErrorCode::RouteDacOutOfRange,
        PlanBuildErrorCode::DuplicateRouteDac,
    ];
    let mut seen = HashSet::new();
    for code in codes {
        let text = code.as_str();
        assert!(!text.is_empty(), "empty code string");
        assert_eq!(text, text.to_ascii_lowercase(), "code must be lowercase");
        assert!(seen.insert(text), "duplicate code string: {text}");
        assert_eq!(code.to_string(), text);
    }
    assert_eq!(seen.len(), codes.len());

    let stages = [
        BuildStage::Config,
        BuildStage::Content,
        BuildStage::Crossover,
        BuildStage::Matrix,
        BuildStage::Protection,
        BuildStage::Routing,
    ];
    let mut stage_names = HashSet::new();
    for stage in stages {
        assert!(stage_names.insert(stage.as_str()), "duplicate stage");
        assert_eq!(stage.to_string(), stage.as_str());
    }
    assert_eq!(stage_names.len(), stages.len());
}

#[test]
fn error_display_names_code_and_stage() {
    let error = PlanBuilder::new(config())
        .build()
        .expect_err("missing matrix");
    assert_eq!(error.code(), PlanBuildErrorCode::MissingMatrix);
    assert_eq!(error.stage(), BuildStage::Matrix);
    assert!(!error.detail().is_empty());
    let message = error.to_string();
    assert!(message.contains("missing_matrix"), "message: {message}");
    assert!(message.contains("matrix"), "message: {message}");
    // 可以作为 std::error::Error 传播。
    let as_error: &dyn std::error::Error = &error;
    assert!(as_error.source().is_none());
}
