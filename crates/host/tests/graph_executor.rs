//! 图执行器测试：identity 逐位直通、content 折叠、matrix、protection、routing、
//! 块长无关与编译期错误路径。

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    ProtectionEntry, ProtectionSection, RoutingSection,
};
use khaen_host::{AdmissionLimits, CompileError, EngineConfig, PlanGraph, Processor};

const SAMPLE_RATE: u32 = 96_000;

fn engine_config(channels: usize, block_frames: usize) -> EngineConfig {
    EngineConfig {
        block_frames,
        sample_rate: SAMPLE_RATE,
        input_channels: channels,
        output_channels: channels,
    }
}

/// 确定性伪随机测试信号：值域 (-1, 1)，只用整数 LCG 与 f32 除法，跨平台一致。
fn signal(samples: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = (state >> 8) as f32 / (1u32 << 24) as f32;
        out.push(unit * 2.0 - 1.0);
    }
    out
}

fn run(graph: &mut PlanGraph, input: &[f32]) -> Vec<f32> {
    let mut output = vec![0.0f32; input.len()];
    graph.process(input, &mut output);
    output
}

/// 按固定块长多次调用 process（模拟 BlockEngine 的块边界），拼接输出。
fn run_in_blocks(
    graph: &mut PlanGraph,
    input: &[f32],
    channels: usize,
    frames_per_block: usize,
) -> Vec<f32> {
    let samples_per_block = frames_per_block * channels;
    let mut output = Vec::with_capacity(input.len());
    for chunk in input.chunks(samples_per_block) {
        let mut block_out = vec![0.0f32; chunk.len()];
        graph.process(chunk, &mut block_out);
        output.extend_from_slice(&block_out);
    }
    output
}

/// identity 判据要求 bit-exact：逐样本按位比较。
fn assert_bit_eq(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "length mismatch");
    for (index, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a.to_bits(), e.to_bits(), "sample {index}: {a} vs {e}");
    }
}

fn channel_values(input: &[f32], channels: usize, channel: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(input.len() / channels);
    let mut frame = 0;
    while frame * channels < input.len() {
        values.push(input[frame * channels + channel]);
        frame += 1;
    }
    values
}

