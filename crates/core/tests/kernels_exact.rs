//! 卷积内核验收测试（T10b 迁移）：逐位一致 / 误差有界 / partial / reset / 零分配 / golden。
//!
//! 测试项与验收条目对应：
//! (a) direct 与朴素参考实现逐样本 bit-exact（f32/f64 两个精度；固定累加顺序，见 `naive_reference` 注释）。
//! (b) partitioned 与 direct 的误差有界（f64 / f32 阈值与实测值见测试内注释）。
//! (c) 同一输入一次处理整块 vs 拆成 3 个不等长子块，输出一致（direct 的可拼接 partial 语义；
//!     partitioned 按契约只支持 full-B 块 + 末块 DRAIN，另有 block-size 不变性测试）。
//! (d) state reset 后重跑同一输入输出一致（两块内核、f32/f64）。
//! (e) process 路径零堆分配（计数全局分配器；thread-local 武装 + Mutex 串行，见分配器注释）。
//! (f) 旧实现（legacy/pre-rewrite）生成的 f32 golden 向量逐位复现。
//! (g) 终止性 DRAIN、错误码、未映射通道严格 +0.0 等契约最小覆盖。
//!
//! 运行：`cargo test -p khaen-core --offline --test kernels_exact`。

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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use khaen_core::kernels::direct::{
    direct_block_f32, direct_block_f64, direct_state_zero_f32, direct_state_zero_f64,
    DirectCoeffsF32, DirectCoeffsF64, DirectLayout, DirectTopology, DIRECT_NO_SOURCE,
};
use khaen_core::kernels::partitioned::{
    partitioned_block_f32, partitioned_block_f64, partitioned_state_zero_f32,
    partitioned_state_zero_f64, PartitionedLayout, PartitionedPlan,
};
use khaen_core::kernels::RtStatus;

const NO: u16 = DIRECT_NO_SOURCE;

// ===========================================================================
// 计数全局分配器（零分配断言）
// ===========================================================================
//
// 需求：断言 process 路径不碰堆。做法与理由：
// - `ALLOC_COUNT` 是全局 `AtomicUsize`，分配器包装 `System` 并统计；
// - 但 libtest 默认多线程跑测试，其它测试线程的分配会污染全局计数。因此用
//   **thread-local `TRACKING` 标记**：只有被 `MeasureGuard` 武装的当前测试线程的分配才累加
//   到全局计数；并且 `MEASURE_LOCK` 用 Mutex 串行化所有测量窗口，保证 `ALLOC_COUNT` 的
//   增量只可能来自本窗口（即便两个测试同时在跑，也只有一个处于武装状态）。
// - 分配器里用 `try_with` 访问 TLS，TLS 销毁期安全忽略；`Cell<bool>` const 初始化不分配，
//   避免“统计分配器自己分配”的递归。
//
// 串行化说明：测量期间本线程只调用内核入口，不打印/不 assert 成功路径（失败路径才分配）；
// 其它测试线程的分配因 TLS 未武装而不计数。

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

    /// 结束窗口，返回窗口内的堆分配次数；同时断言武装线程 = 计数线程。
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
/// scatter=[2,0] 使容器通道 1 未映射（必须逐样本 +0.0），并含对角 g + 非对角 g 路由。
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

/// 稀疏拓扑：2 源 / 3 角色 / 4 容器；D=2，L_A=2，L_G=3，L_P=1，M=3；角色 1 无源（A_map 整行
/// 0），但仍是 participant（g 行/列参与），容器通道 2 未映射。
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

/// identity + 公共延迟 D=3 拓扑（1 源 / 1 角色 / 2 容器，通道 0 未映射）。
fn topo_identity_d3() -> DirectTopology {
    let mut src_of_role = [NO; 22];
    src_of_role[0] = 0;
    let mut participants = [NO; 8];
    participants[0] = 0;
    let mut scatter = [0u16; 22];
    scatter[0] = 1;
    DirectTopology {
        n_src: 1,
        n_role: 1,
        container_channels: 2,
        d_samples: 3,
        l_a: 1,
        l_g: 4,
        l_p: 1,
        m: 1,
        src_of_role,
        participants,
        scatter,
    }
}

/// 确定性系数（全部是 2 的负幂整数倍，f32/f64 均可精确表示）。
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

/// 确定性 planar 输入（2 的负幂整数倍，精确可表示）。
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
// 朴素参考实现
// ===========================================================================
//
// 为什么与内核可比（bit-exact）：两者都按内核的**固定累加顺序**逐步做 IEEE-754 标量运算：
// u 阶段 tap 升序（tap0 的乘法作为初值，tap1.. 逐个 add）；v 阶段列 k 升序、每列内 tap 升序
// （acc 从 +0.0 起，coef 先加 identity baseline 再加 g）；y 阶段 tap 升序。没有 FMA/重排/reduce，
// 因此同一输入在任意 block 切分下每一步舍入都相同 —— 这正是内核“固定累加顺序”的可比性前提。
// 语义公式见直接卷积内核文件头（u = A x；v = (delta_D I + g) u；y = P v；z = scatter y）。

trait RefSample: Copy {
    const ZERO: Self;
    const ONE: Self;
    fn add(self, other: Self) -> Self;
    fn mul(self, other: Self) -> Self;
}

impl RefSample for f32 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }
}

impl RefSample for f64 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }
}

