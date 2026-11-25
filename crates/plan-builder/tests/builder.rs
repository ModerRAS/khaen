//! 正向测试：组装往返、确定性、host 准入与图编译、graph_id 语义。
//!
//! 覆盖任务 T32a 的验收点 a/b/c/e；错误码路径见 errors.rs。

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection, GRAPH_ID_DOMAIN,
};
use khaen_core::sha256::sha256_hex;
use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::graph::PlanGraph;
use khaen_host::{EngineConfig, Processor};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

const BLOCK_SIZE: u32 = 128;
const SAMPLE_RATE: u32 = 96_000;
const CHANNELS: u16 = 2;
/// +6 dB 的线性增益（10^(6/20)）；断言容差 1e-6。
const GAIN_6DB: f32 = 1.995_262;
/// ABI v3 trailer：4 字节 crc32c + 32 字节 graph_id。
const TRAILER_BYTES: usize = 36;
/// ABI v3 trailer 里的 graph_id 字节数。
const GRAPH_ID_BYTES: usize = 32;

fn config() -> PlanConfig {
    PlanConfig::new(BLOCK_SIZE, SAMPLE_RATE, CHANNELS)
}

fn engine() -> EngineConfig {
    EngineConfig {
        block_frames: BLOCK_SIZE as usize,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS as usize,
    }
}