/// 全恒等方案（任务要求第 5 条的 identity 判据）。
fn identity_plan(block_frames: u32, channels: u16) -> Plan {
    let lanes = channels;
    let count = channels as usize;
    let mut matrix_coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        matrix_coeffs[lane * count + lane] = 1.0;
    }
    Plan {
        block_size: block_frames,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: channels,
            out_lanes: lanes,
            folds: (0..lanes)
                .map(|lane| Fold {
                    src: lane,
                    dst: lane,
                    gain: 1.0,
                })
                .collect(),
        }),
        crossover: Some(CrossoverSection {
            lanes,
            filters: (0..lanes)
                .map(|lane| CrossoverFilter {
                    lane,
                    coeffs: vec![1.0],
                })
                .collect(),
        }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection {
            lanes,
            entries: (0..lanes)
                .map(|lane| ProtectionEntry {
                    lane,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                })
                .collect(),
        }),
        routing: Some(RoutingSection {
            dac_channels: channels,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

/// identity 判据：逐段断言方案确实是恒等链（任务要求第 5 条）。
fn assert_identity_criteria(plan: &Plan) {
    let content = plan.content.as_ref().expect("content");
    let lanes = content.out_lanes;
    assert_eq!(content.in_channels, lanes, "content: in_channels == lanes");
    for lane in 0..lanes {
        let mut folds = content.folds.iter().filter(|fold| fold.dst == lane);
        let fold = folds.next().expect("每 lane 必须有一个 fold");
        assert!(folds.next().is_none(), "每 lane 只能有一个 fold");
        assert_eq!(fold.src, lane, "fold src == dst");
        assert_eq!(fold.gain, 1.0, "fold gain == 1.0");
    }
    let crossover = plan.crossover.as_ref().expect("crossover");
    assert_eq!(crossover.lanes, lanes);
    for filter in &crossover.filters {
        assert_eq!(filter.coeffs, vec![1.0], "crossover taps == [1.0]");
    }
    let matrix = plan.matrix.as_ref().expect("matrix");
    let count = lanes as usize;
    assert_eq!(matrix.rows, lanes);
    assert_eq!(matrix.cols, lanes);
    assert_eq!(matrix.decimation, 1);
    assert_eq!(matrix.interpolation, 1);
    assert_eq!(matrix.taps, 1);
    for row in 0..count {
        for col in 0..count {
            let expected = if row == col { 1.0 } else { 0.0 };
            assert_eq!(matrix.coeffs[row * count + col], expected);
        }
    }
    let protection = plan.protection.as_ref().expect("protection");
    assert_eq!(protection.lanes, lanes);
    for lane in 0..lanes {
        let entry = protection
            .entries
            .iter()
            .find(|entry| entry.lane == lane)
            .expect("每 lane 必须有一条保护项");
        assert_eq!(entry.delay, 0);
        assert_eq!(entry.polarity, 1);
        assert_eq!(entry.gain_db, 0.0);
        for section in &entry.peq {
            assert_eq!(section.b0, 1.0);
            assert_eq!(section.b1, 0.0);
            assert_eq!(section.b2, 0.0);
            assert_eq!(section.a1, 0.0);
            assert_eq!(section.a2, 0.0);
        }
    }
    let routing = plan.routing.as_ref().expect("routing");
    assert_eq!(routing.dac_channels, lanes);
    for lane in 0..lanes {
        assert!(
            routing.routes.contains(&(lane, lane)),
            "routing: lane {lane} -> dac {lane}"
        );
    }
}

#[test]
fn identity_chain_is_bit_exact_at_64_and_128_frames() {
    for block_frames in [64usize, 128] {
        let plan = identity_plan(block_frames as u32, 2);
        assert_identity_criteria(&plan);
        let config = engine_config(2, block_frames);
        let mut graph =
            PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
        for block in 0..4u32 {
            let input = signal(block_frames * 2, 0x51ce + block);
            let output = run(&mut graph, &input);
            assert_bit_eq(&output, &input);
        }
    }
}

#[test]
fn encoded_identity_plan_compiles_and_passes_through() {
    let plan = identity_plan(128, 2);
    let bytes = plan.encode();
    let decoded = Plan::decode(&bytes).expect("decode");
    let config = engine_config(2, 128);
    let mut graph =
        PlanGraph::compile(&decoded, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(256, 7);
    let output = run(&mut graph, &input);
    assert_bit_eq(&output, &input);
}

#[test]
fn content_folds_sum_channels_and_map_lanes() {
    let plan = Plan {
        block_size: 64,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: 2,
            out_lanes: 1,
            folds: vec![
                Fold {
                    src: 0,
                    dst: 0,
                    gain: 0.5,
                },
                Fold {
                    src: 1,
                    dst: 0,
                    gain: 0.25,
                },
            ],
        }),
        crossover: None,
        matrix: Some(MatrixSection {
            rows: 2,
            cols: 1,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: vec![1.0, 1.0],
        }),
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: 2,
            routes: vec![(0, 0), (1, 1)],
        }),
    };
    let config = engine_config(2, 64);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(64 * 2, 11);
    let output = run(&mut graph, &input);
    let mut frame = 0;
    while frame * 2 < input.len() {
        let expected = 0.5 * input[frame * 2] + 0.25 * input[frame * 2 + 1];
        assert_eq!(
            output[frame * 2].to_bits(),
            expected.to_bits(),
            "frame {frame} lane 0"
        );
        assert_eq!(
            output[frame * 2 + 1].to_bits(),
            expected.to_bits(),
            "frame {frame} lane 1"
        );
        frame += 1;
    }
}

#[test]
fn protection_delay_is_exact_in_samples() {
    let block_frames = 128usize;
    let mut plan = identity_plan(block_frames as u32, 2);
    if let Some(protection) = &mut plan.protection {
        protection.entries[0].delay = 5;
    }
    let config = engine_config(2, block_frames);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(block_frames * 2 * 3, 13);
    let output = run_in_blocks(&mut graph, &input, 2, block_frames);
    let mut expected = vec![0.0f32; input.len()];
    let mut frame = 0;
    while frame * 2 < input.len() {
        if frame >= 5 {
            expected[frame * 2] = input[(frame - 5) * 2];
        }
        expected[frame * 2 + 1] = input[frame * 2 + 1];
        frame += 1;
    }
    assert_bit_eq(&output, &expected);
}