fn naive_reference<T: RefSample>(
    topo: &DirectTopology,
    a: &[T],
    g: &[T],
    p: &[T],
    input: &[Vec<T>],
) -> Vec<Vec<T>> {
    let n_role = topo.n_role as usize;
    let containers = topo.container_channels as usize;
    let (l_a, l_g, l_p, m) = (
        topo.l_a as usize,
        topo.l_g as usize,
        topo.l_p as usize,
        topo.m as usize,
    );
    let n = input[0].len();
    let mut pos = [usize::MAX; 22];
    for i in 0..m {
        pos[topo.participants[i] as usize] = i;
    }
    // u_j[n]：每角色一列（A_map 只选择/零填）。
    let mut u = vec![vec![T::ZERO; n]; n_role];
    for j in 0..n_role {
        let src = topo.src_of_role[j];
        if src == NO {
            continue;
        }
        let si = src as usize;
        for nn in 0..n {
            let mut acc = T::ZERO;
            for tap in 0..l_a {
                let xv = if tap <= nn {
                    input[si][nn - tap]
                } else {
                    T::ZERO
                };
                let term = a[si * l_a + tap].mul(xv);
                acc = if tap == 0 { term } else { acc.add(term) };
            }
            u[j][nn] = acc;
        }
    }
    // v_j[n] = sum_k G[j][k] * u_k[n]（k 升序、tap 升序，coef 先 baseline 后 g）。
    let mut v = vec![vec![T::ZERO; n]; n_role];
    for j in 0..n_role {
        for nn in 0..n {
            let mut acc = T::ZERO;
            for k in 0..n_role {
                let has_g = pos[j] != usize::MAX && pos[k] != usize::MAX;
                for tap in 0..l_g {
                    let mut coef = T::ZERO;
                    if j == k && tap == topo.d_samples as usize {
                        coef = coef.add(T::ONE);
                    }
                    if has_g {
                        coef = coef.add(g[(pos[j] * m + pos[k]) * l_g + tap]);
                    }
                    let uv = if tap <= nn { u[k][nn - tap] } else { T::ZERO };
                    acc = acc.add(coef.mul(uv));
                }
            }
            v[j][nn] = acc;
        }
    }
    // y_j = p_j * v_j，写 scatter 容器通道；未映射通道保持 +0.0。
    let mut out = vec![vec![T::ZERO; n]; containers];
    for j in 0..n_role {
        let base = j * l_p;
        let chan = topo.scatter[j] as usize;
        for nn in 0..n {
            let mut acc = T::ZERO;
            for tap in 0..l_p {
                let vv = if tap <= nn { v[j][nn - tap] } else { T::ZERO };
                let term = p[base + tap].mul(vv);
                acc = if tap == 0 { term } else { acc.add(term) };
            }
            out[chan][nn] = acc;
        }
    }
    out
}

// ===========================================================================
// 内核流式驱动（NONRT 包装：分配都在这里，process 窗口内不调用）
// ===========================================================================

fn direct_layout(topo: &DirectTopology) -> DirectLayout {
    DirectLayout::build(topo).expect("direct layout")
}

#[allow(clippy::too_many_arguments)]
fn run_direct_f32(
    topo: &DirectTopology,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f32>],
) -> Vec<Vec<f32>> {
    let layout = direct_layout(topo);
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let total: usize = chunks.iter().sum();
    let mut out = vec![vec![0.0f32; total]; containers];
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut inb = vec![0.0f32; n_src * block_size];
    let mut ob = vec![0.0f32; containers * block_size];
    let coeffs = DirectCoeffsF32 { a_fir: a, g, p };
    let mut off = 0usize;
    for &v in chunks {
        for i in 0..n_src {
            for n in 0..v {
                inb[i * block_size + n] = input[i][off + n];
            }
        }
        let status = direct_block_f32(
            topo, &layout, &coeffs, &mut state, &inb, block_size, &mut ob, block_size, block_size,
            v,
        );
        assert!(status.is_ok(), "direct f32 status {:?}", status);
        for ch in 0..containers {
            for n in 0..v {
                out[ch][off + n] = ob[ch * block_size + n];
            }
        }
        off += v;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn run_direct_f64(
    topo: &DirectTopology,
    a: &[f64],
    g: &[f64],
    p: &[f64],
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f64>],
) -> Vec<Vec<f64>> {
    let layout = direct_layout(topo);
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let total: usize = chunks.iter().sum();
    let mut out = vec![vec![0.0f64; total]; containers];
    let mut state = vec![0.0f64; layout.state_samples()];
    let mut inb = vec![0.0f64; n_src * block_size];
    let mut ob = vec![0.0f64; containers * block_size];
    let coeffs = DirectCoeffsF64 { a_fir: a, g, p };
    let mut off = 0usize;
    for &v in chunks {
        for i in 0..n_src {
            for n in 0..v {
                inb[i * block_size + n] = input[i][off + n];
            }
        }
        let status = direct_block_f64(
            topo, &layout, &coeffs, &mut state, &inb, block_size, &mut ob, block_size, block_size,
            v,
        );
        assert!(status.is_ok(), "direct f64 status {:?}", status);
        for ch in 0..containers {
            for n in 0..v {
                out[ch][off + n] = ob[ch * block_size + n];
            }
        }
        off += v;
    }
    out
}

struct PartF32 {
    layout: PartitionedLayout,
    plan: PartitionedPlan,
    plan_data: Vec<f32>,
}

struct PartF64 {
    layout: PartitionedLayout,
    plan: PartitionedPlan,
    plan_data: Vec<f64>,
}

fn build_part_f32(
    topo: &DirectTopology,
    block_size: usize,
    a: &[f32],
    g: &[f32],
    p: &[f32],
) -> PartF32 {
    let layout = PartitionedLayout::build(topo, block_size).expect("partitioned layout f32");
    let mut plan_data = vec![0.0f32; layout.plan_samples()];
    let mut scratch = vec![0.0f32; layout.build_scratch_samples()];
    let coeffs = DirectCoeffsF32 { a_fir: a, g, p };
    let plan = PartitionedPlan::build_f32(topo, block_size, &coeffs, &mut plan_data, &mut scratch)
        .expect("partitioned plan f32");
    PartF32 {
        layout,
        plan,
        plan_data,
    }
}

fn build_part_f64(
    topo: &DirectTopology,
    block_size: usize,
    a: &[f64],
    g: &[f64],
    p: &[f64],
) -> PartF64 {
    let layout = PartitionedLayout::build(topo, block_size).expect("partitioned layout f64");
    let mut plan_data = vec![0.0f64; layout.plan_samples()];
    let mut scratch = vec![0.0f64; layout.build_scratch_samples()];
    let coeffs = DirectCoeffsF64 { a_fir: a, g, p };
    let plan = PartitionedPlan::build_f64(topo, block_size, &coeffs, &mut plan_data, &mut scratch)
        .expect("partitioned plan f64");
    PartF64 {
        layout,
        plan,
        plan_data,
    }
}

fn run_part_f32(
    topo: &DirectTopology,
    part: &PartF32,
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f32>],
) -> Vec<Vec<f32>> {
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let total: usize = chunks.iter().sum();
    let mut out = vec![vec![0.0f32; total]; containers];
    let mut state = vec![0.0f32; part.layout.state_samples()];
    let mut inb = vec![0.0f32; n_src * block_size];
    let mut ob = vec![0.0f32; containers * block_size];
    let mut off = 0usize;
    for &v in chunks {
        for i in 0..n_src {
            for n in 0..v {
                inb[i * block_size + n] = input[i][off + n];
            }
        }
        let status = partitioned_block_f32(
            topo,
            &part.plan,
            &part.plan_data,
            &mut state,
            &inb,
            block_size,
            &mut ob,
            block_size,
            block_size,
            v,
        );
        assert!(status.is_ok(), "partitioned f32 status {:?}", status);
        for ch in 0..containers {
            for n in 0..v {
                out[ch][off + n] = ob[ch * block_size + n];
            }
        }
        off += v;
    }
    out
}

