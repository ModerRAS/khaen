//! crossover 段分区卷积（UPOLS）验收测试（T39b）。
//!
//! 覆盖点：
//! 1. 2048 / 4096 taps 分区 lane 与 direct 内核逐块对拍（确定性随机信号，相对 L2 <= 1e-5）；
//! 2. 4096 taps 冲激响应逐样本等于系数（taps / 延迟 / 系数方向正确）；
//! 3. 阈值边界：taps <= 阈值仍走 direct 并与 direct 内核逐位一致；taps = 阈值 + 1 走分区，
//!    与直算不同位但落在容差内；
//! 4. 块长不变性（full-chunk 调用口径）：一次整段 vs 拆成多次调用逐位一致；B=64 与 B=128
//!    两条分区 plan 之间按容差一致（与 core 内核的 block-size 不变性同口径）；
//! 5. partial 末块对分区 lane 是终止性 DRAIN：末块输出仍正确，但之后从新 epoch 开始
//!    （与 direct lane 可拼接 partial 的语义不同，显式钉住）；
//! 6. B=1 时 Q = ceil(taps/B) > MAX_PARTITIONS，分区 plan 建不出来 -> 回退 direct；
//! 7. 22 lane × 2048 taps 默认尺寸（bench 同款形状）与 direct 内核逐 lane 对拍；
//! 8. taps 超 direct 上限（4097）的稳定错误码 TapsTooLong。
//!
//! T56（host 接 SIMD 派发）追加覆盖点：
//!
//! 9. 内核派发口径：`KernelChoice::Scalar` 的图与标量参考内核逐位一致（bit-exact 口径保留），
//!    `Auto` / `Optimized` 的图走核心优化入口；本机有 SIMD 后端时 direct 路径的输出不再与
//!    标量逐位相同（证明派发真的生效，而不是把 optimized 写成了标量）；
//! 10. SIMD parity 容差与 core 一致：direct 路径（≤ 阈值）单样本绝对 ≤ 1e-6、相对 L2 ≤ 1e-6；
//!     partitioned 路径（> 阈值）单样本绝对 ≤ 1e-4、相对 L2 ≤ 1e-5（core
//!     `tests/kernels_simd.rs` 的 DIRECT_TOL_* / PART_TOL_* 口径）；
//! 11. `PlanGraph` 暴露的派发快照是本机真实探测值（backend == SimdBackend::detect()，
//!     path 按选择正确解析），供 status / bench 读取，不编。
//!
//! 运行：cargo test -p khaen-host --offline --test crossover_partitioned。

use khaen_core::kernels::direct::{
    direct_block_f32, DirectCoeffsF32, DirectLayout, DirectTopology,
};
use khaen_core::kernels::dispatch::{direct_block_f32_optimized, SimdBackend};
use khaen_core::kernels::partitioned::PartitionedLayout;
use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, RoutingSection,
};
use khaen_host::graph::{KernelChoice, KernelPath, CROSSOVER_PARTITIONED_MIN_TAPS};
use khaen_host::{AdmissionLimits, CompileError, EngineConfig, PlanGraph, Processor};

const SAMPLE_RATE: u32 = 96_000;
/// 分区卷积与 direct 直算的对拍门：相对 L2（内核文档口径 f32 <= 1e-5）。
const REL_L2_BOUND: f64 = 1.0e-5;
/// 逐样本相对最大误差门（比 L2 宽松一档，防止极少数样本主导）。
const REL_MAX_BOUND: f64 = 1.0e-4;
/// direct 路径 SIMD vs 标量参考的容差，与 core `tests/kernels_simd.rs` 的
/// DIRECT_TOL_SAMPLE / DIRECT_TOL_L2 完全一致（单样本绝对 / 相对 L2）。
const DIRECT_TOL_SAMPLE: f32 = 1.0e-6;
const DIRECT_TOL_L2: f64 = 1.0e-6;
/// partitioned 路径 SIMD vs 标量参考的容差，与 core 的 PART_TOL_SAMPLE / PART_TOL_L2 一致。
const PART_TOL_SAMPLE: f32 = 1.0e-4;
const PART_TOL_L2: f64 = 1.0e-5;

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

