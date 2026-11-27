//! T22 矩阵低速率域（48 kHz -> 3 kHz -> 48 kHz）主机验收测试。
//!
//! 覆盖：
//! 1. `decimation == interpolation == 1` 的 identity 矩阵仍逐位直通（T21 回归）；
//! 2. `decimation == interpolation == 16`（48000 / 16 = 3 kHz）单位对角矩阵 + `taps == 1`：
//!    冲激响应峰值位置与 `PlanGraph::estimated_latency_samples()` 一致（±2 样本），100 Hz
//!    通带正弦幅度保持（容差 1e-3），未激励 lane 无泄漏；
//! 3. 对角增益 0.5 / 0.25：输出幅度按矩阵系数缩放（容差 1e-3）；
//! 4. 块长无关：一次 2048 帧 vs 32 x 64 帧，容差 1e-6（实测逐位一致）；
//! 5. 零堆分配：500 次 `process` 分配 delta = 0（计数全局分配器 + thread-local 武装）；
//! 6. 任意整倍比例（8/8、16/16、32/32）通带增益 = 1，比例上限 / 变比（ASRC）/ 块长不整除的处理；
//! 7. 非法组合的编译期错误码。
//!
//! 运行：`cargo test -p khaen-host --offline --test graph_matrix_3k`。

#![allow(
    clippy::too_many_arguments,
    clippy::needless_range_loop,
    clippy::missing_const_for_thread_local
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use khaen_core::plan::{MatrixSection, Plan, RoutingSection};
use khaen_host::{AdmissionLimits, CompileError, EngineConfig, PlanGraph, Processor};

/// 全采样率：3 kHz 域 = 48000 / 16。
const SAMPLE_RATE: u32 = 48_000;

// ===========================================================================
// 计数全局分配器（零分配断言）
// ===========================================================================
//
// 与 `crates/core/tests/kernels_exact.rs` 同一约定：只有被 `MeasureGuard` 武装的当前测试
// 线程的分配才累加到全局计数；`MEASURE_LOCK` 串行化所有测量窗口，其它测试线程的分配不计入。

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAllocator;

