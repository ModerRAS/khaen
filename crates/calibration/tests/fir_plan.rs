//! FIR -> 方案编译：可复现、可往返、可被 host 准入与图编译接受、可推预测电平。

use khaen_calibration::fir_plan::{
    compile_fir_plan, FirPlanConfig, LaneFir, BAD_INPUT, BUILD, CONFIG, ENCODE,
};
use khaen_calibration::plan_levels::predicted_levels;
use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::graph::PlanGraph;
use khaen_host::{EngineConfig, Processor};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_SIZE: u32 = 128;

fn config(channels: u16) -> FirPlanConfig {
    FirPlanConfig::new(BLOCK_SIZE, SAMPLE_RATE, channels)
}

fn host_config(channels: u16) -> EngineConfig {
    EngineConfig {
        block_frames: BLOCK_SIZE as usize,
        sample_rate: SAMPLE_RATE,
        input_channels: usize::from(channels),
        output_channels: usize::from(channels),
    }
}

fn constant_gain(taps: usize, gain: f64) -> Vec<f64> {
    let mut taps_vec = vec![0.0; taps];
    taps_vec[0] = gain;
    taps_vec
}

#[test]
fn compiles_reproducible_plan_and_host_accepts_it() {
    let lanes = vec![
        LaneFir {
            lane: 0,
            taps: constant_gain(16, 2.0),
        },
        LaneFir {
            lane: 1,
            taps: constant_gain(16, 0.5),
        },
    ];
    let compiled = compile_fir_plan(&lanes, &config(2)).expect("compile");
    assert_eq!(compiled.sha256().len(), 64);
    assert_eq!(compiled.graph_id().len(), 64);
    assert!(compiled.bytes().starts_with(b"KHAENPLN"));

    // 解码往返：crossover 承载 FIR，其余段恒等。
    let plan = compiled.plan();
    let crossover = plan.crossover.as_ref().expect("crossover section");
    assert_eq!(crossover.lanes, 2);
    assert_eq!(crossover.filters.len(), 2);
    assert_eq!(crossover.filters[0].coeffs.len(), 16);
    assert_eq!(crossover.filters[0].coeffs[0], 2.0);
    assert_eq!(crossover.filters[1].coeffs[0], 0.5);
    assert!(plan.matrix.is_some());
    assert!(plan.protection.is_some());

    // 同输入再编译：字节 / sha / graph_id 全相同。
    let again = compile_fir_plan(&lanes, &config(2)).expect("compile again");
    assert_eq!(compiled.bytes(), again.bytes());
    assert_eq!(compiled.sha256(), again.sha256());
    assert_eq!(compiled.graph_id(), again.graph_id());

    // host 准入与图编译接受（与 daemon/rt-host 同一条校验路径）。
    let admission = admit(&compiled.plan().clone(), &AdmissionLimits::default());
    assert!(admission.accepted, "{admission:?}");
    PlanGraph::compile(
        compiled.plan(),
        &host_config(2),
        &AdmissionLimits::default(),
    )
    .expect("plan graph compiles");
}

#[test]
fn predicted_levels_reflect_the_fir() {
    let lanes = vec![LaneFir {
        lane: 0,
        taps: constant_gain(8, 2.0),
    }];
    let compiled = compile_fir_plan(&lanes, &config(1)).expect("compile");
    let levels = predicted_levels(compiled.plan(), &[100.0, 1_000.0, 10_000.0]).expect("levels");
    for band in &levels.points[0].bands {
        assert!((band.level_db.unwrap() - 6.0206).abs() <= 1e-4, "{band:?}");
    }
}

#[test]
fn partial_lane_coverage_keeps_other_lanes_identity() {
    let lanes = vec![LaneFir {
        lane: 1,
        taps: constant_gain(4, 2.0),
    }];
    let compiled = compile_fir_plan(&lanes, &config(2)).expect("compile");
    let levels = predicted_levels(compiled.plan(), &[1_000.0]).expect("levels");
    assert!(levels.points[0].bands[0].level_db.unwrap().abs() <= 1e-12);
    assert!((levels.points[1].bands[0].level_db.unwrap() - 6.0206).abs() <= 1e-4);
}