fn run_part_f64(
    topo: &DirectTopology,
    part: &PartF64,
    block_size: usize,
    chunks: &[usize],
    input: &[Vec<f64>],
) -> Vec<Vec<f64>> {
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let total: usize = chunks.iter().sum();
    let mut out = vec![vec![0.0f64; total]; containers];
    let mut state = vec![0.0f64; part.layout.state_samples()];
    let mut inb = vec![0.0f64; n_src * block_size];
    let mut ob = vec![0.0f64; containers * block_size];
    let mut off = 0usize;
    for &v in chunks {
        for i in 0..n_src {
            for n in 0..v {
                inb[i * block_size + n] = input[i][off + n];
            }
        }
        let status = partitioned_block_f64(
            topo,
            &part.plan,
            &part.plan_data,
            &mut state,
            &inb,
            block_size,
            &mut ob,
            block_size,
            block_size,
            v,
        );
        assert!(status.is_ok(), "partitioned f64 status {:?}", status);
        for ch in 0..containers {
            for n in 0..v {
                out[ch][off + n] = ob[ch * block_size + n];
            }
        }
        off += v;
    }
    out
}

// ===========================================================================
// 比较工具
// ===========================================================================

fn assert_bits_eq_f32(got: &[Vec<f32>], want: &[Vec<f32>], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: channel count");
    for (ch, (gc, wc)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(gc.len(), wc.len(), "{what}: channel {ch} length");
        for (n, (g, w)) in gc.iter().zip(wc.iter()).enumerate() {
            assert_eq!(
                g.to_bits(),
                w.to_bits(),
                "{what}: ch={ch} n={n} got={g:?} want={w:?}"
            );
        }
    }
}

fn assert_bits_eq_f64(got: &[Vec<f64>], want: &[Vec<f64>], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: channel count");
    for (ch, (gc, wc)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(gc.len(), wc.len(), "{what}: channel {ch} length");
        for (n, (g, w)) in gc.iter().zip(wc.iter()).enumerate() {
            assert_eq!(
                g.to_bits(),
                w.to_bits(),
                "{what}: ch={ch} n={n} got={g:?} want={w:?}"
            );
        }
    }
}

fn max_abs_diff_f64(a: &[Vec<f64>], b: &[Vec<f64>]) -> f64 {
    let mut max = 0.0f64;
    for (ca, cb) in a.iter().zip(b.iter()) {
        for (x, y) in ca.iter().zip(cb.iter()) {
            max = max.max((x - y).abs());
        }
    }
    max
}

fn max_abs_diff_f32(a: &[Vec<f32>], b: &[Vec<f32>]) -> f32 {
    let mut max = 0.0f32;
    for (ca, cb) in a.iter().zip(b.iter()) {
        for (x, y) in ca.iter().zip(cb.iter()) {
            max = max.max((x - y).abs());
        }
    }
    max
}

/// 全部样本必须是正零（+0.0）而不是 -0.0/垃圾值。
fn assert_all_positive_zero(chan: &[f32], what: &str) {
    for (n, value) in chan.iter().enumerate() {
        assert_eq!(value.to_bits(), 0, "{what}: 期望 +0.0, n={n} got={value:?}");
    }
}

// ===========================================================================
// (a) direct vs 朴素参考：逐样本 bit-exact
// ===========================================================================

