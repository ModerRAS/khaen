//! MIMO 12 kHz 低频修正支路私有测试（task-253；写域 crates/core/tests/mimo12k/）。
//!
//! 覆盖：
//! - 设计产物回归：12k 域 v2 G（4096 taps, D12=120, 5×5）经抽取→G12→插值后与 Python f64
//!   参考逐样本对拍（误差界 rel L2）；
//! - 抽取/插值保真：通带 20-300 Hz 增益、11.7k/23.5k 镜像/混叠抑制；
//! - 延迟对齐：单位冲激的修正支路峰值 = 输入位置 + delay48（0 样本误差），前回声 ≤ -50 dB；
//! - 分区卷积复用 T08：B48=64（B12=16）与 B48=128（B12=32）跨块长一致 ≤ 1e-5；
//! - RT 路径零分配；成本模型 12k 支路 vs 48k 全速融合路径；release 下实测 fake 吞吐。
//!
//! 运行：cargo test -p khaen-core --test mimo12k --release -- --nocapture

#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};

use khaen_core::mimo12k::{Mimo12kLayout, Mimo12kRuntime, MIMO12K_GROUP, MIMO12K_LOWRATE_HZ};
const M: usize = 5;
const TAPS: usize = 4096;
const D12: usize = 120;
const RESAMP_TAPS: usize = 95;
const D1: usize = (RESAMP_TAPS - 1) / 2;
const DSYS: u32 = (D1 * 2 + MIMO12K_GROUP * D12) as u32;
const BLOCK48: usize = 128;
const STIM_SAMPLES: usize = 16384;
const N_BLOCKS: usize = STIM_SAMPLES / BLOCK48;

// ---------------------------------------------------------------------------
// 零分配计数分配器（只看当前测试线程；T08 同口径）
// ---------------------------------------------------------------------------
thread_local! {
    static ALLOC_COUNT: Cell<u64> = const { Cell::new(0) };
}
struct CountingAllocator;
fn note_alloc() {
    let _ = ALLOC_COUNT.try_with(|cell| cell.set(cell.get() + 1));
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            note_alloc();
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = System.realloc(ptr, layout, new_size);
        if !out.is_null() {
            note_alloc();
        }
        out
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let out = System.alloc_zeroed(layout);
        if !out.is_null() {
            note_alloc();
        }
        out
    }
}
#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn alloc_count() -> u64 {
    ALLOC_COUNT.with(|cell| cell.get())
}
fn alloc_warmup() {
    let _ = alloc_count();
    let _ = alloc_count();
}

fn artifact_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evidence/mimo-12k-v2/artifacts")
}

fn read_f32(path: &Path, expected: usize) -> Vec<f32> {
    let bytes =
        std::fs::read(path).unwrap_or_else(|err| panic!("读取产物失败 {}: {err}", path.display()));
    assert_eq!(
        bytes.len(),
        expected * 4,
        "{} 大小不符：{} B != {} B",
        path.display(),
        bytes.len(),
        expected * 4
    );
    let mut out = Vec::with_capacity(expected);
    for chunk in bytes.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    out
}

struct Artifact {
    g: Vec<f32>,
    decim: Vec<f32>,
    interp: Vec<f32>,
    stimulus: Vec<f32>,
    expected_corr: Vec<f32>,
}

fn load_artifact() -> Artifact {
    let dir = artifact_dir();
    let params = std::fs::read_to_string(dir.join("params.json"))
        .unwrap_or_else(|err| panic!("缺少 params.json: {err}（先跑 design_12k.py）"));
    // 轻量 provenance 断言（不引入 JSON 依赖）。
    for needle in [
        "\"taps\": 4096",
        "\"d12\": 120",
        "\"resamp_taps\": 95",
        "\"dsys_samples\": 574",
        "\"lowrate_hz\": 12000",
        "\"g_layout\"",
    ] {
        assert!(
            params.contains(needle),
            "params.json 缺少/不匹配 {needle}（产物与测试任何一方过期）"
        );
    }
    let g = read_f32(&dir.join("g12_v2_4096.f32"), M * M * TAPS);
    let decim = read_f32(&dir.join("resamp95.f32"), RESAMP_TAPS);
    let interp = read_f32(&dir.join("resamp95.f32"), RESAMP_TAPS)
        .into_iter()
        .map(|v| v * MIMO12K_GROUP as f32)
        .collect();
    let stimulus = read_f32(&dir.join("stimulus_48k.f32"), M * STIM_SAMPLES);
    let expected_corr = read_f32(&dir.join("expected_corr_48k.f32"), M * STIM_SAMPLES);
    Artifact {
        g,
        decim,
        interp,
        stimulus,
        expected_corr,
    }
}