/// 单位直流增益的低通抽头（DC 归一；镜像 core `kernels_simd.rs` 的对拍 workload）。
///
/// parity 的**绝对**容差口径（direct 单样本绝对 ≤ 1e-6）只有在输出量级 O(1) 时才有意义：
/// 随机抽头的 |h|1 随 taps 增长（96 taps 时输出 RMS ~5），绝对误差会随之线性放大。这里与
/// core 用同一套 DC 归一低通，保证 "容差与 core 一致" 是同一 workload 口径下的同一个门。
fn lowpass_fir(taps: usize, cutoff: f64) -> Vec<f32> {
    if taps == 1 {
        return vec![1.0];
    }
    let middle = (taps - 1) as f64 / 2.0;
    let mut coeffs = Vec::with_capacity(taps);
    let mut sum = 0.0f64;
    for index in 0..taps {
        let offset = index as f64 - middle;
        let angle = 2.0 * std::f64::consts::PI * cutoff * offset;
        let sinc = if angle.abs() < 1.0e-12 {
            1.0
        } else {
            angle.sin() / angle
        };
        let window =
            0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / (taps - 1) as f64).cos();
        let value = 2.0 * cutoff * sinc * window;
        coeffs.push(value as f32);
        sum += value;
    }
    if sum.abs() > f64::EPSILON {
        for value in &mut coeffs {
            *value = (*value as f64 / sum) as f32;
        }
    }
    coeffs
}

/// 两条 lane 的 DC 归一抽头（lane 1 用反序：同为低通、相位不同，避免两 lane 输出相关）。
///
/// taps = 1 时用非 1.0 的单抽头，避免被编译期 identity 折叠成直通（那样就测不到内核）。
fn lowpass_pair(taps_len: usize) -> (Vec<f32>, Vec<f32>) {
    if taps_len == 1 {
        return (vec![0.75], vec![-0.5]);
    }
    let forward = lowpass_fir(taps_len, 0.1);
    let mut reversed = forward.clone();
    reversed.reverse();
    (forward, reversed)
}

/// content 恒等映射：通道 i -> lane i，增益 1.0（编译期折叠为逐样本拷贝）。
fn identity_content(channels: u16) -> ContentSection {
    ContentSection {
        in_channels: channels,
        out_lanes: channels,
        folds: (0..channels)
            .map(|lane| Fold {
                src: lane,
                dst: lane,
                gain: 1.0,
            })
            .collect(),
    }
}

/// matrix 恒等：对角 1.0 的 1-tap 矩阵（编译期折叠为逐样本拷贝）。
fn identity_matrix(lanes: u16) -> MatrixSection {
    let count = lanes as usize;
    let mut coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        coeffs[lane * count + lane] = 1.0;
    }
    MatrixSection {
        rows: lanes,
        cols: lanes,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs,
    }
}

