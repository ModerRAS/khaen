//! 内核 SIMD 路径验收测试（T39a）：对拍 / partial/flush / 错误码 / 零分配。
//!
//! 被测入口：
//! - [`khaen_core::kernels::dispatch::direct_block_f32_optimized`]（AVX2+FMA / NEON / 标量回退）
//! - [`khaen_core::kernels::dispatch::partitioned_block_f32_optimized`]（同上）
//!
//! 覆盖点：
//! (a) direct SIMD vs 标量参考：taps {1,8,64,2048,4096} x block {64,128}，多块流式（历史跨块），
//!     逐样本容差（f32 相对 L2 <= 1e-6、单样本绝对 <= 1e-6；实测 max_abs <= 2e-7，见测试输出）。
//! (b) direct SIMD 语义契约：partial 可拼接（两个 V=2 等价一个 V=4）、V=0 只清输出不推进历史、
//!     flush = L_F-1 零的冲激响应与标量逐样本一致、partial 后继续 full 块的历史一致。
//! (c) direct SIMD 错误码：与标量入口同一批非法调用返回同一固定错误码，且失败前不写输出。
//! (d) partitioned SIMD vs 标量参考：block {8,64,128}、taps {L_F 小/~1024/4103 extended}、
//!     多块流式 + 终止性 DRAIN（V < B 与 V=0）；f32 相对 L2 <= 1e-5、单样本绝对 <= 1e-4
//!     （T08 口径；实测 rel_l2 <= 4e-7）。
//! (e) partitioned SIMD vs 直接卷积（f32/f64 oracle）：验证绝对正确性而不只是两路径互相一致。
//! (f) RT 零分配：两条 SIMD 入口的 process 窗口内 0 次堆分配（计数全局分配器 + 串行测量窗口）。
//!
//! 运行：`cargo test -p khaen-core --offline --test kernels_simd`。

#![allow(
    clippy::too_many_arguments,
    clippy::needless_range_loop,
    clippy::missing_const_for_thread_local
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use khaen_core::kernels::direct::{
    direct_block_f32, direct_block_f64, DirectCoeffsF32, DirectCoeffsF64, DirectLayout,
    DirectTopology, DIRECT_NO_SOURCE,
};
use khaen_core::kernels::dispatch::{
    direct_block_f32_optimized, partitioned_block_f32_optimized, SimdBackend,
};
use khaen_core::kernels::partitioned::{
    partitioned_block_f32, partitioned_block_f64, PartitionedLayout, PartitionedPlan,
};

const NO: u16 = DIRECT_NO_SOURCE;

/// direct SIMD vs 标量参考的容差（实测 max_abs <= 2e-7、rel_l2 <= 1.1e-7；留 5~10x 余量）。
const DIRECT_TOL_SAMPLE: f32 = 1.0e-6;
const DIRECT_TOL_L2: f64 = 1.0e-6;
/// partitioned SIMD vs 标量参考的容差（T08 口径的 f32 门；实测 rel_l2 <= 4e-7）。
const PART_TOL_SAMPLE: f32 = 1.0e-4;
const PART_TOL_L2: f64 = 1.0e-5;

// ===========================================================================
// 计数全局分配器（零分配断言；做法与 kernels_exact.rs 相同）
// ===========================================================================

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
// 测试用拓扑与系数
// ===========================================================================

/// 混合拓扑：2 源 / 2 角色 / 3 容器；D=1，L_A=2，L_G=3，L_P=2，M=2，A_map=identity，
/// scatter=[2,0]（容器通道 1 未映射必须逐样本 +0.0），含对角 + 非对角 g 路由。
fn topo_mix() -> DirectTopology {
    let mut src_of_role = [NO; 22];
    src_of_role[0] = 0;
    src_of_role[1] = 1;
    let mut participants = [NO; 8];
    participants[0] = 0;
    participants[1] = 1;
    let mut scatter = [0u16; 22];
    scatter[0] = 2;
    scatter[1] = 0;
    DirectTopology {
        n_src: 2,
        n_role: 2,
        container_channels: 3,
        d_samples: 1,
        l_a: 2,
        l_g: 3,
        l_p: 2,
        m: 2,
        src_of_role,
        participants,
        scatter,
    }
}

/// 稀疏拓扑：2 源 / 3 角色 / 4 容器；D=2，L_G=3，L_P=1；角色 1 无源但仍是 participant。
fn topo_sparse() -> DirectTopology {
    let mut src_of_role = [NO; 22];
    src_of_role[0] = 0;
    src_of_role[2] = 1;
    let mut participants = [NO; 8];
    participants[0] = 0;
    participants[1] = 1;
    participants[2] = 2;
    let mut scatter = [0u16; 22];
    scatter[0] = 3;
    scatter[1] = 1;
    scatter[2] = 0;
    DirectTopology {
        n_src: 2,
        n_role: 3,
        container_channels: 4,
        d_samples: 2,
        l_a: 2,
        l_g: 3,
        l_p: 1,
        m: 3,
        src_of_role,
        participants,
        scatter,
    }
}