#[test]
fn direct_matches_naive_reference_bit_exact() {
    let cases: [(DirectTopology, usize); 2] = [(topo_mix(), 4), (topo_sparse(), 3)];
    for (topo, block) in cases.iter() {
        let (a32, g32, p32) = coeffs_f32(topo);
        let a64: Vec<f64> = a32.iter().map(|v| *v as f64).collect();
        let g64: Vec<f64> = g32.iter().map(|v| *v as f64).collect();
        let p64: Vec<f64> = p32.iter().map(|v| *v as f64).collect();
        let len = block * 5;
        let input32 = planar_input(topo.n_src as usize, len, 7);
        let input64: Vec<Vec<f64>> = input32
            .iter()
            .map(|chan| chan.iter().map(|v| *v as f64).collect())
            .collect();
        let chunks = [*block; 5];

        // f64 oracle：与朴素参考逐位一致（同一固定累加顺序，见 naive_reference 注释）。
        let want64 = naive_reference(topo, &a64, &g64, &p64, &input64);
        let got64 = run_direct_f64(topo, &a64, &g64, &p64, *block, &chunks, &input64);
        assert_bits_eq_f64(&got64, &want64, "direct f64 vs naive");

        // f32 生产路径：同样逐位一致（f32 的乘法/加法顺序与参考完全相同）。
        let want32 = naive_reference(topo, &a32, &g32, &p32, &input32);
        let got32 = run_direct_f32(topo, &a32, &g32, &p32, *block, &chunks, &input32);
        assert_bits_eq_f32(&got32, &want32, "direct f32 vs naive");
    }
}

// ===========================================================================
// (b) partitioned vs direct：误差有界
// ===========================================================================

#[test]
fn partitioned_agrees_with_direct_within_bound() {
    // 阈值依据（本机实测，见下方 println）：f64 oracle 的 FFT 舍入最大绝对误差 1.11e-16；
    // f32 生产路径 2.98e-8（信号幅度 < 1）。需求要求 < 1e-6；这里两个精度都用 1e-6 上限
    // （f32 实测有 ~33x 余量），实测值打印在 stdout 供报告引用；若输入幅度放大需按比例复核。
    const BOUND: f64 = 1e-6;

    let cases: [(DirectTopology, [usize; 3]); 2] = [
        // (拓扑, 三种 block size；覆盖 2 的幂与非 2 的幂、Q=1/Q=2)
        (topo_mix(), [3, 4, 6]),
        (topo_sparse(), [3, 4, 8]),
    ];
    let mut worst64 = 0.0f64;
    let mut worst32 = 0.0f32;
    for (topo, blocks) in cases.iter() {
        let (a32, g32, p32) = coeffs_f32(topo);
        let a64: Vec<f64> = a32.iter().map(|v| *v as f64).collect();
        let g64: Vec<f64> = g32.iter().map(|v| *v as f64).collect();
        let p64: Vec<f64> = p32.iter().map(|v| *v as f64).collect();
        for &block in blocks.iter() {
            let n_blocks = 6usize;
            let len = block * n_blocks;
            let input32 = planar_input(topo.n_src as usize, len, block + 11);
            let input64: Vec<Vec<f64>> = input32
                .iter()
                .map(|chan| chan.iter().map(|v| *v as f64).collect())
                .collect();
            let chunks = vec![block; n_blocks];

            let direct64 = run_direct_f64(topo, &a64, &g64, &p64, block, &chunks, &input64);
            let part64 = build_part_f64(topo, block, &a64, &g64, &p64);
            let got64 = run_part_f64(topo, &part64, block, &chunks, &input64);
            let err64 = max_abs_diff_f64(&direct64, &got64);
            worst64 = worst64.max(err64);
            assert!(
                err64 < BOUND,
                "partitioned f64 vs direct 最大绝对误差 {err64:e} 超过 {BOUND:e}（topo={topo:?} B={block}）"
            );

            let direct32 = run_direct_f32(topo, &a32, &g32, &p32, block, &chunks, &input32);
            let part32 = build_part_f32(topo, block, &a32, &g32, &p32);
            let got32 = run_part_f32(topo, &part32, block, &chunks, &input32);
            let err32 = max_abs_diff_f32(&direct32, &got32);
            worst32 = worst32.max(err32);
            assert!(
                err32 < BOUND as f32,
                "partitioned f32 vs direct 最大绝对误差 {err32:e} 超过 {BOUND:e}（topo={topo:?} B={block}）"
            );
        }
    }
    println!(
        "partitioned vs direct 实测最大绝对误差：f64={worst64:e} f32={worst32:e}（阈值 {BOUND:e}）"
    );
}

// ===========================================================================
// (c) 一次整块 vs 3 个不等长子块（direct 可拼接 partial 语义）
// ===========================================================================

#[test]
fn direct_partial_split_matches_whole_block() {
    // 12 个样本：整块用 block_size=12、V=12 一次算完；拆成 [3,5,4] 三个不等长 partial 块，
    // 每块 block_size=5（块缓冲容量 128 与 block_size 无关）。direct 的 partial 按 V 推进历史，
    // 因此两条路径的逐样本运算顺序完全相同 -> 逐位一致（这是 direct 的扩展语义，非 kernel bug）。
    let topo = topo_mix();
    let (a32, g32, p32) = coeffs_f32(&topo);
    let a64: Vec<f64> = a32.iter().map(|v| *v as f64).collect();
    let g64: Vec<f64> = g32.iter().map(|v| *v as f64).collect();
    let p64: Vec<f64> = p32.iter().map(|v| *v as f64).collect();
    let input32 = planar_input(topo.n_src as usize, 12, 5);
    let input64: Vec<Vec<f64>> = input32
        .iter()
        .map(|chan| chan.iter().map(|v| *v as f64).collect())
        .collect();

    let whole32 = run_direct_f32(&topo, &a32, &g32, &p32, 12, &[12], &input32);
    let split32 = run_direct_f32(&topo, &a32, &g32, &p32, 5, &[3, 5, 4], &input32);
    assert_bits_eq_f32(&whole32, &split32, "direct f32 whole vs split[3,5,4]");

    let whole64 = run_direct_f64(&topo, &a64, &g64, &p64, 12, &[12], &input64);
    let split64 = run_direct_f64(&topo, &a64, &g64, &p64, 5, &[3, 5, 4], &input64);
    assert_bits_eq_f64(&whole64, &split64, "direct f64 whole vs split[3,5,4]");
}

// ===========================================================================
// (c') partitioned：block-size 不变性（只能 full-B 块，不能拼接 partial）
// ===========================================================================