/// 只测 crossover 的方案：content / matrix / routing 全部恒等，protection 缺省。
///
/// 图输出 = 每 lane 的 crossover FIR 结果（direct 路径下与 direct 内核逐位一致）。
fn crossover_plan(block_frames: u32, lanes: u16, filters: Vec<(u16, Vec<f32>)>) -> Plan {
    Plan {
        block_size: block_frames,
        sample_rate: SAMPLE_RATE,
        content: Some(identity_content(lanes)),
        crossover: Some(CrossoverSection {
            lanes,
            filters: filters
                .into_iter()
                .map(|(lane, coeffs)| CrossoverFilter { lane, coeffs })
                .collect(),
        }),
        matrix: Some(identity_matrix(lanes)),
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: lanes,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

fn compile_graph(plan: &Plan, channels: usize, block_frames: usize) -> PlanGraph {
    PlanGraph::compile(
        plan,
        &engine_config(channels, block_frames),
        &AdmissionLimits::default(),
    )
    .expect("compile graph")
}

/// 按显式内核选择编译图（T56：`Scalar` = bit-exact 口径，`Auto`/`Optimized` = SIMD 派发）。
fn compile_graph_with_kernel(
    plan: &Plan,
    channels: usize,
    block_frames: usize,
    choice: KernelChoice,
) -> PlanGraph {
    PlanGraph::compile_with_kernel(
        plan,
        &engine_config(channels, block_frames),
        &AdmissionLimits::default(),
        choice,
    )
    .expect("compile graph with kernel choice")
}

/// 帧交错 -> 每通道一个平面。
fn planar(input: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let frames = input.len() / channels;
    let mut planes = vec![Vec::with_capacity(frames); channels];
    for frame in 0..frames {
        for (channel, plane) in planes.iter_mut().enumerate() {
            plane.push(input[frame * channels + channel]);
        }
    }
    planes
}

/// 帧交错输出取某一通道的样本序列。
fn channel_values(interleaved: &[f32], channels: usize, channel: usize) -> Vec<f32> {
    interleaved
        .iter()
        .skip(channel)
        .step_by(channels)
        .copied()
        .collect()
}

/// 用 direct 内核算一条 lane 的参考输出（1 源 / 1 角色 / 1 容器，g=0、p=1）。
///
/// chunks 是每次调用推进的有效帧数（必须 <= block）；返回拼接后的 valid 样本。
fn direct_lane_output(taps: &[f32], block: usize, planar: &[f32], chunks: &[usize]) -> Vec<f32> {
    direct_lane_output_with(KernelPath::Scalar, taps, block, planar, chunks)
}

/// 同上，但按显式内核路径调用（`Scalar` = 标量参考内膜，`Optimized` = 核心 SIMD 入口）。
fn direct_lane_output_with(
    path: KernelPath,
    taps: &[f32],
    block: usize,
    planar: &[f32],
    chunks: &[usize],
) -> Vec<f32> {
    let mut topo = DirectTopology::identity();
    topo.l_a = taps.len() as u16;
    let layout = DirectLayout::build(&topo).expect("direct layout");
    let coeffs = DirectCoeffsF32 {
        a_fir: taps,
        g: &[0.0f32],
        p: &[1.0f32],
    };
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut in_block = vec![0.0f32; block];
    let mut out_block = vec![0.0f32; block];
    let mut out = Vec::new();
    let mut offset = 0usize;
    for &valid in chunks {
        in_block[..valid].copy_from_slice(&planar[offset..offset + valid]);
        let status = match path {
            KernelPath::Scalar => direct_block_f32(
                &topo,
                &layout,
                &coeffs,
                &mut state,
                &in_block,
                block,
                &mut out_block,
                block,
                block,
                valid,
            ),
            KernelPath::Optimized => direct_block_f32_optimized(
                &topo,
                &layout,
                &coeffs,
                &mut state,
                &in_block,
                block,
                &mut out_block,
                block,
                block,
                valid,
            ),
        };
        assert!(status.is_ok(), "direct reference status {:?}", status);
        out.extend_from_slice(&out_block[..valid]);
        offset += valid;
    }
    out
}

fn run_whole(graph: &mut PlanGraph, input: &[f32]) -> Vec<f32> {
    let mut output = vec![0.0f32; input.len()];
    graph.process(input, &mut output);
    output
}

/// 按给定的每块帧数多次调用 process，拼接输出。
fn run_chunks(
    graph: &mut PlanGraph,
    input: &[f32],
    channels: usize,
    chunk_frames: &[usize],
) -> Vec<f32> {
    let expected: usize = chunk_frames.iter().sum();
    assert_eq!(
        input.len(),
        expected * channels,
        "chunk frames must cover input"
    );
    let mut output = Vec::with_capacity(input.len());
    let mut offset = 0usize;
    for &frames in chunk_frames {
        let start = offset * channels;
        let end = (offset + frames) * channels;
        let mut chunk_out = vec![0.0f32; end - start];
        graph.process(&input[start..end], &mut chunk_out);
        output.extend_from_slice(&chunk_out);
        offset += frames;
    }
    output
}

fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|v| f64::from(*v) * f64::from(*v)).sum()
}

/// 相对 L2 误差：||got - want||_2 / ||want||_2。
fn relative_l2(got: &[f32], want: &[f32]) -> f64 {
    assert_eq!(got.len(), want.len(), "length mismatch");
    let diff = energy(
        &got.iter()
            .zip(want.iter())
            .map(|(a, b)| a - b)
            .collect::<Vec<f32>>(),
    );
    let base = energy(want);
    if base == 0.0 {
        if diff == 0.0 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (diff / base).sqrt()
    }
}

/// 逐样本相对最大误差：max|got - want| / max|want|。
fn relative_max(got: &[f32], want: &[f32]) -> f64 {
    assert_eq!(got.len(), want.len(), "length mismatch");
    let max_diff = got
        .iter()
        .zip(want.iter())
        .map(|(a, b)| f64::from((a - b).abs()))
        .fold(0.0f64, f64::max);
    let max_want = want
        .iter()
        .map(|v| f64::from(v.abs()))
        .fold(0.0f64, f64::max);
    if max_want == 0.0 {
        max_diff
    } else {
        max_diff / max_want
    }
}