fn note_alloc() {
    let _ = TRACKING.try_with(|enabled| {
        if enabled.get() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            note_alloc();
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(ptr, layout, new_size) };
        if !out.is_null() {
            note_alloc();
        }
        out
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let out = unsafe { System.alloc_zeroed(layout) };
        if !out.is_null() {
            note_alloc();
        }
        out
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

static MEASURE_LOCK: Mutex<()> = Mutex::new(());

struct MeasureGuard {
    start: usize,
    thread_id: std::thread::ThreadId,
    _lock: MutexGuard<'static, ()>,
}

impl MeasureGuard {
    fn begin() -> Self {
        let lock = MEASURE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let start = ALLOC_COUNT.load(Ordering::Relaxed);
        let thread_id = std::thread::current().id();
        TRACKING.with(|enabled| enabled.set(true));
        Self {
            start,
            thread_id,
            _lock: lock,
        }
    }

    fn end(self) -> usize {
        TRACKING.with(|enabled| enabled.set(false));
        assert_eq!(
            self.thread_id,
            std::thread::current().id(),
            "零分配测量窗口跨线程"
        );
        let end = ALLOC_COUNT.load(Ordering::Relaxed);
        end - self.start
    }
}

// ===========================================================================
// 方案 / 信号工具
// ===========================================================================

fn engine_config(channels: usize, block_frames: usize) -> EngineConfig {
    EngineConfig {
        block_frames,
        sample_rate: SAMPLE_RATE,
        input_channels: channels,
        output_channels: channels,
    }
}

/// 只有 matrix 段（+ 直通路由）的方案：每个 lane 一条对角低速率 FIR。
///
/// `lanes` 的第 i 项是 row i / col i 的 taps 个抽头（非对角格子全 0）。
fn matrix_only_plan(block_frames: u32, factor: u8, lanes: &[Vec<f32>], sample_rate: u32) -> Plan {
    let channels = lanes.len();
    let taps = lanes[0].len();
    assert!(lanes.iter().all(|coeffs| coeffs.len() == taps));
    let mut coeffs = vec![0.0f32; channels * channels * taps];
    for (lane, taps_of_lane) in lanes.iter().enumerate() {
        for (tap, coefficient) in taps_of_lane.iter().enumerate() {
            coeffs[(lane * channels + lane) * taps + tap] = *coefficient;
        }
    }
    Plan {
        block_size: block_frames,
        sample_rate,
        content: None,
        crossover: None,
        matrix: Some(MatrixSection {
            rows: channels as u16,
            cols: channels as u16,
            decimation: factor,
            interpolation: factor,
            taps: taps as u32,
            coeffs,
        }),
        protection: None,
        routing: Some(RoutingSection {
            dac_channels: channels as u16,
            routes: (0..channels)
                .map(|lane| (lane as u16, lane as u16))
                .collect(),
        }),
    }
}

/// 编译一份 matrix-only 方案，返回图与配置。
fn compile_matrix_only(
    block_frames: u32,
    factor: u8,
    lanes: &[Vec<f32>],
    channels: usize,
) -> PlanGraph {
    let plan = matrix_only_plan(block_frames, factor, lanes, SAMPLE_RATE);
    let config = engine_config(channels, block_frames as usize);
    PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile matrix plan")
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

/// 生成 `frames` 帧的 `channels` 通道正弦（振幅 `amplitude`）。
fn sine_input(frames: usize, channels: usize, frequency: f64, amplitude: f32) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * channels];
    for frame in 0..frames {
        let value =
            (2.0 * PI * frequency * frame as f64 / SAMPLE_RATE as f64).sin() as f32 * amplitude;
        for channel in 0..channels {
            out[frame * channels + channel] = value;
        }
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

fn channel_values(input: &[f32], channels: usize, channel: usize) -> Vec<f32> {
    input
        .iter()
        .skip(channel)
        .step_by(channels)
        .copied()
        .collect()
}

fn assert_bit_eq(actual: &[f32], expected: &[f32], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: 长度不一致");
    for (index, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a.to_bits(), e.to_bits(), "{what}: 样本 {index} {a} vs {e}");
    }
}

/// 单频最小二乘幅值：`2/N * |sum y[n] e^{-j w n}|`（对纯正弦等于其幅度）。
///
/// 求和窗口截断到整数个周期（测试频率都能整除采样率），否则非整数周期会引入约
/// `1/(N * sin(omega))` 的系统偏差（100 Hz、N=8192 时约 0.5%，会淹没 1e-3 的断言）。
fn amplitude_at(samples: &[f32], frequency: f64) -> f64 {
    let omega = 2.0 * PI * frequency / SAMPLE_RATE as f64;
    let period = SAMPLE_RATE as f64 / frequency;
    let periods = (samples.len() as f64 / period).floor();
    let count = if periods >= 1.0 {
        (periods * period).round() as usize
    } else {
        samples.len()
    };
    let mut cos_sum = 0.0f64;
    let mut sin_sum = 0.0f64;
    for (index, sample) in samples[..count].iter().enumerate() {
        let angle = omega * index as f64;
        cos_sum += f64::from(*sample) * angle.cos();
        sin_sum += f64::from(*sample) * angle.sin();
    }
    let count = count as f64;
    2.0 * (cos_sum * cos_sum + sin_sum * sin_sum).sqrt() / count
}

fn argmax_abs(samples: &[f32]) -> usize {
    let mut peak = 0usize;
    let mut peak_value = f32::MIN_POSITIVE;
    for (index, sample) in samples.iter().enumerate() {
        if sample.abs() > peak_value {
            peak_value = sample.abs();
            peak = index;
        }
    }
    peak
}

// ===========================================================================
// 1) identity（decim == interp == 1）逐位直通回归
// ===========================================================================

#[test]
fn unit_rate_identity_matrix_is_bit_exact() {
    let mut graph = compile_matrix_only(128, 1, &[vec![1.0], vec![1.0]], 2);
    for block in 0..4u32 {
        let input = signal(128 * 2, 0x51ce + block);
        let output = run(&mut graph, &input);
        assert_bit_eq(&output, &input, "identity 直通");
    }
}

// ===========================================================================
// 2) 3 kHz 域：单位冲激 + 延迟估算 + 通带正弦
// ===========================================================================

/// 实测（本机）：估算延迟 255、冲激峰值 255（相差 0）；100 Hz 幅度比 1.000514（偏差 5.1e-4）。
#[test]
fn three_khz_domain_unit_matrix_matches_latency_estimate() {
    // 48000 / 16 = 3000 Hz 低速率域；对角 1、taps = 1。
    let mut graph = compile_matrix_only(128, 16, &[vec![1.0], vec![1.0]], 2);
    let latency = graph.estimated_latency_samples();
    // 2 * ((16 * 16 - 1) / 2) = 255（P=16、F=16 时两端各 127.5 样本）。
    assert!(latency > 0, "低速率域延迟应大于 0");
    assert!(
        (latency as f64 - 255.0).abs() <= 2.0,
        "估算延迟 {latency} 应约等于 255 样本"
    );

    // 冲激响应：ch0 frame 0 冲击，峰值位置应与估算延迟一致（±2 样本）。
    let frames = 128 * 16;
    let mut input = vec![0.0f32; frames * 2];
    input[0] = 1.0;
    let output = run_in_blocks(&mut graph, &input, 2, 128);
    let lane0 = channel_values(&output, 2, 0);
    let lane1 = channel_values(&output, 2, 1);
    let peak = argmax_abs(&lane0);
    assert!(
        (peak as i64 - latency as i64).abs() <= 2,
        "冲激峰值位置 {peak} 与估算延迟 {latency} 相差超过 2 样本"
    );
    // 未被激励的 lane 不能有输出（矩阵对角、重采样状态互相独立）。
    let leakage = lane1
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()));
    assert!(leakage <= 1.0e-7, "未激励 lane 泄漏 {leakage:e}");

    // 通带：100 Hz 正弦幅度保持（容差 1e-3；带外滚降是设计行为，不在本断言内）。
    let frames = 128 * 80;
    let input = sine_input(frames, 2, 100.0, 0.5);
    let output = run_in_blocks(&mut graph, &input, 2, 128);
    let lane0 = channel_values(&output, 2, 0);
    let lane1 = channel_values(&output, 2, 1);
    let gain0 = amplitude_at(&lane0[2048..], 100.0) / 0.5;
    let gain1 = amplitude_at(&lane1[2048..], 100.0) / 0.5;
    assert!(
        (gain0 - 1.0).abs() <= 1.0e-3,
        "3 kHz 域 100 Hz 通带幅度比 {gain0} 偏离 1 超过 1e-3"
    );
    assert!(
        (gain1 - 1.0).abs() <= 1.0e-3,
        "3 kHz 域 100 Hz 通带幅度比 {gain1} 偏离 1 超过 1e-3"
    );
    println!(
        "(2) 估算延迟 = {latency}；冲激峰值 = {peak}；100 Hz 幅度比 = {gain0:.6} / {gain1:.6}"
    );
}

