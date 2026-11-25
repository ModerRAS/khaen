//! T22 有理数多相重采样内核测试（`crates/core/tests/resample.rs`）。
//!
//! 覆盖：
//! (a) 直流增益 = 1（多组 `(decimation, interpolation)` 组合，容差 1e-3）；
//! (b) 48 kHz -> 3 kHz -> 48 kHz 级联的通带幅度误差 < 0.5 dB（单频最小二乘幅值，无第三方依赖），
//!     以及阻带抑制（5 kHz 输入应被滤除）；
//! (c) 与定义式朴素参考逐样本一致（多组有理数比例）；
//! (d) 群延迟与冲激响应峰值位置一致（±1 / ±2 输出样本）；
//! (e) 块长无关（同一输入一次整段 vs 拆块，逐位一致）与 reset 复现；
//! (f) 构造期确定性（同参数两次构造，系数与输出逐位一致）；
//! (g) 非法构造参数返回错误码、输出缓冲不足时不推进状态；
//! (h) `process` 零堆分配（计数全局分配器；thread-local 武装 + Mutex 串行，与
//!     `kernels_exact.rs` 同一约定：本二进制内的测量窗口互斥，其它测试线程的分配不计入）。
//!
//! 运行：`cargo test -p khaen-core --offline --test resample`。

// clippy 1.99 的 missing_const_for_thread_local 对已写 const 块初始化的 thread_local 也误报
// （见下方 TRACKING）；保留 const 初始化、整体 allow 该 lint，避免退回惰性初始化后在
// 分配器首次调用时递归分配。
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

use khaen_core::kernels::resample::{
    RationalResampler, ResamplerError, MAX_RATE_FACTOR, MAX_TAPS_PER_PHASE, MIN_TAPS_PER_PHASE,
};

/// 采样率：3 kHz 域由 48000 / 16 得到（T22 的目标域）。
const SAMPLE_RATE: f64 = 48_000.0;
/// 测试统一使用的每相抽头数。
const TAPS_PER_PHASE: usize = 16;
/// 测试统一使用的截止比例（占低侧 Nyquist）。
const CUTOFF_NORM: f64 = 0.9;

// ===========================================================================
// 计数全局分配器（零分配断言）
// ===========================================================================
//
// 只有被 `MeasureGuard` 武装的当前测试线程的分配才累加到全局计数；`MEASURE_LOCK` 用
// Mutex 串行化所有测量窗口。libtest 默认多线程跑测试，其它测试线程的分配因 TLS 未武装而不计数。
// 分配器里用 `try_with` 访问 TLS，TLS 销毁期安全忽略；`Cell<bool>` const 初始化不分配，
// 避免“统计分配器自己分配”的递归。

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

/// 测量窗口的串行锁（测试默认并行，窗口必须互斥才能用全局计数）。
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