fn assert_agrees(got: &[f32], want: &[f32], what: &str) {
    let l2 = relative_l2(got, want);
    let m = relative_max(got, want);
    assert!(
        l2 <= REL_L2_BOUND,
        "{what}: 相对 L2 {l2:e} 超过 {REL_L2_BOUND:e}"
    );
    assert!(
        m <= REL_MAX_BOUND,
        "{what}: 相对最大误差 {m:e} 超过 {REL_MAX_BOUND:e}"
    );
    println!("{what}: 相对 L2 {l2:e} 相对最大 {m:e}");
}

fn assert_bits_eq(actual: &[f32], expected: &[f32], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: 长度不一致");
    for (index, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a.to_bits(), e.to_bits(), "{what}: 样本 {index} {a} vs {e}");
    }
}

/// 单样本绝对误差（与 core `kernels_simd.rs` 的 max_abs_diff_f32 同式）。
fn max_abs_diff(got: &[f32], want: &[f32]) -> f32 {
    assert_eq!(got.len(), want.len(), "长度不一致");
    got.iter()
        .zip(want.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max)
}

/// SIMD parity 断言：容差口径与 core `tests/kernels_simd.rs` 完全一致（绝对单样本 + 相对 L2）。
///
/// 返回 (max_abs, rel_l2) 供测试打印实测值；两个门同时成立才算通过（与 core 相同，不让
/// 极少数样本或整体能量任何一侧单独主导）。
fn assert_simd_parity(
    got: &[f32],
    scalar: &[f32],
    tol_sample: f32,
    tol_l2: f64,
    what: &str,
) -> (f32, f64) {
    let max_abs = max_abs_diff(got, scalar);
    let rel_l2 = relative_l2(got, scalar);
    assert!(
        max_abs <= tol_sample,
        "{what}: SIMD 单样本绝对误差 {max_abs:e} 超过 {tol_sample:e}（rel_l2={rel_l2:e}）"
    );
    assert!(
        rel_l2 <= tol_l2,
        "{what}: SIMD 相对 L2 {rel_l2:e} 超过 {tol_l2:e}（max_abs={max_abs:e}）"
    );
    println!("{what}: SIMD parity max_abs {max_abs:e} rel_l2 {rel_l2:e}");
    (max_abs, rel_l2)
}

/// 两段输出里是否存在逐位不同的样本（用于证明 optimized 派发真的换了内核）。
fn any_bit_difference(a: &[f32], b: &[f32]) -> bool {
    a.iter()
        .zip(b.iter())
        .any(|(x, y)| x.to_bits() != y.to_bits())
}

// ===========================================================================
// 1. 2048 / 4096 taps 与 direct 内核逐块对拍
// ===========================================================================

#[test]
fn partitioned_crossover_matches_direct_kernel_at_2048_and_4096_taps() {
    let lanes = 2usize;
    let block = 128usize;
    let blocks = 4usize;
    for taps_len in [2048usize, 4096] {
        let h0 = signal(taps_len, 0x5100 + taps_len as u32);
        let h1 = signal(taps_len, 0x9100 + taps_len as u32);
        let plan = crossover_plan(
            block as u32,
            lanes as u16,
            vec![(0, h0.clone()), (1, h1.clone())],
        );
        let mut graph = compile_graph(&plan, lanes, block);
        let input = signal(block * blocks * lanes, 0x1234 + taps_len as u32);
        let got = run_whole(&mut graph, &input);
        let planes = planar(&input, lanes);
        let lanes_taps = [h0.clone(), h1.clone()];
        for lane in 0..lanes {
            let want = direct_lane_output(&lanes_taps[lane], block, &planes[lane], &[block; 4]);
            let lane_out = channel_values(&got, lanes, lane);
            assert_agrees(
                &lane_out,
                &want,
                &format!("taps={taps_len} lane={lane} 分区 vs direct"),
            );
            assert!(
                energy(&lane_out) > 1.0,
                "taps={taps_len} lane={lane}: 输出全静音"
            );
        }
    }
}

// ===========================================================================
// 2. 冲激响应逐样本等于系数（taps / 延迟 / 方向）
// ===========================================================================

#[test]
fn partitioned_impulse_response_equals_coefficients_at_4096_taps() {
    let taps_len = 4096usize;
    let block = 128usize;
    let lanes = 1usize;
    let taps = signal(taps_len, 0x4096);
    let plan = crossover_plan(block as u32, 1, vec![(0, taps.clone())]);
    let mut graph = compile_graph(&plan, lanes, block);
    let blocks = taps_len.div_ceil(block) + 1;
    let frames = block * blocks;
    let mut input = vec![0.0f32; frames * lanes];
    input[0] = 1.0;
    let got = run_whole(&mut graph, &input);
    let lane_out = channel_values(&got, lanes, 0);
    // 前 taps_len 个输出样本 = 系数（因果卷积的冲激响应）。
    let response = &lane_out[..taps_len];
    assert_agrees(response, &taps, "4096-tap 冲激响应 vs 系数");
    // 冲激响应之外严格安静（同一容差门）。
    let tail = &lane_out[taps_len..];
    let max_tail = tail.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
    let max_taps = taps.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
    assert!(
        f64::from(max_tail) <= f64::from(max_taps) * REL_MAX_BOUND,
        "冲激响应尾部不安静：{max_tail} vs max|h| {max_taps}"
    );
}

