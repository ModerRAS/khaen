//! 方案编译（design → plan ABI v3）的验收测试：可复现、可往返、可被 host 接受。
//!
//! 全部离线、平台无关（Windows 本机可跑）；真实设备/ALSA 不在本测试范围。

use khaen_calibration::plan_compile::{
    compile_peq_design, error, PlanCompileError, PlanCompileOptions, PLAN_ARTIFACT_PATH,
};
use khaen_designer::modal_peq::{BandType, ChannelPeq, DesignMode, ModalBand, PeqDesign};
use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::graph::PlanGraph;
use khaen_host::EngineConfig;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_SIZE: u32 = 128;
const CHANNELS: u16 = 2;

fn band(kind: BandType, freq_hz: f64, q: f64, gain_db: f64) -> ModalBand {
    ModalBand::from_params(kind, freq_hz, q, gain_db, f64::from(SAMPLE_RATE)).expect("band")
}

fn design() -> PeqDesign {
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    let mut left = ChannelPeq::new("L", 0).expect("channel");
    left.push_band(band(BandType::Peaking, 63.0, 4.0, -6.0));
    left.push_band(band(BandType::Peaking, 125.0, 6.0, -3.0));
    let mut right = ChannelPeq::new("R", 1).expect("channel");
    right.push_band(band(BandType::Peaking, 80.0, 5.0, -4.5));
    design.channels.push(left);
    design.channels.push(right);
    design
}

fn options() -> PlanCompileOptions {
    PlanCompileOptions {
        sample_rate: SAMPLE_RATE,
        block_size: BLOCK_SIZE,
        channels: CHANNELS,
        lane_gain_db: vec![0.0, 2.5],
        lane_delay: vec![0, 16],
    }
}

fn host_config() -> EngineConfig {
    EngineConfig {
        block_frames: BLOCK_SIZE as usize,
        sample_rate: SAMPLE_RATE,
        channels: usize::from(CHANNELS),
    }
}