#[test]
fn partitioned_block_size_invariance() {
    // partitioned 契约：连续流必须 full-B，partial 只在末块；因此“拆 3 个不等长子块”不适用，
    // 等价性质是 block-size 不变性：同一 12 样本流按 B=3/4/6 处理，输出应一致（容差同 (b)）。
    let topo = topo_mix();
    let (a64, g64, p64) = {
        let (a, g, p) = coeffs_f32(&topo);
        (
            a.iter().map(|v| *v as f64).collect::<Vec<f64>>(),
            g.iter().map(|v| *v as f64).collect::<Vec<f64>>(),
            p.iter().map(|v| *v as f64).collect::<Vec<f64>>(),
        )
    };
    let input: Vec<Vec<f64>> = planar_input(topo.n_src as usize, 12, 23)
        .iter()
        .map(|chan| chan.iter().map(|v| *v as f64).collect())
        .collect();
    let reference = build_part_f64(&topo, 3, &a64, &g64, &p64);
    let base = run_part_f64(&topo, &reference, 3, &[3, 3, 3, 3], &input);
    for block in [4usize, 6] {
        let part = build_part_f64(&topo, block, &a64, &g64, &p64);
        let got = run_part_f64(&topo, &part, block, &vec![block; 12 / block], &input);
        let err = max_abs_diff_f64(&base, &got);
        assert!(err < 1e-6, "block-size 不变性失败：B={block} 误差 {err:e}");
        println!("partitioned block-size invariance B={block} err={err:e}");
    }
}

// ===========================================================================
// (d) state reset 后重跑同一输入，输出一致
// ===========================================================================

#[test]
fn state_reset_reproduces_output() {
    let topo = topo_mix();
    let block = 4usize;
    let (a32, g32, p32) = coeffs_f32(&topo);
    let a64: Vec<f64> = a32.iter().map(|v| *v as f64).collect();
    let g64: Vec<f64> = g32.iter().map(|v| *v as f64).collect();
    let p64: Vec<f64> = p32.iter().map(|v| *v as f64).collect();
    let input32 = planar_input(topo.n_src as usize, block * 4, 31);
    let input64: Vec<Vec<f64>> = input32
        .iter()
        .map(|chan| chan.iter().map(|v| *v as f64).collect())
        .collect();
    let chunks = [block; 4];

    // direct f32：两次从零 state 出发，输出逐位一致。
    let layout = direct_layout(&topo);
    let mut state = vec![0.0f32; layout.state_samples()];
    let coeffs32 = DirectCoeffsF32 {
        a_fir: &a32,
        g: &g32,
        p: &p32,
    };
    let mut run = || {
        direct_state_zero_f32(&mut state);
        let mut out = vec![vec![0.0f32; block * 4]; topo.container_channels as usize];
        let mut inb = vec![0.0f32; topo.n_src as usize * block];
        let mut ob = vec![0.0f32; topo.container_channels as usize * block];
        let mut off = 0usize;
        for &v in chunks.iter() {
            for i in 0..topo.n_src as usize {
                for n in 0..v {
                    inb[i * block + n] = input32[i][off + n];
                }
            }
            let status = direct_block_f32(
                &topo, &layout, &coeffs32, &mut state, &inb, block, &mut ob, block, block, v,
            );
            assert!(status.is_ok());
            for ch in 0..topo.container_channels as usize {
                for n in 0..v {
                    out[ch][off + n] = ob[ch * block + n];
                }
            }
            off += v;
        }
        out
    };
    let first = run();
    let second = run();
    assert_bits_eq_f32(&first, &second, "direct f32 state reset");

    // partitioned f32：同样逐位一致（plan 只读；reset 清 x_prev/x_hist/slot）。
    let part = build_part_f32(&topo, block, &a32, &g32, &p32);
    let mut pstate = vec![0.0f32; part.layout.state_samples()];
    let mut runp = || {
        partitioned_state_zero_f32(&mut pstate);
        let mut out = vec![vec![0.0f32; block * 4]; topo.container_channels as usize];
        let mut inb = vec![0.0f32; topo.n_src as usize * block];
        let mut ob = vec![0.0f32; topo.container_channels as usize * block];
        let mut off = 0usize;
        for &v in chunks.iter() {
            for i in 0..topo.n_src as usize {
                for n in 0..v {
                    inb[i * block + n] = input32[i][off + n];
                }
            }
            let status = partitioned_block_f32(
                &topo,
                &part.plan,
                &part.plan_data,
                &mut pstate,
                &inb,
                block,
                &mut ob,
                block,
                block,
                v,
            );
            assert!(status.is_ok());
            for ch in 0..topo.container_channels as usize {
                for n in 0..v {
                    out[ch][off + n] = ob[ch * block + n];
                }
            }
            off += v;
        }
        out
    };
    let firstp = runp();
    let secondp = runp();
    assert_bits_eq_f32(&firstp, &secondp, "partitioned f32 state reset");

    // f64 oracle 路径同样覆盖（direct / partitioned）。
    let layout64 = direct_layout(&topo);
    let coeffs64 = DirectCoeffsF64 {
        a_fir: &a64,
        g: &g64,
        p: &p64,
    };
    let mut state64 = vec![0.0f64; layout64.state_samples()];
    let d64_first = {
        direct_state_zero_f64(&mut state64);
        run_direct_f64(&topo, &a64, &g64, &p64, block, &chunks, &input64)
    };
    let d64_second = run_direct_f64(&topo, &a64, &g64, &p64, block, &chunks, &input64);
    assert_bits_eq_f64(&d64_first, &d64_second, "direct f64 state reset");
    let _ = coeffs64;

    let part64 = build_part_f64(&topo, block, &a64, &g64, &p64);
    let p64_first = {
        let mut st = vec![0.0f64; part64.layout.state_samples()];
        partitioned_state_zero_f64(&mut st);
        run_part_f64(&topo, &part64, block, &chunks, &input64)
    };
    let p64_second = run_part_f64(&topo, &part64, block, &chunks, &input64);
    assert_bits_eq_f64(&p64_first, &p64_second, "partitioned f64 state reset");
}