// ===========================================================================
// 3. 阈值边界：<= 阈值走 direct，阈值 + 1 走分区；标量口径逐位、Auto 口径 SIMD parity
// ===========================================================================

#[test]
fn threshold_selects_direct_then_partitioned() {
    let threshold = CROSSOVER_PARTITIONED_MIN_TAPS;
    assert!(threshold >= 2, "阈值必须允许至少 2-tap 的 direct lane");
    let block = 128usize;
    let lanes = 2usize;
    let blocks = 4usize;
    let simd = SimdBackend::detect().is_simd();

    // 3a. taps = 1 / 16 / 阈值：仍走 direct 直算。
    //
    // 抽头用 DC 归一低通（同 core 对拍 workload）：parity 的绝对容差只有在这种输出量级下才
    // 与 core 的 DIRECT_TOL_SAMPLE 同义（见 lowpass_fir 文档）。
    for taps_len in [1usize, 16, threshold] {
        let (h0, h1) = lowpass_pair(taps_len);
        let plan = crossover_plan(
            block as u32,
            lanes as u16,
            vec![(0, h0.clone()), (1, h1.clone())],
        );
        let input = signal(block * blocks * lanes, 0x300 + taps_len as u32);
        let planes = planar(&input, lanes);
        let lanes_taps = [h0.clone(), h1.clone()];

        // 标量口径：KernelChoice::Scalar 的图与标量参考内核逐位一致（bit-exact 保留）。
        let mut scalar_graph = compile_graph_with_kernel(&plan, lanes, block, KernelChoice::Scalar);
        assert_eq!(scalar_graph.kernel_path(), KernelPath::Scalar);
        let scalar_out = run_whole(&mut scalar_graph, &input);

        // 默认（Auto）图：direct 路径接 core 优化入口（本机有 SIMD 时是 SIMD，否则标量回退）。
        let mut auto_graph = compile_graph(&plan, lanes, block);
        assert_eq!(auto_graph.simd_backend(), SimdBackend::detect());
        assert_eq!(
            auto_graph.kernel_path(),
            if simd {
                KernelPath::Optimized
            } else {
                KernelPath::Scalar
            },
            "Auto 应解析成本机可用路径"
        );
        let auto_out = run_whole(&mut auto_graph, &input);

        for lane in 0..lanes {
            let want = direct_lane_output(
                &lanes_taps[lane],
                block,
                &planes[lane],
                &vec![block; blocks],
            );
            assert_bits_eq(
                &channel_values(&scalar_out, lanes, lane),
                &want,
                &format!("taps={taps_len} lane={lane}: 标量选择应走 direct 且逐位一致"),
            );
            let auto_lane = channel_values(&auto_out, lanes, lane);
            assert_simd_parity(
                &auto_lane,
                &want,
                DIRECT_TOL_SAMPLE,
                DIRECT_TOL_L2,
                &format!("taps={taps_len} lane={lane}: direct 路径 Auto vs 标量参考"),
            );
            if simd && taps_len > 1 {
                assert!(
                    any_bit_difference(&auto_lane, &want),
                    "taps={taps_len} lane={lane}: 本机有 SIMD 但 optimized 输出与标量逐位相同，\
                     派发可能没有生效"
                );
            }
        }
    }

    // 3b. taps = 阈值 + 1：走分区，与直算数值不同位、但落在容差内。
    let taps_len = threshold + 1;
    let h0 = signal(taps_len, 0x400 + taps_len as u32);
    let h1 = signal(taps_len, 0x500 + taps_len as u32);
    let plan = crossover_plan(
        block as u32,
        lanes as u16,
        vec![(0, h0.clone()), (1, h1.clone())],
    );
    let input = signal(block * blocks * lanes, 0x600 + taps_len as u32);
    let planes = planar(&input, lanes);
    let lanes_taps = [h0.clone(), h1.clone()];
    let mut graph = compile_graph(&plan, lanes, block);
    let got = run_whole(&mut graph, &input);
    for lane in 0..lanes {
        let want = direct_lane_output(
            &lanes_taps[lane],
            block,
            &planes[lane],
            &vec![block; blocks],
        );
        let lane_out = channel_values(&got, lanes, lane);
        assert_agrees(
            &lane_out,
            &want,
            &format!("taps={taps_len} lane={lane}: > 阈值应走分区（容差对拍）"),
        );
        assert!(
            any_bit_difference(&lane_out, &want),
            "taps={taps_len} lane={lane}: 分区与 direct 逐位相同，阈值分支可能没有生效"
        );
    }
}