#[test]
fn protection_polarity_flips_the_lane() {
    let block_frames = 128usize;
    let mut plan = identity_plan(block_frames as u32, 2);
    if let Some(protection) = &mut plan.protection {
        protection.entries[0].polarity = -1;
    }
    let config = engine_config(2, block_frames);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(block_frames * 2 * 2, 17);
    let output = run_in_blocks(&mut graph, &input, 2, block_frames);
    let mut expected = input.clone();
    let mut frame = 0;
    while frame * 2 < input.len() {
        expected[frame * 2] = -input[frame * 2];
        frame += 1;
    }
    assert_bit_eq(&output, &expected);
}

#[test]
fn protection_gain_db_uses_linear_conversion() {
    let block_frames = 128usize;
    let mut plan = identity_plan(block_frames as u32, 2);
    if let Some(protection) = &mut plan.protection {
        protection.entries[0].gain_db = 6.0;
        protection.entries[1].gain_db = -6.0;
    }
    let config = engine_config(2, block_frames);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(block_frames * 2, 19);
    let output = run(&mut graph, &input);
    let positive = 10f32.powf(6.0 / 20.0);
    let negative = 10f32.powf(-6.0 / 20.0);
    // ±6 dB 的线性换算：10^(±6/20) = 1.9952623 / 0.5011872。
    assert!((positive - 1.995_262_3).abs() < 1.0e-6);
    assert!((negative - 0.501_187_2).abs() < 1.0e-6);
    let mut frame = 0;
    while frame * 2 < input.len() {
        assert_eq!(
            output[frame * 2].to_bits(),
            (input[frame * 2] * positive).to_bits(),
            "frame {frame} lane 0"
        );
        assert_eq!(
            output[frame * 2 + 1].to_bits(),
            (input[frame * 2 + 1] * negative).to_bits(),
            "frame {frame} lane 1"
        );
        frame += 1;
    }
}

/// 独立直接型 biquad 参考实现（与图内的 Direct Form I 逐步同式）。
fn biquad_reference(sections: &[PeqSection], input: &[f32]) -> Vec<f32> {
    let mut output = vec![0.0f32; input.len()];
    let mut state = vec![0.0f32; sections.len() * 4];
    for (index, &sample) in input.iter().enumerate() {
        let mut y = sample;
        let mut section_index = 0;
        while section_index < sections.len() {
            let section = &sections[section_index];
            let start = section_index * 4;
            let section_state = &mut state[start..start + 4];
            let next =
                section.b0 * y + section.b1 * section_state[0] + section.b2 * section_state[1]
                    - section.a1 * section_state[2]
                    - section.a2 * section_state[3];
            section_state[1] = section_state[0];
            section_state[0] = y;
            section_state[3] = section_state[2];
            section_state[2] = next;
            y = next;
            section_index += 1;
        }
        output[index] = y;
    }
    output
}

#[test]
fn protection_single_peq_matches_direct_form_reference() {
    let block_frames = 128usize;
    let sections = [PeqSection {
        b0: 0.9,
        b1: 0.1,
        b2: 0.0,
        a1: -0.5,
        a2: 0.25,
    }];
    let mut plan = identity_plan(block_frames as u32, 2);
    if let Some(protection) = &mut plan.protection {
        protection.entries[0].peq = sections.to_vec();
    }
    let config = engine_config(2, block_frames);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(block_frames * 2 * 2, 23);
    let output = run_in_blocks(&mut graph, &input, 2, block_frames);
    let lane0 = channel_values(&input, 2, 0);
    let lane1 = channel_values(&input, 2, 1);
    let reference = biquad_reference(&sections, &lane0);
    let mut frame = 0;
    while frame * 2 < output.len() {
        assert_eq!(
            output[frame * 2].to_bits(),
            reference[frame].to_bits(),
            "peq frame {frame}"
        );
        assert_eq!(
            output[frame * 2 + 1].to_bits(),
            lane1[frame].to_bits(),
            "untouched lane frame {frame}"
        );
        frame += 1;
    }
}