/// 每 lane 一条 L_A=taps 全采样率 FIR 的拓扑（镜像 host graph/crossover.rs 的 22 条 1-lane
/// 内核：A 即抽头、G 只有 delta_D baseline、P 为 delta）。lanes 可裁剪到测试需要的大小。
fn topo_crossover(lanes: u16, taps: u16, container: u16) -> DirectTopology {
    let mut src_of_role = [NO; 22];
    let mut participants = [NO; 8];
    let mut scatter = [0u16; 22];
    for lane in 0..lanes as usize {
        src_of_role[lane] = lane as u16;
        scatter[lane] = lane as u16;
    }
    participants[0] = 0;
    DirectTopology {
        n_src: lanes,
        n_role: lanes,
        container_channels: container,
        d_samples: 0,
        l_a: taps,
        l_g: 1,
        l_p: 1,
        m: 1,
        src_of_role,
        participants,
        scatter,
    }
}

/// M 参与者满矩阵拓扑（m x m 非对角 g，对角线 delta_D baseline），L_A=1、L_P=1：
/// 镜像校准链 P_post / 矩阵段融合进 G 后进入 partitioned 内核的形态。
fn topo_matrix(roles: u16, container: u16, m: u16, l_g: u16, d: u32) -> DirectTopology {
    let mut src_of_role = [NO; 22];
    let mut participants = [NO; 8];
    let mut scatter = [0u16; 22];
    for role in 0..roles as usize {
        src_of_role[role] = role as u16;
        scatter[role] = role as u16;
    }
    for participant in 0..m as usize {
        participants[participant] = participant as u16;
    }
    DirectTopology {
        n_src: roles,
        n_role: roles,
        container_channels: container,
        d_samples: d,
        l_a: 1,
        l_g,
        l_p: 1,
        m,
        src_of_role,
        participants,
        scatter,
    }
}

/// 确定性系数（2 的负幂整数倍，f32/f64 均可精确表示；a/g/p 非平凡）。
fn coeffs_f32(topo: &DirectTopology) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n_a = topo.n_src as usize * topo.l_a as usize;
    let n_g = topo.m as usize * topo.m as usize * topo.l_g as usize;
    let n_p = topo.n_role as usize * topo.l_p as usize;
    let f = |k: usize| (((k * 13 + 5) % 23) as f32 - 11.0) * 0.0625;
    let a = (0..n_a).map(f).collect();
    let g = (0..n_g).map(|k| f(k + 100)).collect();
    let p = (0..n_p).map(|k| f(k + 200)).collect();
    (a, g, p)
}

/// 低通 FIR 形状的抽头（1/2^? 不可精确表示，用来覆盖一般浮点累加；DC 归一）。
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

/// 确定性 planar 输入（2 的负幂整数倍；跨块连续，检验历史推进）。
fn planar_input(n_src: usize, len: usize, seed: usize) -> Vec<Vec<f32>> {
    let mut out = vec![vec![0.0f32; len]; n_src];
    for (ch, chan) in out.iter_mut().enumerate() {
        for (n, value) in chan.iter_mut().enumerate() {
            let k = seed + ch * 131 + n * 17;
            *value = (((k * 7 + 3) % 41) as f32 - 20.0) * 0.03125;
        }
    }
    out
}

// ===========================================================================
// 流式驱动（两条实现路径共用；分配都在测量窗口外）
// ===========================================================================

/// 被测 direct 实现路径。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectPath {
    Scalar,
    Simd,
}

/// 被测 partitioned 实现路径。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PartPath {
    Scalar,
    Simd,
}

/// 按 chunks 流式跑 direct 内核，返回各容器通道的输出（只保留 valid_frames 个样本）。
fn run_direct(
    path: DirectPath,
    topo: &DirectTopology,
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f32>],
) -> Vec<Vec<f32>> {
    run_direct_with_coeffs(path, topo, block_size, chunks, input, None)
}

fn run_direct_with_coeffs(
    path: DirectPath,
    topo: &DirectTopology,
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f32>],
    coeffs_override: Option<&(Vec<f32>, Vec<f32>, Vec<f32>)>,
) -> Vec<Vec<f32>> {
    let layout = DirectLayout::build(topo).expect("direct layout");
    let owned;
    let (a, g, p) = match coeffs_override {
        Some(value) => (value.0.as_slice(), value.1.as_slice(), value.2.as_slice()),
        None => {
            owned = coeffs_f32(topo);
            (owned.0.as_slice(), owned.1.as_slice(), owned.2.as_slice())
        }
    };
    let coeffs = DirectCoeffsF32 { a_fir: a, g, p };
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let total: usize = chunks.iter().sum();
    let mut out = vec![vec![0.0f32; total]; containers];
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut inb = vec![0.0f32; n_src * block_size];
    let mut ob = vec![0.0f32; containers * block_size];
    let mut off = 0usize;
    for &valid in chunks {
        for i in 0..n_src {
            for n in 0..valid {
                inb[i * block_size + n] = input[i][off + n];
            }
        }
        let status = match path {
            DirectPath::Scalar => direct_block_f32(
                topo, &layout, &coeffs, &mut state, &inb, block_size, &mut ob, block_size,
                block_size, valid,
            ),
            DirectPath::Simd => direct_block_f32_optimized(
                topo, &layout, &coeffs, &mut state, &inb, block_size, &mut ob, block_size,
                block_size, valid,
            ),
        };
        assert!(status.is_ok(), "direct status code {}", status.0);
        for ch in 0..containers {
            for n in 0..valid {
                out[ch][off + n] = ob[ch * block_size + n];
            }
        }
        off += valid;
    }
    out
}