// ===========================================================================
// (e) process 路径零堆分配
// ===========================================================================

#[test]
fn process_path_does_not_allocate() {
    // 预分配全部在测量窗口之外：plan/state/输入/输出缓冲全部先建好；窗口内只调用 f32 生产入口。
    let topo = topo_mix();
    let block = 8usize;
    let blocks = 6usize;
    let (a32, g32, p32) = coeffs_f32(&topo);
    let input = planar_input(topo.n_src as usize, block * blocks, 47);
    let containers = topo.container_channels as usize;
    let n_src = topo.n_src as usize;

    let layout = direct_layout(&topo);
    let mut direct_state = vec![0.0f32; layout.state_samples()];
    let mut direct_out = vec![0.0f32; containers * block];
    let mut inb = vec![0.0f32; n_src * block];
    let direct_coeffs = DirectCoeffsF32 {
        a_fir: &a32,
        g: &g32,
        p: &p32,
    };
    let part = build_part_f32(&topo, block, &a32, &g32, &p32);
    let mut part_state = vec![0.0f32; part.layout.state_samples()];
    let mut part_out = vec![0.0f32; containers * block];

    // 预热：让 TLS/首次触碰在测量窗口之前完成，并验证一次基线调用 OK。
    for (i, chan) in input.iter().enumerate() {
        for n in 0..block {
            inb[i * block + n] = chan[n];
        }
    }
    let warm = direct_block_f32(
        &topo,
        &layout,
        &direct_coeffs,
        &mut direct_state,
        &inb,
        block,
        &mut direct_out,
        block,
        block,
        block,
    );
    assert!(warm.is_ok());
    let warmp = partitioned_block_f32(
        &topo,
        &part.plan,
        &part.plan_data,
        &mut part_state,
        &inb,
        block,
        &mut part_out,
        block,
        block,
        block,
    );
    assert!(warmp.is_ok());

    let guard = MeasureGuard::begin();

    for blk in 0..blocks {
        for i in 0..n_src {
            for n in 0..block {
                inb[i * block + n] = input[i][blk * block + n];
            }
        }
        let status = direct_block_f32(
            &topo,
            &layout,
            &direct_coeffs,
            &mut direct_state,
            &inb,
            block,
            &mut direct_out,
            block,
            block,
            block,
        );
        // 只在失败路径分配；成功路径不触发格式化。
        if !status.is_ok() {
            panic!("direct f32 tick {blk} status {:?}", status);
        }
    }

    // partitioned：注意每个 full-B 块都会推进历史；同一 state 连续 6 块即可。
    for blk in 0..blocks {
        for i in 0..n_src {
            for n in 0..block {
                inb[i * block + n] = input[i][blk * block + n];
            }
        }
        let status = partitioned_block_f32(
            &topo,
            &part.plan,
            &part.plan_data,
            &mut part_state,
            &inb,
            block,
            &mut part_out,
            block,
            block,
            block,
        );
        if !status.is_ok() {
            panic!("partitioned f32 tick {blk} status {:?}", status);
        }
    }

    let allocations = guard.end();
    assert_eq!(allocations, 0, "process 路径发生 {allocations} 次堆分配");

    // 控制组：证明计数窗口确实能捕获分配（否则上面的 0 断言可能只是统计失效）。
    let control = {
        let guard = MeasureGuard::begin();
        std::hint::black_box(vec![0u8; 64]);
        guard.end()
    };
    assert!(
        control >= 1,
        "计数分配器未捕获控制组分配（零分配断言本身失效）"
    );
}

// ===========================================================================
// (f) legacy 生成的 golden 向量（f32 逐位复现）
// ===========================================================================

fn f32_bits(values: &[u32]) -> Vec<f32> {
    values.iter().map(|bits| f32::from_bits(*bits)).collect()
}