struct MeasureGuard {
    start: usize,
    thread_id: std::thread::ThreadId,
    /// 持有到测量结束，保证同时只有一个窗口。
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
// 工具
// ===========================================================================

/// 一组测试用比例：(decimation, interpolation)。
const RATE_PAIRS: [(u32, u32); 7] = [
    (16, 1), // 48k -> 3k
    (1, 16), // 3k -> 48k
    (8, 8),  // 48k -> 6k（速率不变）
    (8, 32),
    (32, 8),
    (3, 2),
    (2, 3),
];

fn sampler(decimation: u32, interpolation: u32) -> RationalResampler {
    RationalResampler::new(decimation, interpolation, TAPS_PER_PHASE, CUTOFF_NORM)
        .expect("测试配置必须合法")
}

/// 按 `block` 大小反复喂入，返回完整输出；顺带断言 `output_len_for` 与 `process` 一致。
fn resample_all(resampler: &mut RationalResampler, input: &[f32], block: usize) -> Vec<f32> {
    let mut output = Vec::with_capacity(resampler.output_len_for(input.len()));
    for chunk in input.chunks(block) {
        let expected = resampler.output_len_for(chunk.len());
        let mut block_out = vec![0.0f32; expected];
        let written = resampler.process(chunk, &mut block_out);
        assert_eq!(written, expected, "process 写出数与 output_len_for 不一致");
        output.extend_from_slice(&block_out);
    }
    output
}

/// 定义式朴素参考：`y[m] = sum_p h[phase][p] * x[base - p]`（`base = floor(m * D / I)`）。
fn naive_reference(resampler: &RationalResampler, input: &[f32]) -> Vec<f32> {
    let decimation = resampler.decimation() as u128;
    let interpolation = resampler.interpolation() as u128;
    let arm_taps = resampler.arm_taps();
    let coefficients = resampler.coefficients();
    let mut output = vec![0.0f32; resampler.output_len_for(input.len())];
    for (index, slot) in output.iter_mut().enumerate() {
        let position = index as u128;
        let base = (position * decimation / interpolation) as i64;
        let phase = (position % interpolation) as usize;
        let mut accumulator = 0.0f32;
        for tap in 0..arm_taps {
            let source = base - tap as i64;
            if source >= 0 {
                let sample = input.get(source as usize).copied().unwrap_or(0.0);
                accumulator += coefficients[phase * arm_taps + tap] * sample;
            }
        }
        *slot = accumulator;
    }
    output
}

/// 单频最小二乘幅值：`2/N * |sum y[n] e^{-j w n}|`（对纯正弦等于其幅度）。
///
/// 求和窗口截断到整数个周期（测试频率都能整除采样率），否则非整数周期会引入约
/// `1/(N * sin(omega))` 的系统偏差（100 Hz、N=8192 时约 0.5%，会淹没 1e-3 的断言）。
fn amplitude_at(samples: &[f32], frequency: f64) -> f64 {
    let omega = 2.0 * PI * frequency / SAMPLE_RATE;
    let period = SAMPLE_RATE / frequency;
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

/// 绝对幅度峰值的位置。
fn argmax_abs(samples: &[f32]) -> usize {
    let mut peak = 0usize;
    let mut peak_value = f32::MIN_POSITIVE;
    for (index, sample) in samples.iter().enumerate() {
        let magnitude = sample.abs();
        if magnitude > peak_value {
            peak_value = magnitude;
            peak = index;
        }
    }
    peak
}

/// 确定性测试信号：值域约 (-1, 1)，只用整数运算，跨平台一致。
fn test_signal(len: usize) -> Vec<f32> {
    (0..len)
        .map(|index| (((index * 37 + 11) % 13) as f32 - 6.0) / 8.0)
        .collect()
}

// ===========================================================================
// (a) 直流增益 = 1
// ===========================================================================

/// 实测（本机，f32 生产路径）：DC 增益最大偏差 8.3e-7（阈值 1e-3）。
#[test]
fn dc_gain_is_unity_for_rational_rate_pairs() {
    const INPUT_LEN: usize = 8192;
    let mut worst = 0.0f64;
    for (decimation, interpolation) in RATE_PAIRS {
        let mut resampler = sampler(decimation, interpolation);
        let input = vec![1.0f32; INPUT_LEN];
        let output = resample_all(&mut resampler, &input, 128);
        assert!(!output.is_empty());
        // 跳过滤波器暂态（前一半），只看稳态。
        for (index, sample) in output[output.len() / 2..].iter().enumerate() {
            let deviation = (f64::from(*sample) - 1.0).abs();
            worst = worst.max(deviation);
            assert!(
                deviation <= 1.0e-3,
                "DC 增益偏差 {deviation:e}（{decimation}/{interpolation}，稳态样本 {index}）"
            );
        }
    }
    println!("(a) DC 增益最大偏差 = {worst:e}（阈值 1e-3）");
}

// ===========================================================================
// (b) 48k -> 3k -> 48k 通带 / 阻带
// ===========================================================================

/// 实测（本机）：48k -> 3k -> 48k 通带最大幅度误差 0.098 dB（1000 Hz 处；20~750 Hz 更小），
/// 5 kHz 阻带抑制 -175 dB（抽取器截止 1350 Hz，5 kHz 折回 3 kHz 域的 Nyquist 之外后基本被清除）。
#[test]
fn passband_response_of_3k_domain_is_flat() {
    const INPUT_LEN: usize = 8192;
    // 级联延迟约 255 样本（P=16、F=16：两端各 (L-1)/2 = 127.5），丢 1024 样本足够。
    const DISCARD: usize = 1024;
    let mut worst_db = 0.0f64;
    for frequency in [20.0, 50.0, 100.0, 200.0, 300.0, 500.0, 750.0, 1000.0] {
        let mut decimator = sampler(16, 1);
        let mut interpolator = sampler(1, 16);
        let input: Vec<f32> = (0..INPUT_LEN)
            .map(|index| (2.0 * PI * frequency * index as f64 / SAMPLE_RATE).sin() as f32)
            .collect();
        let low = resample_all(&mut decimator, &input, 128);
        let output = resample_all(&mut interpolator, &low, 32);
        assert_eq!(output.len(), INPUT_LEN, "级联输出长度应保持不变");
        let gain = amplitude_at(&output[DISCARD..], frequency);
        let gain_db = 20.0 * gain.log10();
        worst_db = worst_db.max(gain_db.abs());
        assert!(
            gain_db.abs() < 0.5,
            "{frequency} Hz 通带幅度误差 {gain_db:.4} dB 超过 0.5 dB"
        );
    }
    // 阻带：5 kHz 远高于 3 kHz 域的 Nyquist（1.5 kHz），抽取器应把它滤掉。
    let mut decimator = sampler(16, 1);
    let mut interpolator = sampler(1, 16);
    let input: Vec<f32> = (0..INPUT_LEN)
        .map(|index| (2.0 * PI * 5000.0 * index as f64 / SAMPLE_RATE).sin() as f32)
        .collect();
    let low = resample_all(&mut decimator, &input, 128);
    let output = resample_all(&mut interpolator, &low, 32);
    let stopband = amplitude_at(&output[DISCARD..], 5000.0);
    let stopband_db = 20.0 * stopband.max(1.0e-12).log10();
    assert!(
        stopband_db < -20.0,
        "5 kHz 阻带抑制只有 {stopband_db:.2} dB（应 < -20 dB）"
    );
    println!(
        "(b) 48k->3k->48k 通带最大幅度误差 = {worst_db:.4} dB（阈值 0.5 dB）；5 kHz 阻带 = {stopband_db:.2} dB"
    );
}

// ===========================================================================
// (c) 与定义式朴素参考逐样本一致
// ===========================================================================

/// 实测（本机）：与定义式朴素参考逐位一致（最大绝对误差 0）。
#[test]
fn matches_naive_definition() {
    let mut worst = 0.0f32;
    for (decimation, interpolation) in [
        (2u32, 1u32),
        (1, 2),
        (3, 2),
        (2, 3),
        (8, 8),
        (8, 32),
        (32, 8),
        (5, 3),
        (3, 5),
    ] {
        let mut resampler = sampler(decimation, interpolation);
        let input = test_signal(256);
        let expected = naive_reference(&resampler, &input);
        let output = resample_all(&mut resampler, &input, 64);
        assert_eq!(output.len(), expected.len());
        for (index, (actual, reference)) in output.iter().zip(expected.iter()).enumerate() {
            let error = (actual - reference).abs();
            worst = worst.max(error);
            assert!(
                error <= 1.0e-7,
                "{decimation}/{interpolation} 样本 {index}: 实际 {actual} vs 定义 {reference}"
            );
        }
    }
    println!("(c) 与定义式朴素参考最大绝对误差 = {worst:e}（0 表示逐位一致）");
}

// ===========================================================================
// (d) 群延迟与冲激响应峰值
// ===========================================================================

/// 实测（本机，P=16、F=16）：抽取器峰值 8（估算 7.96875）、插值器峰值 127（估算 127.5）、
/// 级联峰值 255（估算 255 = 127.5 + 127.5）。
#[test]
fn group_delay_matches_impulse_peak() {
    // 抽取器：冲激响应为 h[m * D]，峰值在 group_delay_output_samples()（输出样本）。
    let mut decimator = sampler(16, 1);
    let mut input = vec![0.0f32; 1024];
    input[0] = 1.0;
    let low = resample_all(&mut decimator, &input, 64);
    let decimator_peak = argmax_abs(&low) as f64;
    let decimator_expected = decimator.group_delay_output_samples();
    assert!(
        (decimator_peak - decimator_expected).abs() <= 1.0,
        "抽取器峰值 {decimator_peak} vs 估算群延迟 {decimator_expected}"
    );

    // 插值器：冲激响应即原型滤波器 h[m]，峰值在 group_delay_output_samples()。
    let mut interpolator = sampler(1, 16);
    let mut low_impulse = vec![0.0f32; 64];
    low_impulse[0] = 1.0;
    let output = resample_all(&mut interpolator, &low_impulse, 8);
    let interpolator_peak = argmax_abs(&output) as f64;
    let interpolator_expected = interpolator.group_delay_output_samples();
    assert!(
        (interpolator_peak - interpolator_expected).abs() <= 1.0,
        "插值器峰值 {interpolator_peak} vs 估算群延迟 {interpolator_expected}"
    );

    // 级联 48k -> 3k -> 48k：总延迟 = 抽取器输入侧群延迟 + 插值器输出侧群延迟（均为全速率样本）。
    let mut decimator = sampler(16, 1);
    let mut interpolator = sampler(1, 16);
    let mut impulse = vec![0.0f32; 2048];
    impulse[0] = 1.0;
    let low = resample_all(&mut decimator, &impulse, 128);
    let output = resample_all(&mut interpolator, &low, 32);
    let cascade_peak = argmax_abs(&output) as f64;
    let cascade_expected =
        decimator.group_delay_input_samples() + interpolator.group_delay_output_samples();
    assert!(
        (cascade_peak - cascade_expected).abs() <= 2.0,
        "级联峰值 {cascade_peak} vs 估算延迟 {cascade_expected}"
    );
    println!(
        "(d) 抽取器峰值 {decimator_peak}（估算 {decimator_expected}）；插值器峰值 {interpolator_peak}（估算 {interpolator_expected}）；级联峰值 {cascade_peak}（估算 {cascade_expected}）"
    );
}

// ===========================================================================
// (e) 块长无关 / reset 复现
// ===========================================================================

/// 实测（本机）：整段 vs 8 x 64 帧逐位一致（0 误差）。
#[test]
fn output_is_independent_of_block_length() {
    let input = test_signal(512);
    for (decimation, interpolation) in [(16u32, 1u32), (1, 16), (8, 8)] {
        let mut whole = sampler(decimation, interpolation);
        let whole_output = resample_all(&mut whole, &input, input.len());
        let mut split = sampler(decimation, interpolation);
        let split_output = resample_all(&mut split, &input, 64);
        assert_eq!(whole_output.len(), split_output.len());
        for (index, (a, b)) in whole_output.iter().zip(split_output.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "{decimation}/{interpolation} 块长无关性样本 {index}: {a} vs {b}"
            );
        }
    }
    println!("(e) 块长无关：整段 vs 8 x 64 逐位一致");
}

#[test]
fn reset_reproduces_output() {
    let input = test_signal(384);
    let mut resampler = sampler(16, 8);
    let first = resample_all(&mut resampler, &input, 64);
    resampler.reset();
    let second = resample_all(&mut resampler, &input, 64);
    assert_eq!(first.len(), second.len());
    for (index, (a, b)) in first.iter().zip(second.iter()).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "reset 后样本 {index} 不一致");
    }
}