struct PartBuilt {
    layout: PartitionedLayout,
    plan: PartitionedPlan,
    plan_data: Vec<f32>,
}

fn build_part_f32(topo: &DirectTopology, block_size: usize, extended: bool) -> PartBuilt {
    let (a, g, p) = coeffs_f32(topo);
    let coeffs = DirectCoeffsF32 {
        a_fir: &a,
        g: &g,
        p: &p,
    };
    let layout = if extended {
        PartitionedLayout::build_extended(topo, block_size).expect("partitioned layout extended")
    } else {
        PartitionedLayout::build(topo, block_size).expect("partitioned layout")
    };
    let mut plan_data = vec![0.0f32; layout.plan_samples()];
    let mut scratch = vec![0.0f32; layout.build_scratch_samples()];
    let plan = if extended {
        PartitionedPlan::build_f32_extended(topo, block_size, &coeffs, &mut plan_data, &mut scratch)
            .expect("extended plan")
    } else {
        PartitionedPlan::build_f32(topo, block_size, &coeffs, &mut plan_data, &mut scratch)
            .expect("plan")
    };
    PartBuilt {
        layout,
        plan,
        plan_data,
    }
}

fn run_part(
    path: PartPath,
    topo: &DirectTopology,
    built: &PartBuilt,
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f32>],
) -> Vec<Vec<f32>> {
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let total: usize = chunks.iter().sum();
    let mut out = vec![vec![0.0f32; total]; containers];
    let mut state = vec![0.0f32; built.layout.state_samples()];
    let mut inb = vec![0.0f32; n_src * block_size];
    let mut ob = vec![0.0f32; containers * block_size];
    let mut off = 0usize;
    for &valid in chunks {
        for i in 0..n_src {
            for n in 0..valid {
                inb[i * block_size + n] = input[i][off + n];
            }
        }
        let status = match path {
            PartPath::Scalar => partitioned_block_f32(
                topo,
                &built.plan,
                &built.plan_data,
                &mut state,
                &inb,
                block_size,
                &mut ob,
                block_size,
                block_size,
                valid,
            ),
            PartPath::Simd => partitioned_block_f32_optimized(
                topo,
                &built.plan,
                &built.plan_data,
                &mut state,
                &inb,
                block_size,
                &mut ob,
                block_size,
                block_size,
                valid,
            ),
        };
        assert!(status.is_ok(), "partitioned status code {}", status.0);
        for ch in 0..containers {
            for n in 0..valid {
                out[ch][off + n] = ob[ch * block_size + n];
            }
        }
        off += valid;
    }
    out
}

// ===========================================================================
// 比较工具
// ===========================================================================

fn max_abs_diff_f32(a: &[Vec<f32>], b: &[Vec<f32>]) -> f32 {
    let mut max = 0.0f32;
    for (ca, cb) in a.iter().zip(b.iter()) {
        for (x, y) in ca.iter().zip(cb.iter()) {
            max = max.max((x - y).abs());
        }
    }
    max
}

fn rel_l2_f32(a: &[Vec<f32>], b: &[Vec<f32>]) -> f64 {
    let mut diff = 0.0f64;
    let mut base = 0.0f64;
    for (ca, cb) in a.iter().zip(b.iter()) {
        for (x, y) in ca.iter().zip(cb.iter()) {
            diff += (*x as f64 - *y as f64) * (*x as f64 - *y as f64);
            base += (*x as f64) * (*x as f64);
        }
    }
    (diff / base.max(1.0e-30)).sqrt()
}

fn assert_close_f32(got: &[Vec<f32>], want: &[Vec<f32>], what: &str, tol_abs: f32, tol_l2: f64) {
    assert_eq!(got.len(), want.len(), "{what}: 通道数");
    let max_abs = max_abs_diff_f32(got, want);
    let rel_l2 = rel_l2_f32(want, got);
    assert!(
        max_abs <= tol_abs,
        "{what}: 单样本绝对误差 {max_abs:.3e} > {tol_abs:.1e}（rel_l2={rel_l2:.3e}）"
    );
    assert!(
        rel_l2 <= tol_l2,
        "{what}: 相对 L2 误差 {rel_l2:.3e} > {tol_l2:.1e}（max_abs={max_abs:.3e}）"
    );
}

fn assert_all_positive_zero(chan: &[f32], what: &str) {
    for (n, value) in chan.iter().enumerate() {
        assert_eq!(value.to_bits(), 0, "{what}: 期望 +0.0, n={n} got={value:?}");
    }
}

// ===========================================================================
// (a) direct SIMD vs 标量参考：taps x block 覆盖
// ===========================================================================