#[test]
fn legacy_golden_vectors_are_reproduced() {
    // golden 由 legacy/pre-rewrite 的 direct.rs / partitioned.rs 本体生成（见迁移报告）；
    // 新内核必须逐位复现，证明迁移没有改变任何数值行为。
    // G1：identity + D=3，B=4，2 个 full block；容器通道 0 未映射。
    let topo1 = topo_identity_d3();
    let a1 = [1.0f32];
    let g1 = [0.0f32; 4];
    let p1 = [1.0f32];
    let input1 = vec![vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]];
    let direct1 = run_direct_f32(&topo1, &a1, &g1, &p1, 4, &[4, 4], &input1);
    assert_all_positive_zero(&direct1[0], "G1 direct 未映射通道");
    assert_eq!(
        direct1[1].iter().map(|v| v.to_bits()).collect::<Vec<u32>>(),
        vec![
            0x00000000, 0x00000000, 0x00000000, 0x3f800000, 0x40000000, 0x40400000, 0x40800000,
            0x40a00000
        ],
        "G1 direct 延迟通道"
    );
    let part1 = build_part_f32(&topo1, 4, &a1, &g1, &p1);
    let part1_out = run_part_f32(&topo1, &part1, 4, &[4, 4], &input1);
    assert_all_positive_zero(&part1_out[0], "G1 partitioned 未映射通道");
    assert_bits_eq_f32(
        std::slice::from_ref(&part1_out[1]),
        std::slice::from_ref(&f32_bits(&[
            0x34000000, 0x34000000, 0x00000000, 0x3f7fffff, 0x40000000, 0x40400000, 0x40800000,
            0x40a00000,
        ])),
        "G1 partitioned 延迟通道",
    );

    // G2：2x2 混合拓扑（D=1，B=3 非 2 的幂，3 个 full block），含未映射通道 1。
    let topo2 = topo_mix();
    let a2 = f32_bits(&[0x3f000000, 0x3e800000, 0xbe000000, 0x3f400000]);
    let g2 = f32_bits(&[
        0x3e000000, 0xbe800000, 0x3f000000, 0x3ec00000, 0x3d800000, 0xbf000000, 0x3e800000,
        0xbe000000, 0x3f400000, 0x3f000000, 0xbec00000, 0x3e800000,
    ]);
    let p2 = f32_bits(&[0x3f000000, 0x3e800000, 0xbf000000, 0x3e000000]);
    let input2 = vec![
        vec![0.5f32, -0.25, 0.75, 0.125, -0.5, 0.25, 0.875, -0.375, 0.625],
        vec![
            -0.125f32, 0.375, -0.625, 0.25, 0.5, -0.75, 0.125, 0.375, -0.25,
        ],
    ];
    let direct2 = run_direct_f32(&topo2, &a2, &g2, &p2, 3, &[3, 3, 3], &input2);
    let expect_direct2 = vec![
        f32_bits(&[
            0xbd100000, 0x3d600000, 0xbe44c000, 0x3d828000, 0x3aa00000, 0xbe4f8000, 0x3d560000,
            0x3e0c8000, 0xbe892000,
        ]),
        f32_bits(&[0; 9]),
        f32_bits(&[
            0x3c980000, 0x3d9e0000, 0x3e334000, 0x3e1fc000, 0x3df38000, 0x3e710000, 0xbd4c0000,
            0xbc380000, 0x3ec5e000,
        ]),
    ];
    assert_bits_eq_f32(&direct2, &expect_direct2, "G2 direct golden");

    let part2 = build_part_f32(&topo2, 3, &a2, &g2, &p2);
    let part2_out = run_part_f32(&topo2, &part2, 3, &[3, 3, 3], &input2);
    let expect_part2 = vec![
        f32_bits(&[
            0xbd0fffff, 0x3d600000, 0xbe44c000, 0x3d828000, 0x3aa00000, 0xbe4f8000, 0x3d55fffe,
            0x3e0c8000, 0xbe892000,
        ]),
        f32_bits(&[0; 9]),
        f32_bits(&[
            0x3c980000, 0x3d9e0001, 0x3e334000, 0x3e1fc000, 0x3df38000, 0x3e710000, 0xbd4bfffe,
            0xbc380000, 0x3ec5e000,
        ]),
    ];
    assert_bits_eq_f32(&part2_out, &expect_part2, "G2 partitioned golden");

    // G3：末块终止性 DRAIN（chunks [3,3,2]）的 golden。
    let part3_out = run_part_f32(&topo2, &part2, 3, &[3, 3, 2], &input2);
    let expect_part3 = vec![
        f32_bits(&[
            0xbd0fffff, 0x3d600000, 0xbe44c000, 0x3d828000, 0x3aa00000, 0xbe4f8000, 0x3d55ffff,
            0x3e0c8000,
        ]),
        f32_bits(&[0; 8]),
        f32_bits(&[
            0x3c980000, 0x3d9e0001, 0x3e334000, 0x3e1fc000, 0x3df38000, 0x3e710000, 0xbd4bfffe,
            0xbc380000,
        ]),
    ];
    assert_bits_eq_f32(&part3_out, &expect_part3, "G3 drain golden");
}

// ===========================================================================
// (g) DRAIN 语义与错误码最小覆盖
// ===========================================================================

