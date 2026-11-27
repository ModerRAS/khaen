//! 图执行器实时路径零分配门禁（独立测试二进制 + 计数全局分配器）。
//!
//! 本文件只放一个测试：本二进制内没有别的测试并发跑，因此全局分配计数是干净的。计量窗口内
//! 不得有任何堆分配——图的所有缓冲（planar scratch、卷积状态、延迟线、PEQ 状态）必须在
//! `PlanGraph::compile` 里分配完毕。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    ProtectionEntry, ProtectionSection, RoutingSection,
};
use khaen_host::graph::{KernelChoice, KernelPath};
use khaen_host::{AdmissionLimits, EngineConfig, PlanGraph, Processor};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// 确定性长 FIR（只用于让 lane 0 走分区卷积路径；数值形状不重要）。
fn long_fir(taps: usize) -> Vec<f32> {
    (0..taps)
        .map(|index| {
            let sign = if index % 2 == 0 { 1.0 } else { -1.0 };
            sign / (index as f32 + 1.0)
        })
        .collect()
}

/// 非平凡链：五个段全部启用（content fold / crossover FIR / 满矩阵 / 保护 / 路由）。
///
/// crossover lane 0 用 2048 taps（> CROSSOVER_PARTITIONED_MIN_TAPS -> UPOLS 分区卷积路径），
/// lane 1 用 3 taps（direct 直算路径）：零分配断言同时覆盖两条卷积实现。
fn busy_plan() -> Plan {
    Plan {
        block_size: 128,
        sample_rate: 96_000,
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
                    coeffs: long_fir(2048),
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
            coeffs: vec![1.0, 0.0, 0.5, -0.25, 0.0, 0.0, 0.75, 0.0],
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
fn graph_process_performs_no_heap_allocation() {
    let plan = busy_plan();
    let config = EngineConfig {
        block_frames: 128,
        sample_rate: 96_000,
        input_channels: 2,
        output_channels: 2,
    };
    let mut graph =
        PlanGraph::compile(&plan, &config, &AdmissionLimits::default()).expect("compile graph");
    // T56：图侧派发快照是真实探测值；Auto 在本机有 SIMD 时必须接优化入口（partitioned 与
    // direct 两条 lane 路径都在本图里）。
    let backend = graph.simd_backend();
    assert_eq!(graph.kernel_choice(), KernelChoice::Auto);
    assert_eq!(
        graph.kernel_path(),
        if backend.is_simd() {
            KernelPath::Optimized
        } else {
            KernelPath::Scalar
        },
        "Auto 路径必须与本机后端探测一致"
    );
    assert_eq!(graph.kernel_dispatch().backend, backend);
    let mut input = vec![0.0f32; 128 * 2];
    let mut output = vec![0.0f32; 128 * 2];
    for (index, sample) in input.iter_mut().enumerate() {
        *sample = (index as f32) * 1.0e-3 - 0.25;
    }

    // 预热：让首次触碰的懒惰初始化（内核/分支预测等）发生在计量窗口之外。
    graph.process(&input, &mut output);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for round in 0..500usize {
        for (index, sample) in input.iter_mut().enumerate() {
            *sample = (((round * 31 + index) % 97) as f32) * 1.0e-2 - 0.5;
        }
        graph.process(&input, &mut output);
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocated, 0, "graph process allocated {allocated} times");
    assert!(
        output.iter().any(|sample| *sample != 0.0),
        "busy chain produced only silence"
    );

    // 标量回退（显式 KernelChoice::Scalar）的 RT 路径同样零分配：两条内核入口都不能在
    // process 里分配。编译本身允许分配，因此窗口在编译与预热之后才开。
    let mut scalar_graph = PlanGraph::compile_with_kernel(
        &plan,
        &config,
        &AdmissionLimits::default(),
        KernelChoice::Scalar,
    )
    .expect("compile scalar graph");
    assert_eq!(scalar_graph.kernel_path(), KernelPath::Scalar);
    for _ in 0..8usize {
        scalar_graph.process(&input, &mut output);
    }
    let before_scalar = ALLOCATIONS.load(Ordering::Relaxed);
    for round in 0..200usize {
        for (index, sample) in input.iter_mut().enumerate() {
            *sample = (((round * 13 + index) % 89) as f32) * 1.0e-2 - 0.45;
        }
        scalar_graph.process(&input, &mut output);
    }
    let allocated_scalar = ALLOCATIONS.load(Ordering::Relaxed) - before_scalar;
    assert_eq!(
        allocated_scalar, 0,
        "scalar graph process allocated {allocated_scalar} times"
    );
    assert!(
        output.iter().any(|sample| *sample != 0.0),
        "scalar busy chain produced only silence"
    );
}