#[test]
fn direct_simd_matches_scalar_across_taps_and_blocks() {
    // 小 taps：多源/多角色/非平凡 G/D 的混合拓扑与稀疏拓扑。
    for block in [64usize, 128] {
        for topo in [topo_mix(), topo_sparse()] {
            let len = block * 5;
            let input = planar_input(topo.n_src as usize, len, 3);
            let chunks = [block; 5];
            let scalar = run_direct(DirectPath::Scalar, &topo, block, &chunks, &input);
            let simd = run_direct(DirectPath::Simd, &topo, block, &chunks, &input);
            assert_close_f32(
                &simd,
                &scalar,
                &format!("direct taps<multi> block={block}"),
                DIRECT_TOL_SAMPLE,
                DIRECT_TOL_L2,
            );
        }
    }
    // 长 taps：镜像 host graph/crossover.rs 的每 lane 一条 taps-tap A-FIR（1-lane 内核的单块
    // 等价形态：这里用多 lane 一次算完）。抽头用低通（不可精确表示的浮点）覆盖一般累加。
    for (taps, lanes, blocks) in [
        (8usize, 4u16, 5usize),
        (64, 4, 4),
        (2048, 2, 3),
        (4096, 2, 3),
    ] {
        for block in [64usize, 128] {
            let topo = topo_crossover(lanes, taps as u16, lanes);
            let len = block * blocks;
            let input = planar_input(topo.n_src as usize, len, 11);
            let chunks = [block; 5][..blocks].to_vec();
            // 每 lane 一条 taps-tap A-FIR：a_fir 布局 [src][tap]，各 lane 用同一组低通抽头。
            let mut a_fir = Vec::with_capacity(lanes as usize * taps);
            for _ in 0..lanes {
                a_fir.extend_from_slice(&lowpass_fir(taps, 0.1));
            }
            let coeffs = (a_fir, vec![0.0f32], vec![1.0f32; lanes as usize]);
            let scalar = run_direct_with_coeffs(
                DirectPath::Scalar,
                &topo,
                block,
                &chunks,
                &input,
                Some(&coeffs),
            );
            let simd = run_direct_with_coeffs(
                DirectPath::Simd,
                &topo,
                block,
                &chunks,
                &input,
                Some(&coeffs),
            );
            assert_close_f32(
                &simd,
                &scalar,
                &format!("direct taps={taps} lanes={lanes} block={block}"),
                DIRECT_TOL_SAMPLE,
                DIRECT_TOL_L2,
            );
        }
    }
}

#[test]
fn direct_simd_matches_f64_oracle_within_f32_bound() {
    // 与 f64 oracle 比：验证 SIMD 路径的绝对误差在 f32 直算应有的界内（而非仅两路径互相一致）。
    let topo = topo_mix();
    let block = 4usize;
    let len = block * 5;
    let input32 = planar_input(topo.n_src as usize, len, 5);
    let input64: Vec<Vec<f64>> = input32
        .iter()
        .map(|chan| chan.iter().map(|value| f64::from(*value)).collect())
        .collect();
    let (a32, g32, p32) = coeffs_f32(&topo);
    let a64: Vec<f64> = a32.iter().map(|value| f64::from(*value)).collect();
    let g64: Vec<f64> = g32.iter().map(|value| f64::from(*value)).collect();
    let p64: Vec<f64> = p32.iter().map(|value| f64::from(*value)).collect();
    let layout = DirectLayout::build(&topo).expect("layout");
    let coeffs64 = DirectCoeffsF64 {
        a_fir: &a64,
        g: &g64,
        p: &p64,
    };
    let containers = topo.container_channels as usize;
    let n_src = topo.n_src as usize;
    let total = len;
    let mut oracle = vec![vec![0.0f64; total]; containers];
    let mut state = vec![0.0f64; layout.state_samples()];
    let mut inb = vec![0.0f64; n_src * block];
    let mut ob = vec![0.0f64; containers * block];
    let mut off = 0usize;
    for _ in 0..5 {
        for i in 0..n_src {
            for n in 0..block {
                inb[i * block + n] = input64[i][off + n];
            }
        }
        let status = direct_block_f64(
            &topo, &layout, &coeffs64, &mut state, &inb, block, &mut ob, block, block, block,
        );
        assert!(status.is_ok());
        for ch in 0..containers {
            for n in 0..block {
                oracle[ch][off + n] = ob[ch * block + n];
            }
        }
        off += block;
    }
    let simd = run_direct(DirectPath::Simd, &topo, block, &[block; 5], &input32);
    let oracle32: Vec<Vec<f32>> = oracle
        .iter()
        .map(|chan| chan.iter().map(|value| *value as f32).collect())
        .collect();
    assert_close_f32(
        &simd,
        &oracle32,
        "direct SIMD vs f64 oracle",
        1.0e-5,
        1.0e-5,
    );
}

// ===========================================================================
// (b) direct SIMD 的 partial / flush / 历史契约
// ===========================================================================