#[test]
fn routing_maps_lanes_and_keeps_unmapped_dacs_silent() {
    let block_frames = 128usize;
    let mut matrix_coeffs = vec![0.0f32; 9];
    for lane in 0..3 {
        matrix_coeffs[lane * 3 + lane] = 1.0;
    }
    let plan = Plan {
        block_size: block_frames as u32,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: 3,
            out_lanes: 3,
            folds: (0..3)
                .map(|lane| Fold {
                    src: lane,
                    dst: lane,
                    gain: 1.0,
                })
                .collect(),
        }),
        crossover: None,
        matrix: Some(MatrixSection {
            rows: 3,
            cols: 3,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: 3,
            routes: vec![(2, 0), (0, 2)],
        }),
    };
    let config = engine_config(3, block_frames);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let input = signal(block_frames * 3, 29);
    let output = run(&mut graph, &input);
    let mut frame = 0;
    while frame * 3 < input.len() {
        assert_eq!(
            output[frame * 3].to_bits(),
            input[frame * 3 + 2].to_bits(),
            "dac 0 <- lane 2 at frame {frame}"
        );
        assert_eq!(
            output[frame * 3 + 1].to_bits(),
            0.0f32.to_bits(),
            "dac 1 unmapped at frame {frame}"
        );
        assert_eq!(
            output[frame * 3 + 2].to_bits(),
            input[frame * 3].to_bits(),
            "dac 2 <- lane 0 at frame {frame}"
        );
        frame += 1;
    }
}

#[test]
fn full_rate_matrix_matches_naive_convolution_reference() {
    let block_frames = 128usize;
    let rows = 3usize;
    let cols = 2usize;
    let taps = 3usize;
    let mut coeffs = Vec::with_capacity(rows * cols * taps);
    for row in 0..rows {
        for col in 0..cols {
            for tap in 0..taps {
                let value = ((row * 7 + col * 3 + tap * 5) % 11) as f32 / 11.0 - 0.5;
                coeffs.push(value);
            }
        }
    }
    let plan = Plan {
        block_size: block_frames as u32,
        sample_rate: SAMPLE_RATE,
        content: None,
        crossover: None,
        matrix: Some(MatrixSection {
            rows: rows as u16,
            cols: cols as u16,
            decimation: 1,
            interpolation: 1,
            taps: taps as u32,
            coeffs,
        }),
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: 2,
            routes: vec![(0, 0), (2, 1)],
        }),
    };
    let config = engine_config(2, block_frames);
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let frames_total = block_frames * 4;
    let input = signal(frames_total * 2, 31);
    let output = run_in_blocks(&mut graph, &input, 2, block_frames);
    let samples = [channel_values(&input, 2, 0), channel_values(&input, 2, 1)];
    let matrix = plan.matrix.as_ref().expect("matrix");
    let mut expected = vec![0.0f32; input.len()];
    let mut frame = 0;
    while frame < frames_total {
        let mut acc = [0.0f64; 3];
        let mut row = 0;
        while row < rows {
            let mut col = 0;
            while col < cols {
                let values = &samples[col];
                let mut tap = 0;
                while tap < taps {
                    let coeff = f64::from(matrix.coeffs[(row * cols + col) * taps + tap]);
                    let sample = if frame >= tap {
                        f64::from(values[frame - tap])
                    } else {
                        0.0
                    };
                    acc[row] += coeff * sample;
                    tap += 1;
                }
                col += 1;
            }
            row += 1;
        }
        expected[frame * 2] = acc[0] as f32;
        expected[frame * 2 + 1] = acc[2] as f32;
        frame += 1;
    }
    let mut max_error = 0.0f32;
    for (actual, reference) in output.iter().zip(expected.iter()) {
        max_error = max_error.max((actual - reference).abs());
    }
    assert!(max_error < 1.0e-5, "max matrix error {max_error}");
    assert!(output.iter().any(|sample| *sample != 0.0));
}

/// 非平凡链：content fold、crossover FIR、满矩阵、保护（延迟/极性/电平/PEQ）、identity 路由。
fn busy_plan(block_frames: u32) -> Plan {
    Plan {
        block_size: block_frames,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: 2,
            out_lanes: 2,
            folds: vec![
                Fold {
                    src: 0,
                    dst: 0,
                    gain: 0.75,
                },
                Fold {
                    src: 1,
                    dst: 1,
                    gain: 1.0,
                },
            ],
        }),
        crossover: Some(CrossoverSection {
            lanes: 2,
            filters: vec![
                CrossoverFilter {
                    lane: 0,
                    coeffs: vec![0.5, 0.25, 0.125],
                },
                CrossoverFilter {
                    lane: 1,
                    coeffs: vec![1.0],
                },
            ],
        }),
        matrix: Some(MatrixSection {
            rows: 2,
            cols: 2,
            decimation: 1,
            interpolation: 1,
            taps: 2,
            coeffs: vec![
                1.0, 0.0, // row 0, col 0
                0.5, -0.25, // row 0, col 1
                0.0, 0.0, // row 1, col 0（全零格子跳过）
                0.75, 0.0, // row 1, col 1
            ],
        }),
        protection: Some(ProtectionSection {
            lanes: 2,
            entries: vec![
                ProtectionEntry {
                    lane: 0,
                    delay: 3,
                    polarity: -1,
                    gain_db: 2.0,
                    peq: vec![PeqSection {
                        b0: 0.9,
                        b1: 0.1,
                        b2: 0.0,
                        a1: -0.5,
                        a2: 0.25,
                    }],
                },
                ProtectionEntry {
                    lane: 1,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                },
            ],
        }),
        routing: Some(RoutingSection {
            dac_channels: 2,
            routes: vec![(0, 0), (1, 1)],
        }),
    }
}