// ===========================================================================
// 4. 块长不变性（full-chunk 调用口径）
// ===========================================================================

#[test]
fn full_chunk_calls_are_block_length_invariant() {
    let taps_len = 2048usize;
    let block = 128usize;
    let lanes = 2usize;
    let total = 512usize;
    let h0 = signal(taps_len, 0x7a00);
    let h1 = signal(taps_len, 0x7b00);
    let plan = crossover_plan(
        block as u32,
        lanes as u16,
        vec![(0, h0.clone()), (1, h1.clone())],
    );
    let input = signal(total * lanes, 0x7c00);

    let mut whole_graph = compile_graph(&plan, lanes, block);
    let whole = run_whole(&mut whole_graph, &input);

    // 4 次 128 帧 vs 2 次 256 帧：每次调用帧数都是子块 B=128 的整数倍 -> 逐位一致。
    let mut by_block = compile_graph(&plan, lanes, block);
    let split_block = run_chunks(&mut by_block, &input, lanes, &[128, 128, 128, 128]);
    assert_bits_eq(&split_block, &whole, "128 帧连续 vs 整段");

    let mut by_two = compile_graph(&plan, lanes, block);
    let split_two = run_chunks(&mut by_two, &input, lanes, &[256, 256]);
    assert_bits_eq(&split_two, &whole, "256 帧连续 vs 整段");

    // B=64 的分区 plan（block_frames=64）与 B=128 之间按容差一致（与 core 内核同口径）。
    let plan64 = crossover_plan(64, lanes as u16, vec![(0, h0), (1, h1)]);
    let mut graph64 = compile_graph(&plan64, lanes, 64);
    let out64 = run_chunks(&mut graph64, &input, lanes, &[64; 8]);
    assert_agrees(&out64, &whole, "B=64 分区 vs B=128 分区");
}

// ===========================================================================
// 5. partial 末块对分区 lane 是终止性 DRAIN
// ===========================================================================

#[test]
fn partial_chunk_is_terminal_drain() {
    let taps_len = 2048usize;
    let block = 128usize;
    let lanes = 1usize;
    let taps = signal(taps_len, 0x8a00);
    let plan = crossover_plan(block as u32, 1, vec![(0, taps.clone())]);
    let input = signal(block, 0x8b00);
    let planes = planar(&input, lanes);

    let mut graph = compile_graph(&plan, lanes, block);
    let first_frames = 100usize;
    let second_frames = block - first_frames;

    // 第一次调用 100 帧（< B）：分区内核按 DRAIN 处理，[0,100) 仍是正确卷积值。
    let first = run_chunks(&mut graph, &input[..first_frames], lanes, &[first_frames]);
    let first_want = direct_lane_output(&taps, block, &planes[0][..first_frames], &[first_frames]);
    assert_agrees(&first, &first_want, "partial 末块首段输出");

    // DRAIN 之后历史已清零：下一次调用从新 epoch（零历史）开始，而不是接着上一块。
    let second = run_chunks(&mut graph, &input[first_frames..], lanes, &[second_frames]);
    let second_fresh =
        direct_lane_output(&taps, block, &planes[0][first_frames..], &[second_frames]);
    assert_agrees(&second, &second_fresh, "DRAIN 后新 epoch 输出");

    // direct 路径的对照：跨调用拼接 partial（历史保留）时第二段输出不同。
    let second_stitched =
        direct_lane_output(&taps, block, &planes[0], &[first_frames, second_frames]);
    let stitched_tail = &second_stitched[first_frames..];
    let difference = relative_l2(&second, stitched_tail);
    assert!(
        difference > 1.0e-3,
        "DRAIN 与 direct 拼接的输出差异过小（{difference:e}），测试可能没覆盖到语义差异"
    );
    println!("DRAIN vs direct 拼接：相对 L2 差异 {difference:e}");
}