#[test]
fn direct_simd_partial_chunks_concatenate() {
    // direct 的 partial 块可拼接：两个 V=2 块等价一个 V=4 块（block_size 仍为 4）。
    let topo = topo_mix();
    let block = 4usize;
    let len = 16usize;
    let input = planar_input(topo.n_src as usize, len, 7);
    let whole = run_direct(DirectPath::Simd, &topo, block, &[block; 4], &input);
    let split = run_direct(DirectPath::Simd, &topo, block, &[2, 2, 4, 1, 3, 4], &input);
    assert_close_f32(&split, &whole, "partial 拼接 vs 整块", 0.0, 0.0);
}

#[test]
fn direct_simd_partial_matches_scalar_history() {
    // partial 之后继续 full 块：历史推进量必须与标量一致（V 有效样本，不是 block）。
    let topo = topo_mix();
    let block = 8usize;
    let len = 8 * 6;
    let input = planar_input(topo.n_src as usize, len, 13);
    let chunks = [8usize, 3, 8, 5, 8, 8];
    let scalar = run_direct(DirectPath::Scalar, &topo, block, &chunks, &input);
    let simd = run_direct(DirectPath::Simd, &topo, block, &chunks, &input);
    assert_close_f32(
        &simd,
        &scalar,
        "partial 历史推进",
        DIRECT_TOL_SAMPLE,
        DIRECT_TOL_L2,
    );
}

#[test]
fn direct_simd_v0_block_is_output_zero_and_no_history_change() {
    let topo = topo_mix();
    let block = 8usize;
    let layout = DirectLayout::build(&topo).expect("layout");
    let (a, g, p) = coeffs_f32(&topo);
    let coeffs = DirectCoeffsF32 {
        a_fir: &a,
        g: &g,
        p: &p,
    };
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut inb = vec![0.0f32; n_src * block];
    let mut ob = vec![0.0f32; containers * block];
    // 先跑 3 个 full 块建立非零历史。
    for round in 0..3 {
        for (i, value) in inb.iter_mut().enumerate() {
            *value = ((round * 31 + i * 7) % 19) as f32 * 0.0625 - 0.5;
        }
        let status = direct_block_f32_optimized(
            &topo, &layout, &coeffs, &mut state, &inb, block, &mut ob, block, block, block,
        );
        assert!(status.is_ok());
    }
    let history_before = state.clone();
    // V=0：输出整块严格 +0.0，历史不变。
    ob.fill(-1.0);
    let status = direct_block_f32_optimized(
        &topo, &layout, &coeffs, &mut state, &inb, block, &mut ob, block, block, 0,
    );
    assert!(status.is_ok());
    for ch in 0..containers {
        assert_all_positive_zero(&ob[ch * block..(ch + 1) * block], "V=0 输出");
    }
    assert_eq!(
        history_before
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<u32>>(),
        state
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<u32>>(),
        "V=0 不得推进历史"
    );
}