#[test]
fn output_is_independent_of_block_length() {
    let plan = busy_plan(256);
    let config = engine_config(2, 256);
    let input = signal(256 * 2, 37);

    let mut whole =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let whole_output = run(&mut whole, &input);

    let mut split =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile");
    let split_output = run_in_blocks(&mut split, &input, 2, 64);

    assert_bit_eq(&split_output, &whole_output);
}

#[test]
fn unsupported_matrix_rates_are_rejected() {
    let mut plan = identity_plan(128, 2);
    if let Some(matrix) = &mut plan.matrix {
        matrix.decimation = 8;
        matrix.interpolation = 1;
    }
    let config = engine_config(2, 128);
    let error = PlanGraph::compile(&plan, &config, &AdmissionLimits::default())
        .expect_err("decimation != 1 must be rejected");
    assert_eq!(
        error,
        CompileError::UnsupportedRates {
            decimation: 8,
            interpolation: 1,
        }
    );
}

#[test]
fn channel_count_mismatch_is_rejected() {
    let plan = identity_plan(128, 2);
    let config = engine_config(3, 128);
    let error = PlanGraph::compile(&plan, &config, &AdmissionLimits::default())
        .expect_err("3 channels with a 2 channel plan must be rejected");
    match error {
        CompileError::ChannelMismatch {
            stage,
            expected,
            actual,
        } => {
            assert_eq!(stage, "content");
            assert_eq!(expected, 3);
            assert_eq!(actual, 2);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn admission_budget_rejection_is_fail_closed() {
    let plan = identity_plan(128, 2);
    let config = engine_config(2, 128);
    let limits = AdmissionLimits {
        max_coefficients: 3,
        ..AdmissionLimits::default()
    };
    let error =
        PlanGraph::compile(&plan, &config, &limits).expect_err("over budget plan must be rejected");
    match error {
        CompileError::AdmissionRejected(admission) => {
            assert!(!admission.accepted);
            assert!(
                admission
                    .rejections
                    .iter()
                    .any(|rejection| rejection.code == "coefficients"),
                "expected coefficients rejection, got {:?}",
                admission.rejections
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn block_size_and_sample_rate_mismatch_are_rejected() {
    let plan = identity_plan(128, 2);
    let error = PlanGraph::compile(&plan, &engine_config(2, 64), &AdmissionLimits::default())
        .expect_err("block size mismatch must be rejected");
    assert_eq!(
        error,
        CompileError::BlockSizeMismatch {
            plan: 128,
            config: 64,
        }
    );

    let mut wrong_rate = plan.clone();
    wrong_rate.sample_rate = 48_000;
    let error = PlanGraph::compile(
        &wrong_rate,
        &engine_config(2, 128),
        &AdmissionLimits::default(),
    )
    .expect_err("sample rate mismatch must be rejected");
    assert_eq!(
        error,
        CompileError::SampleRateMismatch {
            plan: 48_000,
            config: SAMPLE_RATE,
        }
    );
}

#[test]
fn lane_chain_mismatch_is_rejected() {
    let mut plan = identity_plan(128, 2);
    if let Some(crossover) = &mut plan.crossover {
        crossover.lanes = 3;
    }
    let error = PlanGraph::compile(&plan, &engine_config(2, 128), &AdmissionLimits::default())
        .expect_err("crossover lane chain mismatch must be rejected");
    assert_eq!(
        error,
        CompileError::LaneMismatch {
            stage: "crossover",
            expected: 2,
            actual: 3,
        }
    );
}

// ===========================================================================
// D-009 / IO I1：输入宽度与输出宽度解耦（2 输入 → 22 执行器/DAC）
// ===========================================================================

/// 2 路输入 → 22 个 lane/DAC 的完整图：content 只映射前两路，其余 lane 静音；
/// matrix 为 22×22 恒等；routing lane i -> dac i。这是“输入少、输出多”的最小真实形态。
fn split_2_to_22_plan(block_frames: u32) -> Plan {
    let lanes = 22u16;
    let count = usize::from(lanes);
    let mut coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        coeffs[lane * count + lane] = 1.0;
    }
    Plan {
        block_size: block_frames,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: 2,
            out_lanes: lanes,
            folds: vec![
                Fold {
                    src: 0,
                    dst: 0,
                    gain: 1.0,
                },
                Fold {
                    src: 1,
                    dst: 1,
                    gain: 1.0,
                },
            ],
        }),
        crossover: None,
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs,
        }),
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: lanes,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

fn split_engine_config(input_channels: usize, output_channels: usize) -> EngineConfig {
    EngineConfig {
        block_frames: 128,
        sample_rate: SAMPLE_RATE,
        input_channels,
        output_channels,
    }
}

#[test]
fn two_input_twenty_two_output_plan_routes_without_faking_input_width() {
    let plan = split_2_to_22_plan(128);
    let mut graph = PlanGraph::compile(
        &plan,
        &split_engine_config(2, 22),
        &AdmissionLimits::default(),
    )
    .expect("2-in / 22-out plan must compile");

    let frames = 8usize;
    let mut input = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        input[frame * 2] = 0.25;
        input[frame * 2 + 1] = -0.5;
    }
    let mut output = vec![0.0f32; frames * 22];
    graph.process(&input, &mut output);

    for frame in 0..frames {
        assert_eq!(output[frame * 22], 0.25, "L must pass through to DAC 0");
        assert_eq!(output[frame * 22 + 1], -0.5, "R must pass through to DAC 1");
        for dac in 2..22 {
            assert_eq!(
                output[frame * 22 + dac],
                0.0,
                "lane {dac} has no content fold and must stay silent"
            );
        }
    }
}

#[test]
fn split_widths_are_checked_separately_at_compile_time() {
    let plan = split_2_to_22_plan(128);
    // 输出宽度错：PlanGraph 必须按 routing.dac_channels 拒绝。
    let error = PlanGraph::compile(
        &plan,
        &split_engine_config(2, 21),
        &AdmissionLimits::default(),
    )
    .expect_err("output width mismatch must be rejected");
    assert!(matches!(
        error,
        CompileError::ChannelMismatch {
            stage: "routing",
            expected: 21,
            actual: 22
        }
    ));
    // 输入宽度错：必须按 content.in_channels 拒绝，不能用输出宽度兜底。
    let error = PlanGraph::compile(
        &plan,
        &split_engine_config(4, 22),
        &AdmissionLimits::default(),
    )
    .expect_err("input width mismatch must be rejected");
    assert!(matches!(
        error,
        CompileError::ChannelMismatch {
            stage: "content",
            expected: 4,
            actual: 2
        }
    ));
}

#[test]
fn zero_tap_matrix_and_duplicate_routing_are_rejected() {
    let mut zero_taps = identity_plan(128, 2);
    if let Some(matrix) = &mut zero_taps.matrix {
        matrix.taps = 0;
        matrix.coeffs.clear();
    }
    let error = PlanGraph::compile(
        &zero_taps,
        &engine_config(2, 128),
        &AdmissionLimits::default(),
    )
    .expect_err("zero tap matrix must be rejected");
    assert!(matches!(error, CompileError::InvalidSection(_)));

    let mut duplicate = identity_plan(128, 2);
    if let Some(routing) = &mut duplicate.routing {
        routing.routes = vec![(0, 0), (1, 0)];
    }
    let error = PlanGraph::compile(
        &duplicate,
        &engine_config(2, 128),
        &AdmissionLimits::default(),
    )
    .expect_err("duplicate dac routing must be rejected");
    assert!(matches!(error, CompileError::InvalidSection(_)));

    let mut long = identity_plan(128, 2);
    if let Some(crossover) = &mut long.crossover {
        crossover.filters[0].coeffs = vec![0.5; 4097];
    }
    let error = PlanGraph::compile(&long, &engine_config(2, 128), &AdmissionLimits::default())
        .expect_err("crossover taps beyond the direct kernel limit must be rejected");
    assert_eq!(
        error,
        CompileError::TapsTooLong {
            stage: "crossover",
            taps: 4097,
            limit: 4096,
        }
    );
}