fn build_runtime(art: &Artifact, block48: usize) -> Mimo12kRuntime {
    Mimo12kRuntime::build_from_design(
        M,
        TAPS,
        block48,
        RESAMP_TAPS,
        D12,
        &art.g,
        &art.decim,
        &art.interp,
    )
    .expect("Mimo12kRuntime::build_from_design")
}

/// 流式跑完整段 stim（5ch planar, stride=STIM_SAMPLES），返回修正支路输出（5ch planar）。
fn run_stream(rt: &mut Mimo12kRuntime, stim: &[f32], samples: usize) -> Vec<f32> {
    let block = rt.layout().block48 as usize;
    assert_eq!(samples % block, 0, "测试样本数必须是块长整数倍");
    let nblocks = samples / block;
    let mut out = vec![0.0f32; M * samples];
    let mut corr = vec![0.0f32; M * block];
    for b in 0..nblocks {
        let feeds = &stim[b * block..];
        let status = rt.process_block_f32(feeds, samples, &mut corr, block, block);
        assert!(status.is_ok(), "process_block 返回 {}", status.code());
        for ch in 0..M {
            out[ch * samples + b * block..ch * samples + (b + 1) * block]
                .copy_from_slice(&corr[ch * block..(ch + 1) * block]);
        }
    }
    out
}

fn rel_l2(got: &[f32], want: &[f32]) -> f64 {
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for (a, b) in got.iter().zip(want.iter()) {
        let d = *a as f64 - *b as f64;
        num += d * d;
        den += (*b as f64) * (*b as f64);
    }
    (num / den.max(1e-300)).sqrt()
}

fn max_abs(x: &[f32]) -> f64 {
    x.iter().fold(0.0f64, |acc, v| acc.max((*v as f64).abs()))
}

// ---------------------------------------------------------------------------
// 1) 设计产物回归：Rust（f32 UPOLS）vs Python（f64 直接卷积）参考
// ---------------------------------------------------------------------------
#[ignore = "requires the MIMO artifact directory produced by the design tooling; not shipped in this repository"]
#[test]
fn artifact_kernel_matches_python_reference() {
    let art = load_artifact();
    let mut rt = build_runtime(&art, BLOCK48);
    assert_eq!(rt.delay48(), DSYS, "layout Δsys 与产物不一致");
    assert_eq!(rt.layout().predelay_low, D12 as u16);
    assert_eq!(rt.layout().taps, TAPS as u32);
    assert_eq!(
        rt.layout().partitions,
        (TAPS / (BLOCK48 / MIMO12K_GROUP)) as u16
    );
    let got = run_stream(&mut rt, &art.stimulus, STIM_SAMPLES);
    let rel = rel_l2(&got, &art.expected_corr);
    let peak = max_abs(&art.expected_corr);
    let max_err = max_abs(
        &got.iter()
            .zip(art.expected_corr.iter())
            .map(|(a, b)| a - b)
            .collect::<Vec<f32>>(),
    );
    println!(
        "artifact kernel: rel_L2={rel:.3e} max_abs={max_err:.3e} peak={peak:.3e} \
         delay48={} ms={:.3}",
        rt.delay48(),
        rt.layout().delay_ms()
    );
    // 判据：f32 FFT 卷积 + f32 系数舍入相对 f64 直接卷积；冻结 2e-5（T08 f32 工程门 1e-5，
    // 本链路多两级抽取/插值 FIR，按同量级放宽到 2e-5），max_abs 用 1e-5*max(1,peak)。
    assert!(
        rel <= 2e-5,
        "修正支路 vs Python f64 参考 rel L2 {rel:.3e} > 2e-5"
    );
    assert!(
        max_err <= 1e-5 * peak.max(1.0),
        "修正支路 max_abs {max_err:.3e} 超过 1e-5*max(1,peak)"
    );
}