// ===========================================================================
// 6. 块长 B=1 时 Q 超分区上限 -> 回退 direct
// ===========================================================================

#[test]
fn tiny_block_size_falls_back_to_direct() {
    let taps_len = 2048usize;
    let lanes = 1usize;
    let taps = signal(taps_len, 0x9001);

    // 分区布局在 B=1 时 Q = 2048 > MAX_PARTITIONS(1500)，建不出来（这是回退的触发条件）。
    let mut topo = DirectTopology::identity();
    topo.l_a = taps_len as u16;
    assert!(
        PartitionedLayout::build(&topo, 1).is_err(),
        "B=1、2048 taps 的 partitioned layout 应被拒绝"
    );

    let frames = 64usize;
    let plan = crossover_plan(1, 1, vec![(0, taps.clone())]);
    let input = signal(frames, 0x9002);
    let planes = planar(&input, lanes);
    let want = direct_lane_output(&taps, 1, &planes[0], &[1; 64]);

    // 标量口径：回退到 direct 后与 direct 标量内核逐位一致。
    let mut scalar_graph = compile_graph_with_kernel(&plan, lanes, 1, KernelChoice::Scalar);
    let scalar_out = run_chunks(&mut scalar_graph, &input, lanes, &[1; 64]);
    assert_bits_eq(
        &scalar_out,
        &want,
        "B=1 回退 direct 与 direct 标量内核逐位一致",
    );

    // Auto 口径：同样回退 direct，但接 core 优化入口（SIMD parity 与 core 同容差）。
    let mut auto_graph = compile_graph(&plan, lanes, 1);
    let auto_out = run_chunks(&mut auto_graph, &input, lanes, &[1; 64]);
    assert_simd_parity(
        &auto_out,
        &want,
        DIRECT_TOL_SAMPLE,
        DIRECT_TOL_L2,
        "B=1 回退 direct：Auto vs 标量参考",
    );
}

// ===========================================================================
// 7. 22 lane × 2048 taps 默认尺寸（bench 同款形状）
// ===========================================================================

#[test]
fn twenty_two_lane_2048_tap_default_shape_matches_direct() {
    let lanes = 22usize;
    let taps_len = 2048usize;
    let block = 128usize;
    let blocks = 2usize;
    let filters: Vec<(u16, Vec<f32>)> = (0..lanes)
        .map(|lane| (lane as u16, signal(taps_len, 0xc000 + lane as u32)))
        .collect();
    let plan = crossover_plan(block as u32, lanes as u16, filters.clone());
    let mut graph = compile_graph(&plan, lanes, block);
    let input = signal(block * blocks * lanes, 0xc100);
    let got = run_whole(&mut graph, &input);
    let planes = planar(&input, lanes);
    for (lane, filter) in filters.iter().enumerate() {
        let want = direct_lane_output(&filter.1, block, &planes[lane], &vec![block; blocks]);
        let lane_out = channel_values(&got, lanes, lane);
        assert_agrees(
            &lane_out,
            &want,
            &format!("22 lane 默认尺寸 lane={lane} 分区 vs direct"),
        );
        assert!(energy(&lane_out) > 1.0, "lane={lane}: 输出全静音");
    }
}

// ===========================================================================
// 8. taps 超 direct 上限的稳定错误码
// ===========================================================================

#[test]
fn taps_above_direct_limit_is_rejected() {
    let taps_len = 4097usize;
    let plan = crossover_plan(128, 1, vec![(0, signal(taps_len, 0xaa00))]);
    let error = PlanGraph::compile(&plan, &engine_config(1, 128), &AdmissionLimits::default())
        .expect_err("4097 taps must be rejected");
    assert_eq!(
        error,
        CompileError::TapsTooLong {
            stage: "crossover",
            taps: 4097,
            limit: 4096,
        }
    );
}

// ===========================================================================
// 9. SIMD parity：optimized 图 vs scalar 图，容差与 core 对拍口径一致
// ===========================================================================