#[test]
fn realistic_2048_tap_fir_is_admitted_and_executable() {
    // 与 mimo 默认抽头数同量级：2 lanes x 2048 taps。lane 0 是纯延迟 1024 的冲激，
    // lane 1 全零（静音）；用来验证准入预算、图编译延迟估算与图内 FIR 的实际输出。
    let taps_len = 2048usize;
    let mut delayed = vec![0.0f64; taps_len];
    delayed[taps_len / 2] = 1.0;
    let silent = vec![0.0f64; taps_len];
    let lanes = vec![
        LaneFir {
            lane: 0,
            taps: delayed,
        },
        LaneFir {
            lane: 1,
            taps: silent,
        },
    ];
    let compiled = compile_fir_plan(&lanes, &config(2)).expect("compile");
    let admission = admit(compiled.plan(), &AdmissionLimits::default());
    assert!(admission.accepted, "{admission:?}");
    // crossover 2048 x 2 = 4096 抽头，加上恒等矩阵 2x2x1 = 4 个系数。
    assert_eq!(admission.budget.convolution_taps, 4100);
    assert_eq!(admission.budget.coefficients, 4100);

    // 准入预算里的延迟含 crossover（(2048-1)/2 = 1023）；图执行器的
    // estimated_latency_samples() 只估算 matrix 段，恒等矩阵下为 0，不要混用。
    assert_eq!(admission.budget.latency_samples, 1023);
    let mut graph = PlanGraph::compile(
        compiled.plan(),
        &host_config(2),
        &AdmissionLimits::default(),
    )
    .expect("graph compiles");
    assert_eq!(graph.estimated_latency_samples(), 0);

    let block_frames = 128usize;
    let channels = 2usize;
    let mut output_global: Vec<f32> = Vec::new();
    for block_index in 0..12 {
        let mut input = vec![0.0f32; block_frames * channels];
        if block_index == 0 {
            input[0] = 1.0;
        }
        let mut output = vec![0.0f32; block_frames * channels];
        graph.process(&input, &mut output);
        output_global.extend_from_slice(&output);
    }
    // 冲激在第 0 帧，纯延迟 1024 → 第 1024 帧输出 1.0；lane 1 静音。
    assert!((output_global[1024 * channels] - 1.0).abs() <= 1e-6);
    for frame in 0..1536usize {
        let channel1 = output_global[frame * channels + 1];
        assert!(
            channel1.abs() <= 1e-12,
            "lane 1 should stay silent at {frame}"
        );
    }
}
#[test]
fn invalid_lane_firs_are_input_errors() {
    // 空 FIR 列表。
    assert_eq!(
        compile_fir_plan(&[], &config(1)).expect_err("empty").code(),
        BAD_INPUT
    );
    // lane 越界。
    assert_eq!(
        compile_fir_plan(
            &[LaneFir {
                lane: 3,
                taps: vec![1.0],
            }],
            &config(1)
        )
        .expect_err("lane out of range")
        .code(),
        BAD_INPUT
    );
    // 重复 lane。
    assert_eq!(
        compile_fir_plan(
            &[
                LaneFir {
                    lane: 0,
                    taps: vec![1.0],
                },
                LaneFir {
                    lane: 0,
                    taps: vec![1.0],
                }
            ],
            &config(2)
        )
        .expect_err("duplicate")
        .code(),
        BAD_INPUT
    );
    // 空 taps。
    assert_eq!(
        compile_fir_plan(
            &[LaneFir {
                lane: 0,
                taps: vec![],
            }],
            &config(1)
        )
        .expect_err("no taps")
        .code(),
        BAD_INPUT
    );
    // 非有限 tap / 超出 f32。
    for bad in [f64::NAN, f64::INFINITY, 1e300] {
        assert_eq!(
            compile_fir_plan(
                &[LaneFir {
                    lane: 0,
                    taps: vec![bad],
                }],
                &config(1)
            )
            .expect_err("bad tap")
            .code(),
            BAD_INPUT
        );
    }
    // 超过 ABI 抽头上限。
    let too_long = vec![0.0f64; khaen_core::plan::MAX_CROSSOVER_TAPS as usize + 1];
    assert_eq!(
        compile_fir_plan(
            &[LaneFir {
                lane: 0,
                taps: too_long,
            }],
            &config(1)
        )
        .expect_err("too long")
        .code(),
        BAD_INPUT
    );
}

#[test]
fn invalid_config_is_config_error() {
    let lanes = [LaneFir {
        lane: 0,
        taps: vec![1.0],
    }];
    for bad in [
        FirPlanConfig::new(100, SAMPLE_RATE, 1),
        FirPlanConfig::new(BLOCK_SIZE, 44_100, 1),
        FirPlanConfig::new(BLOCK_SIZE, SAMPLE_RATE, 0),
    ] {
        assert_eq!(
            compile_fir_plan(&lanes, &bad)
                .expect_err("bad config")
                .code(),
            CONFIG
        );
    }
    // FIR 条数多于通道数。
    assert_eq!(
        compile_fir_plan(
            &[
                LaneFir {
                    lane: 0,
                    taps: vec![1.0],
                },
                LaneFir {
                    lane: 1,
                    taps: vec![1.0],
                }
            ],
            &config(1)
        )
        .expect_err("too many lanes")
        .code(),
        CONFIG
    );
}

#[test]
fn build_and_encode_codes_are_distinct_constants() {
    // 稳定码互不相同，避免上层把错误类别混在一起。
    assert_ne!(BAD_INPUT, CONFIG);
    assert_ne!(BUILD, ENCODE);
    assert_ne!(BAD_INPUT, BUILD);
}