/// 手工写出的 2x2 identity 方案，逐字段与 PlanBuilder::identity 对照。
fn identity_expected() -> Plan {
    Plan {
        block_size: BLOCK_SIZE,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: CHANNELS,
            out_lanes: CHANNELS,
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
        crossover: Some(CrossoverSection {
            lanes: CHANNELS,
            filters: vec![
                CrossoverFilter {
                    lane: 0,
                    coeffs: vec![1.0],
                },
                CrossoverFilter {
                    lane: 1,
                    coeffs: vec![1.0],
                },
            ],
        }),
        matrix: Some(MatrixSection {
            rows: CHANNELS,
            cols: CHANNELS,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: vec![1.0, 0.0, 0.0, 1.0],
        }),
        protection: Some(ProtectionSection {
            lanes: CHANNELS,
            entries: vec![
                ProtectionEntry {
                    lane: 0,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
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
            dac_channels: CHANNELS,
            routes: vec![(0, 0), (1, 1)],
        }),
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

fn assert_bit_eq(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "length mismatch");
    for (index, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a.to_bits(), e.to_bits(), "sample {index}: {a} vs {e}");
    }
}

fn compile_and_process(plan: &Plan, input: &[f32]) -> Vec<f32> {
    let limits = AdmissionLimits::default();
    let mut graph = PlanGraph::compile(plan, &engine(), &limits).expect("graph compile");
    let mut output = vec![0.0f32; input.len()];
    graph.process(input, &mut output);
    output
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// ABI v3 trailer 里的 32 字节 graph_id（十六进制）。
fn trailer_graph_id(bytes: &[u8]) -> String {
    let body_end = bytes.len() - TRAILER_BYTES;
    hex(&bytes[body_end + 4..body_end + 4 + GRAPH_ID_BYTES])
}

/// 按 CONTRACTS.md §A.3 重算：sha256("khaen-plan/v3" || 除 trailer 外全部字节)。
fn recompute_graph_id(bytes: &[u8]) -> String {
    let body_end = bytes.len() - TRAILER_BYTES;
    let mut graph_input = Vec::with_capacity(GRAPH_ID_DOMAIN.len() + body_end);
    graph_input.extend_from_slice(GRAPH_ID_DOMAIN);
    graph_input.extend_from_slice(&bytes[..body_end]);
    sha256_hex(&graph_input)
}

// a) 最小 2x2 identity：build -> encode -> decode -> 与原 Plan 全字段相等。
#[test]
fn identity_2x2_roundtrips_all_fields() {
    let builder = PlanBuilder::identity(config());
    let plan = builder.build().expect("identity plan must build");
    assert_eq!(plan, identity_expected());
    assert_eq!(plan.stage_count(), 5);
    assert_eq!(plan.block_size, BLOCK_SIZE);
    assert_eq!(plan.sample_rate, SAMPLE_RATE);

    let bytes = builder.build_bytes().expect("identity plan must encode");
    assert_eq!(bytes, plan.encode(), "build_bytes must equal Plan::encode");
    let decoded = Plan::decode(&bytes).expect("built bytes must decode");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.graph_id_hex(), plan.graph_id_hex());
    assert_eq!(
        decoded.encode(),
        bytes,
        "re-encoding must be byte-identical"
    );
}

// b) 同一输入构造两次：bytes 完全相同、graph_id 相同。
#[test]
fn build_is_deterministic() {
    let recipe = || PlanBuilder::identity(config());
    let first_bytes = recipe().build_bytes().expect("bytes");
    let second_bytes = recipe().build_bytes().expect("bytes");
    assert_eq!(first_bytes, second_bytes);

    let first_plan = recipe().build().expect("plan");
    let second_plan = recipe().build().expect("plan");
    assert_eq!(first_plan.graph_id_hex(), second_plan.graph_id_hex());

    // 同一个组装器上重复 build 也确定（build 不消耗也不修改组装器）。
    let builder = PlanBuilder::identity(config());
    assert_eq!(
        builder.build_bytes().expect("bytes"),
        builder.build_bytes().expect("bytes")
    );
    assert_eq!(
        builder.build().expect("plan").graph_id_hex(),
        builder.build().expect("plan").graph_id_hex()
    );
}

// c) 编出的字节能被 khaen_host::admit 接受且 PlanGraph::compile 成功，并且真的逐样本直通。
#[test]
fn identity_plan_is_admitted_and_compiles_to_passthrough() {
    let plan = PlanBuilder::identity(config()).build().expect("plan");
    let limits = AdmissionLimits::default();
    let admission = admit(&plan, &limits);
    assert!(admission.accepted, "{}", admission.summary());
    assert_eq!(admission.budget.lanes, CHANNELS);
    assert_eq!(admission.budget.dac_channels, CHANNELS);
    // 分频 2 + 矩阵 4 = 6 个系数（identity 段没有 PEQ）。
    assert_eq!(admission.budget.coefficients, 6);

    let input = signal(BLOCK_SIZE as usize * CHANNELS as usize, 0x51ce);
    let output = compile_and_process(&plan, &input);
    assert_bit_eq(&output, &input);
}

// c) 只设置 matrix 的最小方案：host 把缺省区段按恒等处理，仍然可准入 / 可编译 / 直通。
#[test]
fn matrix_only_plan_is_admitted_and_compiles() {
    let builder = PlanBuilder::new(config()).set_matrix_identity(CHANNELS);
    let plan = builder.build().expect("minimal plan must build");
    assert!(plan.content.is_none());
    assert!(plan.routing.is_none());
    assert!(plan.protection.is_none());
    assert!(plan.crossover.is_none());

    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());

    let input = signal(BLOCK_SIZE as usize * CHANNELS as usize, 0x0bad);
    let output = compile_and_process(&plan, &input);
    assert_bit_eq(&output, &input);
}

// lane 链：输入 2 通道 -> 4 lane -> 2 DAC（未映射的 DAC 静音）应当能 build 并编译。
#[test]
fn lane_count_can_exceed_channel_count() {
    let lanes = 4u16;
    let builder = PlanBuilder::new(config())
        .set_content_layout(
            CHANNELS,
            lanes,
            [
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
        )
        .set_matrix_identity(lanes)
        .set_protection_identity(lanes)
        .set_routing_map(CHANNELS, [(0, 0), (1, 1)]);
    let plan = builder.build().expect("lane expansion must build");

    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());
    assert_eq!(admission.budget.lanes, lanes);
    assert_eq!(admission.budget.dac_channels, CHANNELS);
    let mut graph = PlanGraph::compile(&plan, &engine(), &AdmissionLimits::default())
        .expect("lane expansion must compile");
    let input = signal(BLOCK_SIZE as usize * CHANNELS as usize, 0x1234);
    let mut output = vec![0.0f32; input.len()];
    graph.process(&input, &mut output);
    // 前两个 DAC 直通，其余不存在；输入只有 2 通道，所以输出仍应逐样本等于输入。
    assert_bit_eq(&output, &input);
}

// 非 identity 方案：矩阵交叉点 + 保护电平 + 路由交换，编译后数值符合手算。
#[test]
fn nontrivial_plan_compiles_and_processes() {
    // 行优先 2x2：row0 = [1.0, 0.5]，row1 = [0.5, 1.0]。
    let builder = PlanBuilder::new(config())
        .set_content_identity(CHANNELS)
        .set_matrix_cells(CHANNELS, CHANNELS, 1, 1, 1, [1.0, 0.5, 0.5, 1.0])
        .set_protection_gain_db(0, 6.0)
        .set_routing_map(CHANNELS, [(0, 1), (1, 0)]);
    let plan = builder.build().expect("plan must build");

    let admission = admit(&plan, &AdmissionLimits::default());
    assert!(admission.accepted, "{}", admission.summary());

    let input = signal(BLOCK_SIZE as usize * CHANNELS as usize, 0x7ea5);
    let output = compile_and_process(&plan, &input);
    assert_eq!(output.len(), input.len());
    for (frame, dac_pair) in output.chunks(2).enumerate() {
        let x0 = input[frame * 2];
        let x1 = input[frame * 2 + 1];
        // matrix row1 -> protection lane1（identity）-> dac0。
        let expected_dac0 = 0.5f32 * x0 + x1;
        // matrix row0 -> protection lane0（+6 dB）-> dac1。
        let expected_dac1 = (x0 + 0.5f32 * x1) * GAIN_6DB;
        assert!(
            (dac_pair[0] - expected_dac0).abs() <= 1e-6,
            "frame {frame}: dac0 {} vs {expected_dac0}",
            dac_pair[0]
        );
        assert!(
            (dac_pair[1] - expected_dac1).abs() <= 1e-6,
            "frame {frame}: dac1 {} vs {expected_dac1}",
            dac_pair[1]
        );
    }
}

// e) graph_id 语义：sha256(domain || body)，trailer 里就是它；文件 sha256 是另一层。
#[test]
fn graph_id_matches_sha256_of_domain_and_body() {
    let builder = PlanBuilder::identity(config());
    let plan = builder.build().expect("plan");
    let bytes = builder.build_bytes().expect("bytes");
    let expected = recompute_graph_id(&bytes);
    // ABI 层（trailer）的 graph_id 就是 sha256("khaen-plan/v3" || body 全部字节)。
    assert_eq!(trailer_graph_id(&bytes), expected);
    // 整文件 sha256 与 graph_id 是不同的东西（控制面 prepare 校验前者，图身份用后者）。
    assert_ne!(sha256_hex(&bytes), expected);

    // NOTE(core-defect, 只报告不改 core)：khaen_core::plan::Plan::graph_id_hex() 目前把 trailer
    // 里的 32 字节 graph_id 又做了一次 sha256 再转 hex，与 CONTRACTS.md §A.3 的 graph_id 语义
    // 对不上（等于 sha256(sha256(domain || body))）。本 crate 不依赖它的具体值，只锁它是稳定的
    // 64 位小写 hex；core 修好后这里仍然成立。
    let high_level_id = plan.graph_id_hex();
    assert_eq!(high_level_id.len(), 64);
    assert!(
        high_level_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "graph_id_hex must be 64 lowercase hex chars: {high_level_id}"
    );
}

// e) 改一个增益必然改变 bytes 与 graph_id。
#[test]
fn changing_one_gain_changes_bytes_and_graph_id() {
    let base = PlanBuilder::identity(config());
    let mut protection = base.protection().cloned().expect("identity protection");
    protection.entries[0].gain_db = 6.0;
    let changed = base.clone().set_protection(protection);

    let base_plan = base.build().expect("plan");
    let changed_plan = changed.build().expect("plan");
    let base_bytes = base.build_bytes().expect("bytes");
    let changed_bytes = changed.build_bytes().expect("bytes");
    assert_ne!(base_bytes, changed_bytes);
    // ABI 层 graph_id 与整文件 sha256 都必须变化。
    assert_ne!(
        trailer_graph_id(&base_bytes),
        trailer_graph_id(&changed_bytes)
    );
    assert_ne!(sha256_hex(&base_bytes), sha256_hex(&changed_bytes));
    // 高层接口 graph_id_hex 同样变化（当前是稳定但多做一次散列，见上个测试的 NOTE）。
    assert_ne!(base_plan.graph_id_hex(), changed_plan.graph_id_hex());
    // 只改电平，其他区段逐字段相同。
    let mut expected = base_plan.clone();
    expected.protection = changed_plan.protection.clone();
    assert_eq!(expected, changed_plan);
}

// e) ABI v3 没有"不入散列的元数据"：block_size / sample_rate 也在 body 头部、被 graph_id 覆盖。
// 改它们同样改 graph_id（图身份包含运行条件，这是有意设计，不是漏项）。
#[test]
fn header_fields_participate_in_graph_id() {
    let builder_96k = PlanBuilder::identity(config());
    let builder_48k = PlanBuilder::identity(PlanConfig::new(BLOCK_SIZE, 48_000, CHANNELS));
    let builder_64 = PlanBuilder::identity(PlanConfig::new(64, SAMPLE_RATE, CHANNELS));
    let plan_96k = builder_96k.build().expect("plan");
    let plan_48k = builder_48k.build().expect("plan");

    let bytes_96k = builder_96k.build_bytes().expect("bytes");
    let bytes_48k = builder_48k.build_bytes().expect("bytes");
    let bytes_64 = builder_64.build_bytes().expect("bytes");
    assert_ne!(trailer_graph_id(&bytes_96k), trailer_graph_id(&bytes_48k));
    assert_ne!(trailer_graph_id(&bytes_96k), trailer_graph_id(&bytes_64));
    // 除头部外的区段完全相同：只有配置层字段不同，id 仍然不同。
    assert_eq!(plan_96k.content, plan_48k.content);
    assert_eq!(plan_96k.matrix, plan_48k.matrix);
    assert_eq!(plan_96k.routing, plan_48k.routing);
}