#[test]
fn optimized_matches_scalar_within_core_tolerance_on_both_paths() {
    let block = 128usize;
    let lanes = 2usize;
    let blocks = 3usize;

    // direct 路径（≤ 阈值）：core 的 DIRECT_TOL_SAMPLE / DIRECT_TOL_L2。
    // 抽头用 DC 归一低通（core 对拍同款 workload），绝对容差与 core 同义。
    for taps_len in [2usize, 16, 96] {
        let (h0, h1) = lowpass_pair(taps_len);
        let plan = crossover_plan(block as u32, lanes as u16, vec![(0, h0), (1, h1)]);
        let input = signal(block * blocks * lanes, 0xd300 + taps_len as u32);
        let mut scalar_graph = compile_graph_with_kernel(&plan, lanes, block, KernelChoice::Scalar);
        let mut optimized_graph =
            compile_graph_with_kernel(&plan, lanes, block, KernelChoice::Optimized);
        assert_eq!(optimized_graph.kernel_path(), KernelPath::Optimized);
        let scalar_out = run_whole(&mut scalar_graph, &input);
        let optimized_out = run_whole(&mut optimized_graph, &input);
        for lane in 0..lanes {
            assert_simd_parity(
                &channel_values(&optimized_out, lanes, lane),
                &channel_values(&scalar_out, lanes, lane),
                DIRECT_TOL_SAMPLE,
                DIRECT_TOL_L2,
                &format!("direct 路径 taps={taps_len} lane={lane} optimized vs scalar"),
            );
        }
    }

    // partitioned 路径（> 阈值）：core 的 PART_TOL_SAMPLE / PART_TOL_L2。
    for taps_len in [2048usize, 4096] {
        let h = lowpass_fir(taps_len, 0.1);
        let plan = crossover_plan(block as u32, 1, vec![(0, h)]);
        let input = signal(block * blocks, 0xe200 + taps_len as u32);
        let mut scalar_graph = compile_graph_with_kernel(&plan, 1, block, KernelChoice::Scalar);
        let mut optimized_graph =
            compile_graph_with_kernel(&plan, 1, block, KernelChoice::Optimized);
        assert_eq!(optimized_graph.kernel_path(), KernelPath::Optimized);
        let scalar_out = run_whole(&mut scalar_graph, &input);
        let optimized_out = run_whole(&mut optimized_graph, &input);
        assert_simd_parity(
            &optimized_out,
            &scalar_out,
            PART_TOL_SAMPLE,
            PART_TOL_L2,
            &format!("partitioned 路径 taps={taps_len} optimized vs scalar"),
        );
    }
}

// ===========================================================================
// 10. 图侧派发快照（T56 ③）：真实探测值 + 选择解析，供 status / bench 读取
// ===========================================================================

#[test]
fn graph_reports_real_kernel_dispatch() {
    let block = 128usize;
    let plan = crossover_plan(block as u32, 1, vec![(0, signal(16, 0xf001))]);
    let detected = SimdBackend::detect();

    // Auto：backend 是本机探测值，path 按探测结果解析（有 SIMD -> optimized）。
    let auto = compile_graph(&plan, 1, block);
    let auto_dispatch = auto.kernel_dispatch();
    assert_eq!(
        auto_dispatch.backend, detected,
        "图上报的后端必须是真实探测值"
    );
    assert_eq!(auto_dispatch.choice, KernelChoice::Auto);
    assert_eq!(auto_dispatch.path, auto.kernel_path());
    assert_eq!(
        auto.kernel_path(),
        if detected.is_simd() {
            KernelPath::Optimized
        } else {
            KernelPath::Scalar
        }
    );
    assert_eq!(auto.simd_backend(), detected);
    assert_eq!(auto.kernel_choice(), KernelChoice::Auto);
    assert_eq!(auto_dispatch.backend_name(), detected.name());
    assert_eq!(auto_dispatch.path_name(), auto.kernel_path().name());
    assert_eq!(auto_dispatch.choice_name(), "auto");

    // 显式选择：Scalar 强制标量（bit-exact 口径），Optimized 强制核心优化入口。
    let scalar = compile_graph_with_kernel(&plan, 1, block, KernelChoice::Scalar);
    assert_eq!(scalar.kernel_path(), KernelPath::Scalar);
    assert_eq!(scalar.kernel_choice(), KernelChoice::Scalar);
    assert_eq!(
        scalar.simd_backend(),
        detected,
        "强制标量不改本机后端探测值"
    );
    let optimized = compile_graph_with_kernel(&plan, 1, block, KernelChoice::Optimized);
    assert_eq!(optimized.kernel_path(), KernelPath::Optimized);
    assert_eq!(optimized.kernel_choice(), KernelChoice::Optimized);
    assert_eq!(optimized.simd_backend(), detected);

    // 快照的探测入口与图内一致（status 在不编译图时也可读同一份真实值）。
    use khaen_host::graph::KernelDispatch;
    let snapshot = KernelDispatch::detect(KernelChoice::Auto);
    assert_eq!(snapshot, auto_dispatch);
    assert_eq!(snapshot.describe(), auto_dispatch.describe());
}