#[test]
fn compiled_plan_is_deterministic_roundtrips_and_is_accepted_by_host() {
    let compiled = compile_peq_design(&design(), &options()).expect("compile");
    assert_eq!(compiled.artifact_path(), PLAN_ARTIFACT_PATH);
    assert!(!compiled.bytes.is_empty());
    assert_eq!(compiled.sha256.len(), 64, "sha256 must be 64 hex chars");
    assert!(compiled
        .sha256
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    assert_eq!(compiled.graph_id.len(), 64);

    // 同一输入再编译一次：结构、字节、摘要、graph_id 全部相同（可复现）。
    let again = compile_peq_design(&design(), &options()).expect("compile again");
    assert_eq!(again.plan, compiled.plan);
    assert_eq!(again.bytes, compiled.bytes);
    assert_eq!(again.sha256, compiled.sha256);
    assert_eq!(again.graph_id, compiled.graph_id);

    // ABI v3 往返：decode(encode(plan)) == plan。
    let decoded = khaen_core::plan::Plan::decode(&compiled.bytes).expect("decode");
    assert_eq!(decoded, compiled.plan);
    assert_eq!(decoded.graph_id_hex(), compiled.graph_id);

    // 保护段内容：每 lane 的 biquad 系数来自设计（f64 → f32），增益/延迟来自选项。
    let protection = compiled.plan.protection.as_ref().expect("protection");
    assert_eq!(protection.lanes, CHANNELS);
    assert_eq!(protection.entries.len(), usize::from(CHANNELS));
    let left = &protection.entries[0];
    assert_eq!(left.lane, 0);
    assert_eq!(left.polarity, 1);
    assert_eq!(left.delay, 0);
    assert_eq!(left.gain_db, 0.0);
    assert_eq!(left.peq.len(), 2);
    let design_left = &design().channels[0];
    assert_eq!(left.peq[0].b0, design_left.bands[0].b[0] as f32);
    assert_eq!(left.peq[0].b1, design_left.bands[0].b[1] as f32);
    assert_eq!(left.peq[0].b2, design_left.bands[0].b[2] as f32);
    assert_eq!(left.peq[0].a1, design_left.bands[0].a[1] as f32);
    assert_eq!(left.peq[0].a2, design_left.bands[0].a[2] as f32);
    assert!((left.gain_db - 0.0).abs() < f32::EPSILON);
    let right = &protection.entries[1];
    assert_eq!(right.lane, 1);
    assert_eq!(right.delay, 16);
    assert_eq!(right.gain_db, 2.5);
    assert_eq!(right.peq.len(), 1);

    // host 侧真实接受：准入通过 + 图编译成功。
    let admission = admit(&compiled.plan, &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());
    PlanGraph::compile(&compiled.plan, &host_config(), &AdmissionLimits::default())
        .expect("plan graph compiles");
}

#[test]
fn lane_without_design_channel_gets_identity_protection() {
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    let mut only_right = ChannelPeq::new("R", 1).expect("channel");
    only_right.push_band(band(BandType::Peaking, 100.0, 5.0, -2.0));
    design.channels.push(only_right);

    let compiled = compile_peq_design(&design, &options()).expect("compile");
    let protection = compiled.plan.protection.as_ref().expect("protection");
    assert!(
        protection.entries[0].peq.is_empty(),
        "lane 0 must have no PEQ"
    );
    assert_eq!(protection.entries[1].peq.len(), 1);
    let admission = admit(&compiled.plan, &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());
}

fn compile_error(design: &PeqDesign, options: &PlanCompileOptions) -> PlanCompileError {
    compile_peq_design(design, options).expect_err("compile must fail")
}

#[test]
fn error_paths_are_stable_codes() {
    // 空设计。
    let empty = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    assert_eq!(
        compile_error(&empty, &options()).code(),
        error::EMPTY_DESIGN
    );

    // lane 越界。
    let mut out_of_range = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    out_of_range
        .channels
        .push(ChannelPeq::new("SURROUND", 2).expect("channel"));
    assert_eq!(
        compile_error(&out_of_range, &options()).code(),
        error::LANE_OUT_OF_RANGE
    );

    // 重复 lane。
    let mut duplicate = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    duplicate
        .channels
        .push(ChannelPeq::new("A", 0).expect("channel"));
    duplicate
        .channels
        .push(ChannelPeq::new("B", 0).expect("channel"));
    assert_eq!(
        compile_error(&duplicate, &options()).code(),
        error::DUPLICATE_LANE
    );

    // 采样率不一致。
    let other_rate = PeqDesign::new(96_000, DesignMode::ModalOnly).expect("design");
    let mut mismatched_design = design();
    mismatched_design.fs = other_rate.fs;
    assert_eq!(
        compile_error(&mismatched_design, &options()).code(),
        error::SAMPLE_RATE_MISMATCH
    );

    // 选项非法：块长 / 通道数 / gain 长度。
    let mut bad_block = options();
    bad_block.block_size = 96;
    assert_eq!(
        compile_error(&design(), &bad_block).code(),
        error::BAD_OPTIONS
    );
    let mut zero_channels = options();
    zero_channels.channels = 0;
    assert_eq!(
        compile_error(&design(), &zero_channels).code(),
        error::BAD_OPTIONS
    );
    let mut bad_gain = options();
    bad_gain.lane_gain_db = vec![1.0];
    assert_eq!(
        compile_error(&design(), &bad_gain).code(),
        error::BAD_OPTIONS
    );

    // 非有限系数（手工构造坏 band：字段是公开的）。
    let mut broken = design();
    broken.channels[0].bands[0].b[1] = f64::NAN;
    assert_eq!(
        compile_error(&broken, &options()).code(),
        error::NON_FINITE_COEFFICIENT
    );

    // 未归一化 biquad（a0 != 1）。
    let mut denormalized = design();
    denormalized.channels[0].bands[0].a[0] = 2.0;
    assert_eq!(
        compile_error(&denormalized, &options()).code(),
        error::NON_NORMALIZED_BIQUAD
    );
}