// ===========================================================================
// (f) 构造期确定性
// ===========================================================================

#[test]
fn construction_is_deterministic() {
    let first = sampler(16, 1);
    let second = sampler(16, 1);
    assert_eq!(first.coefficients().len(), second.coefficients().len());
    let mut worst = 0.0f32;
    for (a, b) in first.coefficients().iter().zip(second.coefficients()) {
        worst = worst.max((a - b).abs());
        assert_eq!(a.to_bits(), b.to_bits(), "同参数构造的系数不是逐位一致");
    }
    // 每相之和 = 1（相位归一化）的直接校验。
    let arm_taps = first.arm_taps();
    for phase in 0..first.interpolation() as usize {
        let sum: f32 = first.coefficients()[phase * arm_taps..(phase + 1) * arm_taps]
            .iter()
            .sum();
        assert!(
            (sum - 1.0).abs() <= 1.0e-5,
            "相位 {phase} 系数和 {sum} 应为 1"
        );
    }
    println!("(f) 同参数两次构造系数逐位一致（最大差 {worst:e}）");
}

// ===========================================================================
// (g) 非法参数 / 输出不足
// ===========================================================================

#[test]
fn invalid_configuration_is_rejected() {
    assert_eq!(
        RationalResampler::new(0, 1, TAPS_PER_PHASE, CUTOFF_NORM).unwrap_err(),
        ResamplerError::ZeroRate
    );
    assert_eq!(
        RationalResampler::new(1, 0, TAPS_PER_PHASE, CUTOFF_NORM).unwrap_err(),
        ResamplerError::ZeroRate
    );
    assert_eq!(
        RationalResampler::new(MAX_RATE_FACTOR + 1, 1, TAPS_PER_PHASE, CUTOFF_NORM).unwrap_err(),
        ResamplerError::RateTooLarge
    );
    assert_eq!(
        RationalResampler::new(1, MAX_RATE_FACTOR + 1, TAPS_PER_PHASE, CUTOFF_NORM).unwrap_err(),
        ResamplerError::RateTooLarge
    );
    assert_eq!(
        RationalResampler::new(16, 1, MIN_TAPS_PER_PHASE - 1, CUTOFF_NORM).unwrap_err(),
        ResamplerError::TapsOutOfRange
    );
    assert_eq!(
        RationalResampler::new(16, 1, MAX_TAPS_PER_PHASE + 1, CUTOFF_NORM).unwrap_err(),
        ResamplerError::TapsOutOfRange
    );
    assert_eq!(
        RationalResampler::new(16, 1, TAPS_PER_PHASE, 0.0).unwrap_err(),
        ResamplerError::InvalidCutoff
    );
    assert_eq!(
        RationalResampler::new(16, 1, TAPS_PER_PHASE, 1.5).unwrap_err(),
        ResamplerError::InvalidCutoff
    );
    assert_eq!(
        RationalResampler::new(16, 1, TAPS_PER_PHASE, f64::NAN).unwrap_err(),
        ResamplerError::InvalidCutoff
    );
}