// ---------------------------------------------------------------------------
// 2) 抽取/插值保真：通带 + 混叠/镜像抑制
// ---------------------------------------------------------------------------
#[ignore = "requires the MIMO artifact directory produced by the design tooling; not shipped in this repository"]
#[test]
fn resampler_passband_and_alias_image_gates() {
    let art = load_artifact();
    let rt = build_runtime(&art, BLOCK48);
    let mut gains = Vec::new();
    for f in [
        20.0f64, 40.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 300.0,
    ] {
        let g = rt.resampler_gain_db(f, true);
        gains.push((f, g));
        assert!(
            (-0.8..=0.2).contains(&g),
            "抽取+插值通带 {f} Hz 增益 {g:.3} dB 超出 [-0.8, +0.2] dB"
        );
    }
    // 12 kHz 抽取/插值后落入 20-300 Hz 的混叠/镜像来源：11.7-12 kHz / 23.5-24 kHz。
    for f in [11_700.0f64, 11_900.0, 12_100.0, 23_500.0, 23_900.0] {
        let d = rt.resampler_gain_db(f, false);
        assert!(d <= -60.0, "抽取滤波器 {f} Hz 抑制 {d:.1} dB 不足 60 dB");
        let i = rt.resampler_gain_db(f, false) + 20.0 * (MIMO12K_GROUP as f64).log10();
        assert!(i <= -60.0, "插值镜像 {f} Hz 抑制 {i:.1} dB 不足 60 dB");
    }
    println!("resampler passband: {gains:?}");
    println!(
        "12k G span {:.1} ms | delay48 {} samples ({:.2} ms)",
        rt.layout().g_span_ms(),
        rt.delay48(),
        rt.layout().delay_ms()
    );
}

// ---------------------------------------------------------------------------
// 3) 延迟对齐：单位冲激峰值 = 输入位置 + delay48，前回声 ≤ -50 dB
// ---------------------------------------------------------------------------
#[ignore = "requires the MIMO artifact directory produced by the design tooling; not shipped in this repository"]
#[test]
fn impulse_delay_alignment() {
    let art = load_artifact();
    // 用纯 δ_D12 修正（对角）驱动：g[j][j][D12]=1，其余 0 -> 支路 = 抽取+δ_D12+插值。
    let mut g = vec![0.0f32; M * M * TAPS];
    for j in 0..M {
        g[(j * M + j) * TAPS + D12] = 1.0;
    }
    let layout = Mimo12kLayout::build(M, TAPS, BLOCK48, RESAMP_TAPS, D12).unwrap();
    let mut rt2 = Mimo12kRuntime::build(layout, &g, &art.decim, &art.interp).unwrap();
    let mut stim = vec![0.0f32; M * STIM_SAMPLES];
    let impulse_at = 1000usize;
    stim[impulse_at] = 1.0;
    let out = run_stream(&mut rt2, &stim, STIM_SAMPLES);
    let mut peak_idx = 0usize;
    let mut peak = 0.0f32;
    for (i, v) in out.iter().enumerate() {
        if v.abs() > peak {
            peak = v.abs();
            peak_idx = i;
        }
    }
    let expect = impulse_at + DSYS as usize;
    assert!(
        (peak_idx as isize - expect as isize).abs() <= 1,
        "冲激峰值 {peak_idx} != 输入 {impulse_at} + Δsys {DSYS}（±1）"
    );
    let mut pre = 0.0f32;
    let mut post = 0.0f32;
    for ch in 0..M {
        for n in 0..STIM_SAMPLES {
            let v = out[ch * STIM_SAMPLES + n].abs();
            if n + MIMO12K_GROUP * D12 + D1 < expect {
                pre = pre.max(v);
            } else if n > expect + 2 * D1 {
                post = post.max(v);
            }
        }
    }
    let pre_db = 20.0 * (pre.max(1e-30) / peak.max(1e-30)).log10();
    let post_db = 20.0 * (post.max(1e-30) / peak.max(1e-30)).log10();
    println!("impulse: peak@{peak_idx} Δsys={DSYS} pre={pre_db:.1} dB post={post_db:.1} dB");
    assert!(pre_db <= -50.0, "前回声 {pre_db:.1} dB > -50 dB");
}

// ---------------------------------------------------------------------------
// 4) 分区卷积兼容性：B48=64（B12=16）与 B48=128（B12=32）跨块长一致
// ---------------------------------------------------------------------------
#[ignore = "requires the MIMO artifact directory produced by the design tooling; not shipped in this repository"]
#[test]
fn partitioned_engine_block_size_invariance() {
    let art = load_artifact();
    let mut a = build_runtime(&art, 128);
    let mut b = build_runtime(&art, 64);
    assert_eq!(a.layout().low_block, 32);
    assert_eq!(b.layout().low_block, 16);
    assert_eq!(a.delay48(), b.delay48());
    let samples = 2048usize;
    let out_a = run_stream(&mut a, &art.stimulus, samples);
    let out_b = run_stream(&mut b, &art.stimulus, samples);
    let rel = rel_l2(&out_a, &out_b);
    println!("block-size invariance B128 vs B64: rel_L2={rel:.3e}");
    assert!(rel <= 1e-5, "跨块长修正在 rel L2 {rel:.3e} > 1e-5");
}