// ===========================================================================
// 3) 对角增益：输出幅度按矩阵系数缩放
// ===========================================================================

/// 实测（本机）：对角 0.5 / 0.25 的 100 Hz 幅度比 0.500257 / 0.250129（偏差 2.6e-4 / 1.3e-4）。
#[test]
fn three_khz_domain_diagonal_gain_scales_amplitude() {
    let mut graph = compile_matrix_only(128, 16, &[vec![0.5], vec![0.25]], 2);
    let frames = 128 * 80;
    let input = sine_input(frames, 2, 100.0, 0.5);
    let output = run_in_blocks(&mut graph, &input, 2, 128);
    let gain0 = amplitude_at(&channel_values(&output, 2, 0)[2048..], 100.0) / 0.5;
    let gain1 = amplitude_at(&channel_values(&output, 2, 1)[2048..], 100.0) / 0.5;
    assert!(
        (gain0 - 0.5).abs() <= 1.0e-3,
        "lane 0 增益 {gain0} 应为 0.5（±1e-3）"
    );
    assert!(
        (gain1 - 0.25).abs() <= 1.0e-3,
        "lane 1 增益 {gain1} 应为 0.25（±1e-3）"
    );
    println!("(3) 对角 0.5 / 0.25 的实测幅度比 = {gain0:.6} / {gain1:.6}");
}