#[test]
fn short_output_is_rejected_without_state_change() {
    let mut resampler = sampler(16, 1);
    let input = vec![1.0f32; 16];
    let expected = resampler.output_len_for(input.len());
    assert_eq!(expected, 1);
    let mut too_short = vec![0.0f32; expected - 1];
    assert_eq!(resampler.process(&input, &mut too_short), 0);
    // 失败调用不推进状态：随后的正常调用仍应写出 expected 个样本。
    let mut output = vec![0.0f32; expected];
    assert_eq!(resampler.process(&input, &mut output), expected);
}

// ===========================================================================
// (h) process 零堆分配
// ===========================================================================

#[test]
fn process_performs_no_heap_allocation() {
    let mut decimator = sampler(16, 1);
    let mut interpolator = sampler(1, 16);
    let block = 128usize;
    let mut input = test_signal(block);
    let mut low = vec![0.0f32; decimator.output_len_for(block)];
    let mut output = vec![0.0f32; interpolator.output_len_for(low.len())];

    // 预热：首次触碰（TLS/分支预测/页错误）在测量窗口之外。
    let _ = decimator.process(&input, &mut low);
    let _ = interpolator.process(&low, &mut output);

    let guard = MeasureGuard::begin();
    for round in 0..500usize {
        for (index, sample) in input.iter_mut().enumerate() {
            *sample = (((round * 31 + index) % 97) as f32) * 1.0e-2 - 0.5;
        }
        let low_written = decimator.process(&input, &mut low);
        let output_written = interpolator.process(&low, &mut output);
        // 抑制优化，同时不在成功路径上分配。
        std::hint::black_box((low_written, output_written));
    }
    let allocated = guard.end();
    assert_eq!(
        allocated, 0,
        "resampler process 路径发生 {allocated} 次堆分配"
    );

    // 控制组：证明计数窗口确实能捕获分配。
    let control = {
        let guard = MeasureGuard::begin();
        std::hint::black_box(vec![0u8; 64]);
        guard.end()
    };
    assert!(control >= 1, "计数分配器未捕获控制组分配（零分配断言失效）");
}