// ---------------------------------------------------------------------------
// 5) RT 路径零分配
// ---------------------------------------------------------------------------
#[ignore = "requires the MIMO artifact directory produced by the design tooling; not shipped in this repository"]
#[test]
fn rt_path_is_zero_alloc() {
    let art = load_artifact();
    let mut rt = build_runtime(&art, BLOCK48);
    let mut corr = vec![0.0f32; M * BLOCK48];
    alloc_warmup();
    let before = alloc_count();
    for b in 0..4 {
        let feeds = &art.stimulus[b * BLOCK48..];
        let status = rt.process_block_f32(feeds, STIM_SAMPLES, &mut corr, BLOCK48, BLOCK48);
        assert!(status.is_ok());
    }
    let allocs = alloc_count() - before;
    assert_eq!(allocs, 0, "RT process_block 发生 {allocs} 次分配");
}

// ---------------------------------------------------------------------------
// 6) 成本模型 + release 下实测 fake 吞吐
// ---------------------------------------------------------------------------
#[ignore = "requires the MIMO artifact directory produced by the design tooling; not shipped in this repository"]
#[test]
fn cost_model_and_measured_throughput() {
    let art = load_artifact();
    let rt = build_runtime(&art, BLOCK48);
    let layout = *rt.layout();
    let budget = layout.budget();
    println!(
        "cost model: conv {:.3} MMAC/s (complex-mac/lowblock {}), decim {:.3} MMAC/s, \
         interp {:.3} MMAC/s, total {:.3} MMAC/s, 48k-fullrate {:.1} MMAC/s, ratio {:.1}x",
        budget.conv_flops_per_sec / 2e6,
        budget.conv_complex_macs_per_low_block,
        budget.decim_macs_per_sec / 1e6,
        budget.interp_macs_per_sec / 1e6,
        budget.total_macs_per_sec / 1e6,
        budget.fullrate_48k_macs_per_sec / 1e6,
        budget.lowrate_vs_fullrate_ratio
    );
    println!(
        "layout: taps {} ({} ms) | lowblock {} | fft {} | partitions {} | routes {} | \
         plan {} samples | state {} samples | delay {} samples | alloc {} samples",
        layout.taps,
        layout.g_span_ms(),
        layout.low_block,
        layout.fft_size,
        layout.partitions,
        layout.n_routes,
        layout.conv_plan_samples,
        layout.conv_state_samples,
        layout.delay48,
        rt.allocated_samples()
    );
    assert!(
        budget.lowrate_vs_fullrate_ratio >= 5.0,
        "12k 修正支路成本应至少比 48k 全速融合 G 低 5x，实得 {:.2}x",
        budget.lowrate_vs_fullrate_ratio
    );
    // 实测（本机 fake/离线，不是板端；正式性能门 T46/T30 不在此卡）。
    let mut rt = build_runtime(&art, BLOCK48);
    let mut corr = vec![0.0f32; M * BLOCK48];
    let blocks = 4000u32;
    let started = std::time::Instant::now();
    for i in 0..blocks {
        let feed = &art.stimulus[((i as usize) % (N_BLOCKS - 1)) * BLOCK48..];
        let status = rt.process_block_f32(feed, STIM_SAMPLES, &mut corr, BLOCK48, BLOCK48);
        assert!(status.is_ok());
    }
    let elapsed = started.elapsed();
    let per_block_ns = elapsed.as_nanos() as f64 / blocks as f64;
    let deadline_ns = BLOCK48 as f64 / 48_000.0 * 1e9; // 2.667 ms / block
    let realtime_factor = per_block_ns / deadline_ns;
    println!(
        "measured fake throughput (host, not board): {per_block_ns:.0} ns/block, {:.0} blocks/s, \
         realtime factor {:.4} (debug={})",
        1e9 / per_block_ns,
        realtime_factor,
        cfg!(debug_assertions)
    );
    if !cfg!(debug_assertions) {
        assert!(
            realtime_factor < 1.0,
            "release 实测 {realtime_factor:.3} >= 1.0（128 帧块 deadline {deadline_ns:.0} ns）"
        );
    }
    // 12 kHz 域速率/延迟的静态断言。
    assert_eq!(MIMO12K_LOWRATE_HZ * MIMO12K_GROUP as u32, 48_000);
    assert!(layout.delay48 < 1200, "Δsys 应落在 M2 12-21 ms 级预算内");
}