// ===========================================================================
// 4) 块长无关（一次 2048 帧 vs 32 x 64 帧）
// ===========================================================================

/// 实测（本机）：一次 2048 帧 vs 32 x 64 帧最大误差 0（逐位一致）。
#[test]
fn three_khz_domain_is_independent_of_block_length() {
    // 3 抽头对角 FIR，跨块历史必须正确。
    let mut whole =
        compile_matrix_only(256, 16, &[vec![1.0, 0.5, 0.25], vec![0.5, -0.25, 0.125]], 2);
    let input = signal(2048 * 2, 0xbadc0de);
    let whole_output = run(&mut whole, &input);

    let mut split =
        compile_matrix_only(256, 16, &[vec![1.0, 0.5, 0.25], vec![0.5, -0.25, 0.125]], 2);
    let split_output = run_in_blocks(&mut split, &input, 2, 64);

    let mut max_error = 0.0f32;
    for (index, (a, b)) in whole_output.iter().zip(split_output.iter()).enumerate() {
        let error = (a - b).abs();
        max_error = max_error.max(error);
        assert!(error <= 1.0e-6, "块长无关性样本 {index}: {a} vs {b}");
    }
    assert!(
        whole_output.iter().any(|sample| *sample != 0.0),
        "输出不能全是 0"
    );
    println!("(4) 一次 2048 帧 vs 32 x 64 帧最大误差 = {max_error:e}（阈值 1e-6）");
}

// ===========================================================================
// 5) process 零堆分配
// ===========================================================================

#[test]
fn three_khz_domain_process_performs_no_heap_allocation() {
    let mut graph = compile_matrix_only(128, 16, &[vec![1.0], vec![1.0]], 2);
    let mut input = vec![0.0f32; 128 * 2];
    let mut output = vec![0.0f32; 128 * 2];
    for (index, sample) in input.iter_mut().enumerate() {
        *sample = (index as f32) * 1.0e-3 - 0.25;
    }
    // 预热：首次触碰（含分配器统计用的 TLS）在测量窗口之外。
    graph.process(&input, &mut output);

    let guard = MeasureGuard::begin();
    for round in 0..500usize {
        for (index, sample) in input.iter_mut().enumerate() {
            *sample = (((round * 31 + index) % 97) as f32) * 1.0e-2 - 0.5;
        }
        graph.process(&input, &mut output);
    }
    let allocated = guard.end();
    assert_eq!(allocated, 0, "低速率域 graph process 分配了 {allocated} 次");
    assert!(
        output.iter().any(|sample| *sample != 0.0),
        "低速率域链输出不应全为 0"
    );

    // 控制组：证明计数窗口确实能捕获分配。
    let control = {
        let guard = MeasureGuard::begin();
        std::hint::black_box(vec![0u8; 64]);
        guard.end()
    };
    assert!(control >= 1, "计数分配器未捕获控制组分配（零分配断言失效）");
}

// ===========================================================================
// 6) 任意整倍比例：8/8、16/16、32/32 通带增益 = 1，且延迟可估算
// ===========================================================================