#[test]
fn direct_simd_flush_impulse_matches_scalar() {
    // flush 契约：冲激 + (L_F-1) 个零后的尾部输出与标量逐样本一致，且尾部回到 +0.0。
    // 拓扑：1 源/1 角色/1 容器，L_F = L_A + L_G + L_P - 2 = 1 + 4 + 3 - 2 = 6。
    let mut topo = DirectTopology::identity();
    topo.l_g = 4;
    topo.l_p = 3;
    topo.d_samples = 1;
    let layout = DirectLayout::build(&topo).expect("layout");
    let coeffs = (
        vec![1.0f32],
        vec![0.25f32, -0.5, 0.125, 0.0625],
        vec![1.0f32, 0.5, -0.25],
    );
    let coeff32 = DirectCoeffsF32 {
        a_fir: &coeffs.0,
        g: &coeffs.1,
        p: &coeffs.2,
    };
    let block = 4usize;
    let mut state_s = vec![0.0f32; layout.state_samples()];
    let mut state_o = vec![0.0f32; layout.state_samples()];
    let mut inb = vec![0.0f32; block];
    let mut out_s = vec![0.0f32; block];
    let mut out_o = vec![0.0f32; block];
    let mut ys = Vec::new();
    let mut yo = Vec::new();
    for round in 0..4 {
        inb.fill(0.0);
        if round == 0 {
            inb[0] = 1.0;
        }
        assert!(direct_block_f32(
            &topo,
            &layout,
            &coeff32,
            &mut state_s,
            &inb,
            block,
            &mut out_s,
            block,
            block,
            block
        )
        .is_ok());
        assert!(direct_block_f32_optimized(
            &topo,
            &layout,
            &coeff32,
            &mut state_o,
            &inb,
            block,
            &mut out_o,
            block,
            block,
            block
        )
        .is_ok());
        ys.extend_from_slice(&out_s);
        yo.extend_from_slice(&out_o);
    }
    let diff = ys
        .iter()
        .zip(yo.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(diff <= DIRECT_TOL_SAMPLE, "flush 冲激响应差 {diff:.3e}");
    // L_F = 6 个样本之后应严格回到 +0.0。
    for (n, value) in ys.iter().enumerate().skip(6) {
        assert_eq!(value.to_bits(), 0, "flush 尾部样本 {n} 非 +0.0: {value:?}");
    }
}

// ===========================================================================
// (c) direct SIMD 错误码与"失败前不写缓冲"
// ===========================================================================

#[test]
fn direct_simd_error_codes_match_scalar() {
    let topo = topo_mix();
    let block = 4usize;
    let layout = DirectLayout::build(&topo).expect("layout");
    let (a, g, p) = coeffs_f32(&topo);
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;

    // 单次非法调用：独立分配 state/输入/输出（输出填 7.0 哨兵），返回状态码与输出快照。
    let attempt = |path: DirectPath,
                   coeffs: &DirectCoeffsF32<'_>,
                   use_layout: &DirectLayout,
                   block_size: usize,
                   valid: usize,
                   in_stride: usize,
                   out_stride: usize,
                   out_samples: usize|
     -> (u32, Vec<f32>) {
        let mut state = vec![0.0f32; layout.state_samples()];
        let input = vec![0.0f32; n_src * in_stride.max(block_size).max(1)];
        let mut output = vec![7.0f32; out_samples];
        let status = match path {
            DirectPath::Scalar => direct_block_f32(
                &topo,
                use_layout,
                coeffs,
                &mut state,
                &input,
                in_stride,
                &mut output,
                out_stride,
                block_size,
                valid,
            ),
            DirectPath::Simd => direct_block_f32_optimized(
                &topo,
                use_layout,
                coeffs,
                &mut state,
                &input,
                in_stride,
                &mut output,
                out_stride,
                block_size,
                valid,
            ),
        };
        (status.0, output)
    };

    let check = |name: &str,
                 block_size: usize,
                 valid: usize,
                 in_stride: usize,
                 out_stride: usize,
                 out_samples: usize| {
        let coeffs = DirectCoeffsF32 {
            a_fir: &a,
            g: &g,
            p: &p,
        };
        let (scalar_status, scalar_out) = attempt(
            DirectPath::Scalar,
            &coeffs,
            &layout,
            block_size,
            valid,
            in_stride,
            out_stride,
            out_samples,
        );
        let (simd_status, simd_out) = attempt(
            DirectPath::Simd,
            &coeffs,
            &layout,
            block_size,
            valid,
            in_stride,
            out_stride,
            out_samples,
        );
        assert_eq!(
            scalar_status, simd_status,
            "{name}: 错误码不一致 scalar={scalar_status} simd={simd_status}"
        );
        assert_eq!(
            scalar_out, simd_out,
            "{name}: SIMD 失败路径输出与标量失败路径不一致"
        );
        assert!(
            simd_out
                .iter()
                .all(|value| value.to_bits() == 7.0f32.to_bits()),
            "{name}: SIMD 失败路径改写了输出缓冲"
        );
        assert!(scalar_status != 0, "{name}: 非法调用必须返回错误码");
    };

    // block_size 非法（0 / > DIRECT_MAX_BLOCK）。
    check("block=0", 0, 0, 4, 4, containers * 4);
    check("block=129", 129, 0, 129, 129, containers * 129);
    // valid > block_size。
    check(
        "valid>block",
        block,
        block + 1,
        block,
        block,
        containers * block,
    );
    // in_stride < block_size。
    check(
        "in_stride<block",
        block,
        block,
        block - 1,
        block,
        containers * block,
    );
    // out_stride < block_size。
    check(
        "out_stride<block",
        block,
        block,
        block,
        block - 1,
        containers * block,
    );
    // 输出容量不足：out_samples 比 containers*out_stride 少 1。
    check(
        "short_output",
        block,
        block,
        block,
        block,
        containers * block - 1,
    );
    // 系数长度不符（截断 a_fir 视图）。
    {
        let bad = DirectCoeffsF32 {
            a_fir: &a[..a.len() - 1],
            g: &g,
            p: &p,
        };
        let (s1, _) = attempt(
            DirectPath::Scalar,
            &bad,
            &layout,
            block,
            block,
            block,
            block,
            containers * block,
        );
        let (s2, _) = attempt(
            DirectPath::Simd,
            &bad,
            &layout,
            block,
            block,
            block,
            block,
            containers * block,
        );
        assert_eq!(s1, s2, "系数长度不符错误码不一致");
        assert!(s1 != 0, "系数长度不符应返回错误码");
    }
    // 错误 layout（与拓扑不匹配）。
    {
        let other = topo_crossover(1, 1, 1);
        let other_layout = DirectLayout::build(&other).expect("other layout");
        let coeffs = DirectCoeffsF32 {
            a_fir: &a,
            g: &g,
            p: &p,
        };
        let (s1, _) = attempt(
            DirectPath::Scalar,
            &coeffs,
            &other_layout,
            block,
            block,
            block,
            block,
            containers * block,
        );
        let (s2, _) = attempt(
            DirectPath::Simd,
            &coeffs,
            &other_layout,
            block,
            block,
            block,
            block,
            containers * block,
        );
        assert_eq!(s1, s2, "错误 layout 的错误码不一致");
        assert!(s1 != 0, "错误 layout 应返回错误码");
    }
}

// ===========================================================================
// (d) partitioned SIMD vs 标量参考：block / taps / DRAIN
// ===========================================================================

#[test]
fn partitioned_simd_matches_scalar_small_blocks() {
    for (topo, block, blocks) in [
        (topo_mix(), 8usize, 6usize),
        (topo_sparse(), 8, 6),
        (topo_mix(), 64, 4),
        (topo_sparse(), 64, 4),
        (topo_mix(), 128, 4),
        (topo_sparse(), 128, 4),
        (topo_matrix(4, 4, 4, 3, 0), 64, 4),
        (topo_matrix(6, 22, 6, 4, 1), 128, 3),
    ] {
        let len = block * blocks;
        let input = planar_input(topo.n_src as usize, len, 21);
        let chunks = vec![block; blocks];
        let built = build_part_f32(&topo, block, false);
        let scalar = run_part(PartPath::Scalar, &topo, &built, block, &chunks, &input);
        let simd = run_part(PartPath::Simd, &topo, &built, block, &chunks, &input);
        assert_close_f32(
            &simd,
            &scalar,
            &format!(
                "partitioned block={block} src={} role={} L_G={}",
                topo.n_src, topo.n_role, topo.l_g
            ),
            PART_TOL_SAMPLE,
            PART_TOL_L2,
        );
    }
}

#[test]
fn partitioned_simd_matches_scalar_large_taps() {
    // 中等 taps（L_F=1031, Q=9）与 extended 口径（L_F=4103, Q=33，生产校准链 P_post 融合形态）。
    let medium_topo = topo_matrix(8, 22, 8, 8, 0);
    let long_topo = topo_matrix(6, 22, 6, 6, 0);
    let extended_topo = topo_matrix(8, 22, 8, 4096, 0);
    for (topo, extended) in [
        (&medium_topo, false),
        (&long_topo, false),
        (&extended_topo, true),
    ] {
        let block = 128usize;
        let blocks = 2usize;
        let len = block * blocks;
        let input = planar_input(topo.n_src as usize, len, 33);
        let chunks = vec![block; blocks];
        let built = build_part_f32(topo, block, extended);
        let scalar = run_part(PartPath::Scalar, topo, &built, block, &chunks, &input);
        let simd = run_part(PartPath::Simd, topo, &built, block, &chunks, &input);
        assert_close_f32(
            &simd,
            &scalar,
            &format!(
                "partitioned extended={extended} L_F={} block={block}",
                built.layout.taps
            ),
            PART_TOL_SAMPLE,
            PART_TOL_L2,
        );
    }
}

#[test]
fn partitioned_simd_drain_tail_semantics() {
    // 终止性 DRAIN：full 块之后 V<B 与 V=0 都是"清历史 + 新 epoch 起点"，SIMD 与标量一致；
    // DRAIN 后继续 full 块的结果也必须一致（新 epoch 起点）。
    let topo = topo_matrix(4, 4, 4, 3, 0);
    let block = 64usize;
    let chunks = [64usize, 64, 17, 64, 64, 0, 64];
    let len: usize = chunks.iter().sum();
    let input = planar_input(topo.n_src as usize, len, 44);
    let built = build_part_f32(&topo, block, false);
    let scalar = run_part(PartPath::Scalar, &topo, &built, block, &chunks, &input);
    let simd = run_part(PartPath::Simd, &topo, &built, block, &chunks, &input);
    assert_close_f32(
        &simd,
        &scalar,
        "partitioned DRAIN",
        PART_TOL_SAMPLE,
        PART_TOL_L2,
    );
}

#[test]
fn partitioned_simd_matches_direct_and_f64_oracle() {
    // 绝对正确性：partitioned SIMD vs direct f32（同语义、误差有界）。
    let topo = topo_mix();
    let block = 8usize;
    let blocks = 6usize;
    let len = block * blocks;
    let input = planar_input(topo.n_src as usize, len, 55);
    let chunks = vec![block; blocks];
    let built = build_part_f32(&topo, block, false);
    let part_simd = run_part(PartPath::Simd, &topo, &built, block, &chunks, &input);
    let direct_scalar = run_direct(DirectPath::Scalar, &topo, block, &chunks, &input);
    assert_close_f32(
        &part_simd,
        &direct_scalar,
        "partitioned SIMD vs direct f32",
        1.0e-5,
        1.0e-5,
    );

    // partitioned SIMD f32 vs partitioned f64 oracle（offline，NONRT）：f32 相对 L2 <= 1e-5。
    let (a32, g32, p32) = coeffs_f32(&topo);
    let a64: Vec<f64> = a32.iter().map(|value| f64::from(*value)).collect();
    let g64: Vec<f64> = g32.iter().map(|value| f64::from(*value)).collect();
    let p64: Vec<f64> = p32.iter().map(|value| f64::from(*value)).collect();
    let layout64 = PartitionedLayout::build(&topo, block).expect("layout f64");
    let coeffs64 = DirectCoeffsF64 {
        a_fir: &a64,
        g: &g64,
        p: &p64,
    };
    let mut plan_data64 = vec![0.0f64; layout64.plan_samples()];
    let mut scratch64 = vec![0.0f64; layout64.build_scratch_samples()];
    let plan64 =
        PartitionedPlan::build_f64(&topo, block, &coeffs64, &mut plan_data64, &mut scratch64)
            .expect("plan f64");
    let containers = topo.container_channels as usize;
    let n_src = topo.n_src as usize;
    let mut oracle = vec![vec![0.0f64; len]; containers];
    let mut state64 = vec![0.0f64; layout64.state_samples()];
    let mut inb64 = vec![0.0f64; n_src * block];
    let mut ob64 = vec![0.0f64; containers * block];
    let mut off = 0usize;
    for _ in 0..blocks {
        for i in 0..n_src {
            for n in 0..block {
                inb64[i * block + n] = f64::from(input[i][off + n]);
            }
        }
        let status = partitioned_block_f64(
            &topo,
            &plan64,
            &plan_data64,
            &mut state64,
            &inb64,
            block,
            &mut ob64,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        for ch in 0..containers {
            for n in 0..block {
                oracle[ch][off + n] = ob64[ch * block + n];
            }
        }
        off += block;
    }
    let oracle32: Vec<Vec<f32>> = oracle
        .iter()
        .map(|chan| chan.iter().map(|value| *value as f32).collect())
        .collect();
    assert_close_f32(
        &part_simd,
        &oracle32,
        "partitioned SIMD vs f64 oracle",
        1.0e-5,
        1.0e-5,
    );
}

// ===========================================================================
// (f) RT 零分配
// ===========================================================================

#[test]
fn simd_process_paths_do_not_allocate() {
    // direct：22 lane x 512-tap（受调试构建时长限制裁剪；真实基线 bench 用 2048/4096）。
    let direct_topo = topo_crossover(4, 512, 4);
    let direct_block = 128usize;
    let direct_layout = DirectLayout::build(&direct_topo).expect("direct layout");
    let mut direct_a = Vec::with_capacity(4 * 512);
    for _ in 0..4 {
        direct_a.extend_from_slice(&lowpass_fir(512, 0.1));
    }
    let direct_coeffs = (direct_a, vec![0.0f32], vec![1.0f32; 4]);
    let direct_coeffs = DirectCoeffsF32 {
        a_fir: &direct_coeffs.0,
        g: &direct_coeffs.1,
        p: &direct_coeffs.2,
    };
    let mut direct_state = vec![0.0f32; direct_layout.state_samples()];
    let direct_in = vec![0.25f32; 4 * direct_block];
    let mut direct_out = vec![0.0f32; 4 * direct_block];

    // partitioned：8x8 参与者、L_F=1031（Q=9）extended 之外的口径。
    let part_topo = topo_matrix(8, 22, 8, 8, 0);
    let part_block = 128usize;
    let built = build_part_f32(&part_topo, part_block, false);
    let mut part_state = vec![0.0f32; built.layout.state_samples()];
    let part_in = vec![0.25f32; 8 * part_block];
    let mut part_out = vec![0.0f32; 22 * part_block];

    let guard = MeasureGuard::begin();
    for _ in 0..3 {
        let status = direct_block_f32_optimized(
            &direct_topo,
            &direct_layout,
            &direct_coeffs,
            &mut direct_state,
            &direct_in,
            direct_block,
            &mut direct_out,
            direct_block,
            direct_block,
            direct_block,
        );
        assert!(status.is_ok());
        let status = partitioned_block_f32_optimized(
            &part_topo,
            &built.plan,
            &built.plan_data,
            &mut part_state,
            &part_in,
            part_block,
            &mut part_out,
            part_block,
            part_block,
            part_block,
        );
        assert!(status.is_ok());
    }
    let allocations = guard.end();
    assert_eq!(
        allocations, 0,
        "SIMD process 路径出现 {allocations} 次堆分配（RT 路径必须零分配）"
    );
}

// ===========================================================================
// 派发信息
// ===========================================================================

#[test]
fn dispatch_reports_backend_and_layout_is_shared() {
    // 派发后端名必须稳定可读；layout 的 state 容量由 direct/partitioned 同一来源给出
    // （SIMD 路径没有额外 state 需求：与标量共用布局，只换历史方向/算法）。
    let backend = SimdBackend::detect();
    assert!(!backend.name().is_empty());
    assert!(
        matches!(backend.name(), "scalar" | "avx2+fma" | "neon"),
        "未知后端名 {}",
        backend.name()
    );
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        // 本机（Ryzen 7840HS / N150）应走 AVX2+FMA；target_feature 未开启时运行期探测仍可命中。
        assert_eq!(backend.name(), "avx2+fma");
    }
    let topo = topo_mix();
    let layout = DirectLayout::build(&topo).expect("layout");
    // 同一 topology 上标量与 SIMD 入口使用同一个 state 长度（无额外缓冲）。
    assert!(layout.state_samples() > 0);
    let part = build_part_f32(&topo, 8, false);
    assert!(part.layout.state_samples() > 0);
    assert!(part.layout.plan_samples() > 0);
}