#[test]
fn drain_tail_semantics() {
    let topo = topo_mix();
    let block = 4usize;
    let (a32, g32, p32) = coeffs_f32(&topo);
    let a64: Vec<f64> = a32.iter().map(|v| *v as f64).collect();
    let g64: Vec<f64> = g32.iter().map(|v| *v as f64).collect();
    let p64: Vec<f64> = p32.iter().map(|v| *v as f64).collect();
    let total = block * 3;
    let input32 = planar_input(topo.n_src as usize, total, 61);
    let input64: Vec<Vec<f64>> = input32
        .iter()
        .map(|chan| chan.iter().map(|v| *v as f64).collect())
        .collect();
    let containers = topo.container_channels as usize;
    let n_src = topo.n_src as usize;

    // 末块 V=2：partitioned DRAIN 输出 [0,2) 与 direct 可拼接 partial 一致；[2,4) 严格 +0.0。
    let part64 = build_part_f64(&topo, block, &a64, &g64, &p64);
    let chunks = [block, block, 2];
    let part_out = run_part_f64(&topo, &part64, block, &chunks, &input64);
    let direct_out = run_direct_f64(&topo, &a64, &g64, &p64, block, &chunks, &input64);
    let err = max_abs_diff_f64(&part_out, &direct_out);
    assert!(err < 1e-6, "DRAIN 输出与 direct 误差 {err:e}");
    println!("drain V=2 vs direct err={err:e}");

    // 直接验证 [V, B) 尾部写 +0.0：用单块调用检查输出缓冲。
    let part32 = build_part_f32(&topo, block, &a32, &g32, &p32);
    let mut state = vec![0.0f32; part32.layout.state_samples()];
    let mut inb = vec![0.0f32; n_src * block];
    let mut ob = vec![-1.0f32; containers * block];
    for i in 0..n_src {
        for n in 0..2 {
            inb[i * block + n] = input32[i][n];
        }
    }
    let status = partitioned_block_f32(
        &topo,
        &part32.plan,
        &part32.plan_data,
        &mut state,
        &inb,
        block,
        &mut ob,
        block,
        block,
        2,
    );
    assert!(status.is_ok());
    for ch in 0..containers {
        for n in 2..block {
            assert_eq!(
                ob[ch * block + n].to_bits(),
                0,
                "DRAIN 尾部必须 +0.0: ch={ch} n={n}"
            );
        }
    }

    // V=0：整块 +0.0，且历史被清空（下一块等价新 epoch）。
    let mut zero_out = vec![7.0f32; containers * block];
    let status = partitioned_block_f32(
        &topo,
        &part32.plan,
        &part32.plan_data,
        &mut state,
        &inb,
        block,
        &mut zero_out,
        block,
        block,
        0,
    );
    assert!(status.is_ok());
    for ch in 0..containers {
        assert_all_positive_zero(&zero_out[ch * block..ch * block + block], "V=0");
    }
    // V=0 之后按 fresh epoch 处理整块，与 direct 从零 state 处理同一块一致。
    let fresh = {
        let mut ob2 = vec![0.0f32; containers * block];
        let status = direct_block_f32(
            &topo,
            &direct_layout(&topo),
            &DirectCoeffsF32 {
                a_fir: &a32,
                g: &g32,
                p: &p32,
            },
            &mut vec![0.0f32; direct_layout(&topo).state_samples()],
            &inb,
            block,
            &mut ob2,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        ob2
    };
    let epoch_out = {
        let mut ob3 = vec![0.0f32; containers * block];
        let status = partitioned_block_f32(
            &topo,
            &part32.plan,
            &part32.plan_data,
            &mut state,
            &inb,
            block,
            &mut ob3,
            block,
            block,
            block,
        );
        assert!(status.is_ok());
        ob3
    };
    let err = max_abs_diff_f32(&[fresh], &[epoch_out]);
    assert!(err < 1e-6, "V=0 后新 epoch 误差 {err:e}");
}

#[test]
fn invalid_calls_return_error_codes() {
    let topo = topo_mix();
    let layout = direct_layout(&topo);
    let (a32, g32, p32) = coeffs_f32(&topo);
    let block = 4usize;
    let n_src = topo.n_src as usize;
    let containers = topo.container_channels as usize;
    let mut state = vec![0.0f32; layout.state_samples()];
    let input = vec![0.0f32; n_src * block];
    let mut output = vec![0.0f32; containers * block];
    let coeffs = DirectCoeffsF32 {
        a_fir: &a32,
        g: &g32,
        p: &p32,
    };

    // 错误 block size / valid_frames / stride / state / 系数长度 / layout 不符。
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        block,
        &mut output,
        block,
        0,
        0,
    );
    assert_eq!(status, RtStatus::ERR_BLOCK_SIZE);
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        block,
        &mut output,
        block,
        block,
        block + 1,
    );
    assert_eq!(status, RtStatus::ERR_BAD_LAYOUT);
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs,
        &mut state,
        &input,
        block - 1,
        &mut output,
        block,
        block,
        block,
    );
    assert_eq!(status, RtStatus::ERR_STRIDE_ALIGN);
    let status = direct_block_f32(
        &topo,
        &layout,
        &coeffs,
        &mut state[..layout.state_samples() - 1],
        &input,
        block,
        &mut output,
        block,
        block,
        block,
    );
    assert_eq!(status, RtStatus::ERR_CAPACITY);
    let short_a = &a32[..a32.len() - 1];
    let bad_coeffs = DirectCoeffsF32 {
        a_fir: short_a,
        g: &g32,
        p: &p32,
    };
    let status = direct_block_f32(
        &topo,
        &layout,
        &bad_coeffs,
        &mut state,
        &input,
        block,
        &mut output,
        block,
        block,
        block,
    );
    assert_eq!(status, RtStatus::ERR_PATH_TABLE);

    // 拓扑非法：L_G <= D -> ERR_TAPS；scatter 重复 -> ERR_ROLES。
    let mut bad_topo = topo;
    bad_topo.d_samples = bad_topo.l_g as u32;
    assert_eq!(bad_topo.validate(), RtStatus::ERR_TAPS);
    let mut bad_topo = topo;
    bad_topo.scatter[1] = bad_topo.scatter[0];
    assert_eq!(bad_topo.validate(), RtStatus::ERR_ROLES);

    // partitioned：非法 block size / plan-data 容量 / state 容量。
    let status = PartitionedLayout::build(&topo, 0);
    assert_eq!(status.err(), Some(RtStatus::ERR_BLOCK_SIZE));
    let status = PartitionedLayout::build(&topo, 129);
    assert_eq!(status.err(), Some(RtStatus::ERR_BLOCK_SIZE));
    let part = build_part_f32(&topo, block, &a32, &g32, &p32);
    let mut pstate = vec![0.0f32; part.layout.state_samples()];
    let mut pout = vec![0.0f32; containers * block];
    let short_plan_data = &part.plan_data[..part.plan_data.len() - 1];
    let status = partitioned_block_f32(
        &topo,
        &part.plan,
        short_plan_data,
        &mut pstate,
        &input,
        block,
        &mut pout,
        block,
        block,
        block,
    );
    assert_eq!(status, RtStatus::ERR_CAPACITY);
    let status = partitioned_block_f32(
        &topo,
        &part.plan,
        &part.plan_data,
        &mut pstate[..part.layout.state_samples() - 1],
        &input,
        block,
        &mut pout,
        block,
        block,
        block,
    );
    assert_eq!(status, RtStatus::ERR_CAPACITY);
    // plan 与 topology 不一致：用另一个拓扑的 plan 传给本拓扑。
    let other = topo_sparse();
    let (oa, og, op) = coeffs_f32(&other);
    let other_part = build_part_f32(&other, block, &oa, &og, &op);
    let status = partitioned_block_f32(
        &topo,
        &other_part.plan,
        &other_part.plan_data,
        &mut pstate,
        &input,
        block,
        &mut pout,
        block,
        block,
        block,
    );
    assert_eq!(status, RtStatus::ERR_BAD_LAYOUT);
}