/// 实测（本机）：F=8 / 16 / 32 的估算延迟 127 / 255 / 511，冲激峰值与之一致（相差 0），
/// 100 Hz 幅度比 1.000157 / 1.000514 / 1.001069（偏差随 F 增大，见下方断言注释）。
#[test]
fn arbitrary_equal_rate_factors_stay_unity_in_the_passband() {
    for factor in [8u8, 16, 32] {
        let mut graph = compile_matrix_only(128, factor, &[vec![1.0]], 1);
        let latency = graph.estimated_latency_samples();
        // 两端各 (P * factor - 1) / 2 样本（P = 16）。
        let expected = (16.0 * f64::from(factor)) - 1.0;
        assert!(
            (latency as f64 - expected).abs() <= 2.0,
            "因子 {factor} 的估算延迟 {latency} 应约等于 {expected}"
        );

        let frames = 128 * 80;
        let input = sine_input(frames, 1, 100.0, 0.5);
        let output = run_in_blocks(&mut graph, &input, 1, 128);
        let lane = channel_values(&output, 1, 0);
        let gain = amplitude_at(&lane[2048..], 100.0) / 0.5;
        // 实测通带幅度比偏差（本机）：F=8 -> 1.6e-4、F=16 -> 5.1e-4、F=32 -> 1.1e-3；
        // 偏差随因子增大来自每相归一对相邻相位微小增益差的放大（0.001 dB 量级）。
        // 这里是“任意整倍因子”的补充测试，用 2e-3；任务要求的 16/16 用例在 1e-3 内（见测试 2）。
        assert!(
            (gain - 1.0).abs() <= 2.0e-3,
            "因子 {factor} 的 100 Hz 幅度比 {gain} 偏离 1 超过 2e-3"
        );

        // 冲激峰值与估算延迟一致（±2 样本）。
        let mut impulse = vec![0.0f32; frames];
        impulse[0] = 1.0;
        let mut graph = compile_matrix_only(128, factor, &[vec![1.0]], 1);
        let output = run_in_blocks(&mut graph, &impulse, 1, 128);
        let peak = argmax_abs(&output);
        assert!(
            (peak as i64 - latency as i64).abs() <= 2,
            "因子 {factor} 的冲激峰值 {peak} 与估算延迟 {latency} 相差超过 2"
        );
        println!("(6) 因子 {factor}: 延迟 {latency}，峰值 {peak}，100 Hz 幅度比 {gain:.6}");
    }
}

// ===========================================================================
// 7) 非法组合的编译期错误码
// ===========================================================================

#[test]
fn invalid_rate_combinations_are_rejected_at_compile_time() {
    // 变比（ASRC）：8/32 在固定速率图里无法表示。
    let mut plan = matrix_only_plan(128, 16, &[vec![1.0]], SAMPLE_RATE);
    if let Some(matrix) = plan.matrix.as_mut() {
        matrix.decimation = 8;
        matrix.interpolation = 32;
    }
    let config = engine_config(1, 128);
    let error = PlanGraph::compile(&plan, &config, &AdmissionLimits::default())
        .expect_err("8/32 变比必须被拒绝");
    assert_eq!(
        error,
        CompileError::UnsupportedRates {
            decimation: 8,
            interpolation: 32,
        }
    );

    // 超过因子上限（64）。
    let mut plan = matrix_only_plan(128, 16, &[vec![1.0]], SAMPLE_RATE);
    if let Some(matrix) = plan.matrix.as_mut() {
        matrix.decimation = 65;
        matrix.interpolation = 65;
    }
    let error = PlanGraph::compile(&plan, &config, &AdmissionLimits::default())
        .expect_err("因子超过上限必须被拒绝");
    assert_eq!(
        error,
        CompileError::UnsupportedRates {
            decimation: 65,
            interpolation: 65,
        }
    );

    // 块长不能被因子整除：128 % 24 != 0。
    let mut plan = matrix_only_plan(128, 16, &[vec![1.0]], SAMPLE_RATE);
    if let Some(matrix) = plan.matrix.as_mut() {
        matrix.decimation = 24;
        matrix.interpolation = 24;
    }
    let error = PlanGraph::compile(&plan, &config, &AdmissionLimits::default())
        .expect_err("block 128 不能被因子 24 整除，必须被拒绝");
    assert!(
        matches!(error, CompileError::InvalidSection(_)),
        "期望 InvalidSection，实际 {error:?}"
    );

    // 0 因子：准入先拒绝（matrix_rates），不进 matrix 段编译。
    let mut plan = matrix_only_plan(128, 16, &[vec![1.0]], SAMPLE_RATE);
    if let Some(matrix) = plan.matrix.as_mut() {
        matrix.decimation = 0;
        matrix.interpolation = 0;
    }
    let error = PlanGraph::compile(&plan, &config, &AdmissionLimits::default())
        .expect_err("0 因子必须被拒绝");
    match error {
        CompileError::AdmissionRejected(admission) => assert!(
            admission
                .rejections
                .iter()
                .any(|rejection| rejection.code == "matrix_rates"),
            "期望 matrix_rates 拒绝，实际 {:?}",
            admission.rejections
        ),
        other => panic!("期望 AdmissionRejected，实际 {other:?}"),
    }
}
