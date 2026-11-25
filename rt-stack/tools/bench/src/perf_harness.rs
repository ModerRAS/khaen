//! [NONRT] TSPI RT-first DSP 离线性能工装 —— WBS T29/T46/T30（team task-156）。
//!
//! 目的：在占用板端窗口之前把性能门禁"工装化"：
//! 1. 用已合入的 core kernel/DAG（T07 direct / T08 partitioned / T09 workers）在本机
//!    真实跑 1/2/4 worker 的每块 wall、尾值、per-worker busy/barrier、CV/range；
//! 2. 用 T56 schedule_plan 的静态 hybrid DAG 做 CPU 预算核算（deadline 准入/每 worker
//!    负载/hot-swap/fallback 预算/内存模型），与 DSP-HOST-CPU-BUDGET 的口径逐项对齐；
//! 3. 输出 CSV/JSON 供 evidence 归档；把只能在板端取数的门（RT/IRQ/cyclictest、
//!    A72 真实算力下的 1.3333ms/0.6667ms/3.2x、1h soak）明确标成 NOT_RUN。
//!
//! 纪律（README §1/§3）：本文件不在 Cargo workspace 内（rt-stack 只允许唯一 Cargo 根）。
//! run-bench.ps1 把本文件 + Cargo.toml.template 复制到 %TEMP% 的临时 crate，以 path 依赖
//! 只读引用 rt-stack/crates/core，cargo run --release --offline 执行。不写板端/ALSA/git，
//! 不改任何 crate/manifest/lib.rs。x86 结果是本机参考，不是板端门。
//!
//! 线程/DAG 复刻说明：真实线程循环由 T26 host 组合 worker_begin_block/worker_phase_b*/ack；
//! 本工装用同一批 [RT] phase 函数 + TspiBlockAckV1 phase 位图 + wait_phase_done 有界 spin
//! 复刻 block 边界；master 测"发布 block_start -> 各 worker B1/B2/B3 all-ack"的块完成时间
//! （含 partials 清零等块内串行准备）。共享 arena（state/partials/input/output）用 raw
//! pointer 在 worker 间传递，与真实 host 的固定 arena 同义：phase 之间由 ack 位图同步，
//! 同一 phase 内各 worker 只写 plan 归属的不相交区域。这不是 RT 安全证明，只是离线工装。

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use khaen_core::abi::TspiBlockAckV1;
use khaen_core::compile::FusionForm;
use khaen_core::direct::{
    direct_block_f32, direct_state_zero_f32, DirectCoeffsF32, DirectLayout, DirectTopology,
    DIRECT_MAX_PARTICIPANTS, DIRECT_MAX_ROLES, DIRECT_NO_SOURCE,
};
use khaen_core::partitioned::{
    partitioned_block_f32, partitioned_state_zero_f32, PartitionedLayout, PartitionedPlan,
};
use khaen_core::rt::TBLOCK_128_48K_NS;
use khaen_core::schedule_plan::{
    admit_deadline, build_fallback_schedule, build_schedule, evaluate_fallback, evaluate_hot_swap,
    DeadlineBudgets, FallbackBudgets, HotSwapSpec, KernelForm, PlanProfile,
};
use khaen_core::workers::{
    ack_phase, set_cutoff, wait_phase_done, worker_phase_b1, worker_phase_b2, worker_phase_b3,
    WorkerDagPlan, WorkerPartials, WORKER_ACK_FLAG_VALID,
};

const HARNESS_VERSION: &str = "tspi-perf-harness/1";
const GATE_48K_128_NS: u64 = 1_333_333; // T_block(128) 的 50% 门（DECISION §4 / WBS T30）
const GATE_48K_64_NS: u64 = 666_666; //  T_block(64) 的 50% 门
const MAX_WORKERS: usize = 4;
const SPIN_LIMIT: u32 = 1_000_000_000;
const MASTER_SPIN_LIMIT: u64 = 5_000_000_000;
const SENTINEL: u64 = u64::MAX;

// ---------------------------------------------------------------------------
// CLI / 配置
// ---------------------------------------------------------------------------

struct Config {
    out_dir: PathBuf,
    blocks: usize,
    warmup: usize,
    reps: usize,
    env_json: Option<PathBuf>,
    timestamp: String,
}

fn parse_args() -> Config {
    let mut cfg = Config {
        out_dir: PathBuf::from("."),
        blocks: 64,
        warmup: 8,
        reps: 3,
        env_json: None,
        timestamp: "unknown".to_string(),
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_default();
        match arg.as_str() {
            "--out" => cfg.out_dir = PathBuf::from(value()),
            "--blocks" => cfg.blocks = value().parse().unwrap_or(64),
            "--warmup" => cfg.warmup = value().parse().unwrap_or(8),
            "--reps" => cfg.reps = value().parse().unwrap_or(3),
            "--env-json" => cfg.env_json = Some(PathBuf::from(value())),
            "--timestamp" => cfg.timestamp = value(),
            "--quick" => {
                cfg.blocks = 16;
                cfg.warmup = 4;
                cfg.reps = 1;
            }
            "--help" | "-h" => {
                println!(
                    "usage: perf_harness --out DIR [--blocks N] [--warmup N] [--reps N] \
                     [--env-json FILE] [--timestamp S] [--quick]"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    cfg.blocks = cfg.blocks.max(4);
    cfg.warmup = cfg.warmup.min(cfg.blocks / 2);
    cfg.reps = cfg.reps.max(1);
    cfg
}

// ---------------------------------------------------------------------------
// 统计
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Stats {
    n: usize,
    min_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    p999_ns: u64,
    max_ns: u64,
    mean_ns: u64,
    cv: f64,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn stats_of(values: &[u64]) -> Stats {
    if values.is_empty() {
        return Stats::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let sum: u128 = sorted.iter().map(|&v| v as u128).sum();
    let mean = (sum / n as u128) as u64;
    let mut var = 0.0f64;
    for &v in &sorted {
        let d = v as f64 - mean as f64;
        var += d * d;
    }
    var /= n as f64;
    let std = var.sqrt();
    let cv = if mean == 0 { 0.0 } else { std / mean as f64 };
    Stats {
        n,
        min_ns: sorted[0],
        p50_ns: percentile(&sorted, 0.50),
        p95_ns: percentile(&sorted, 0.95),
        p99_ns: percentile(&sorted, 0.99),
        p999_ns: percentile(&sorted, 0.999),
        max_ns: *sorted.last().unwrap(),
        mean_ns: mean,
        cv,
    }
}

fn fnv1a_f32(mut hash: u64, values: &[f32]) -> u64 {
    for &v in values {
        hash ^= v.to_bits() as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// 图与系数（4.1.2 = 7 角色 / 8 容器 / 5 participants；与 T09/T56 同构）
// ---------------------------------------------------------------------------

struct CoeffData {
    a: Vec<f32>,
    g: Vec<f32>,
    p: Vec<f32>,
}

impl CoeffData {
    fn view(&self) -> DirectCoeffsF32<'_> {
        DirectCoeffsF32 {
            a_fir: &self.a,
            g: &self.g,
            p: &self.p,
        }
    }
}

struct Graph {
    topo: DirectTopology,
    coeffs: CoeffData,
}

struct Lcg(u64);

impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let unit = ((self.0 >> 11) as f64) / ((1u64 << 53) as f64);
        (unit * 2.0 - 1.0) as f32
    }
}

/// 4.1.2 目标布局（FL/FR/SL/SR/HFL/HFR/SUB1，容器 ch6 空，5 participants）。
/// participants 角色 = [0,1,2,3,6]（FL/FR/SL/SR/SUB1），scatter = [0,1,2,3,4,5,7]。
fn topo_4_1_2(l_g: u16, l_p: u16, d: u32) -> DirectTopology {
    let mut topo = DirectTopology {
        n_src: 7,
        n_role: 7,
        container_channels: 8,
        d_samples: d,
        l_a: 1,
        l_g,
        l_p,
        m: 5,
        src_of_role: [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES],
        participants: [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS],
        scatter: [0u16; DIRECT_MAX_ROLES],
    };
    let scatter = [0u16, 1, 2, 3, 4, 5, 7];
    let participants = [0u16, 1, 2, 3, 6];
    let mut j = 0usize;
    while j < 7 {
        topo.src_of_role[j] = j as u16;
        topo.scatter[j] = scatter[j];
        j += 1;
    }
    let mut i = 0usize;
    while i < participants.len() {
        topo.participants[i] = participants[i];
        i += 1;
    }
    topo
}

/// 构造 4.1.2 图：G 为 m×m×l_g 稠密系数（25 条真实路径 + 未参与角色零系数），P 为
/// 每角色 l_p tap 对角 FIR。系数取有界伪随机小值，只为性能占位、不是校准系数。
fn graph_4_1_2(l_g: u16, l_p: u16, d: u32) -> Graph {
    let topo = topo_4_1_2(l_g, l_p, d);
    assert_eq!(topo.validate().0, 0, "topology must validate");
    let mut lcg = Lcg(0x1234_5678_9abc_def0);
    let a_len = topo.n_src as usize * topo.l_a as usize;
    let mut a = vec![0.0f32; a_len];
    for value in a.iter_mut() {
        *value = 1.0 + 0.01 * lcg.next_f32();
    }
    let mut g = vec![0.0f32; topo.m as usize * topo.m as usize * l_g as usize];
    for value in g.iter_mut() {
        *value = 0.002 * lcg.next_f32();
    }
    let mut p = vec![0.0f32; topo.n_role as usize * l_p as usize];
    for role in 0..topo.n_role as usize {
        p[role * l_p as usize] = 1.0;
        for t in 1..l_p as usize {
            p[role * l_p as usize + t] = 0.001 * lcg.next_f32();
        }
    }
    Graph {
        topo,
        coeffs: CoeffData { a, g, p },
    }
}

fn fill_input(n_src: usize, block: usize, seed: u64) -> Vec<f32> {
    let mut lcg = Lcg(seed | 1);
    let mut input = vec![0.0f32; n_src * block];
    for value in input.iter_mut() {
        *value = lcg.next_f32();
    }
    input
}

// ---------------------------------------------------------------------------
// 共享 arena（离线工装；worker 间按 plan 归属写不相交区域 + ack 位图同步）
// ---------------------------------------------------------------------------

struct RawBuf<T> {
    ptr: *mut T,
    len: usize,
}

impl<T> Clone for RawBuf<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for RawBuf<T> {}

unsafe impl<T> Send for RawBuf<T> {}
unsafe impl<T> Sync for RawBuf<T> {}

impl<T> RawBuf<T> {
    fn wrap(slice: &mut [T]) -> Self {
        Self {
            ptr: slice.as_mut_ptr(),
            len: slice.len(),
        }
    }
    fn wrap_ro(slice: &[T]) -> Self {
        Self {
            ptr: slice.as_ptr() as *mut T,
            len: slice.len(),
        }
    }
    /// # Safety
    /// 调用方保证在 phase/worker 归属规则下无并发冲突（见文件头）。
    unsafe fn mut_slice(&self) -> &mut [T] {
        std::slice::from_raw_parts_mut(self.ptr, self.len)
    }
    /// # Safety
    /// 同 mut_slice：只在只读 phase 使用。
    unsafe fn ro_slice(&self) -> &[T] {
        std::slice::from_raw_parts(self.ptr, self.len)
    }
    /// # Safety
    /// len 必须为 1（单元素 arena）。
    unsafe fn item_mut(&self) -> &mut T {
        &mut *self.ptr
    }
    /// # Safety
    /// 同 item_mut，只读。
    unsafe fn item(&self) -> &T {
        &*self.ptr
    }
}

struct SharedCtl {
    acks: Vec<TspiBlockAckV1>,
    current_block: AtomicU64,
    done: AtomicU32,
    failed: AtomicU32,
    stop: AtomicBool,
    bad_output: AtomicU32,
}

struct WorkerTimes {
    busy: Vec<u64>,
    span: Vec<u64>,
    phase: Vec<[u64; 3]>,
    executed: u64,
    err: u32,
}

#[allow(clippy::too_many_arguments)]
fn dag_worker(
    worker: u8,
    plan: WorkerDagPlan,
    coeffs: Arc<CoeffData>,
    state: RawBuf<f32>,
    partials: RawBuf<WorkerPartials>,
    input: RawBuf<f32>,
    output: RawBuf<f32>,
    shared: Arc<SharedCtl>,
    total_blocks: usize,
    in_stride: usize,
    out_stride: usize,
) -> WorkerTimes {
    let frames = plan.block_size as usize;
    let expected = plan.expected_workers;
    let view = coeffs.view();
    let mut busy = vec![0u64; total_blocks];
    let mut span = vec![0u64; total_blocks];
    let mut phase_ns = vec![[0u64; 3]; total_blocks];
    let mut executed = 0u64;
    let mut err = 0u32;
    for index in 0..total_blocks {
        let seq = (index + 1) as u64;
        loop {
            if shared.stop.load(Ordering::Acquire) {
                return WorkerTimes {
                    busy,
                    span,
                    phase: phase_ns,
                    executed,
                    err: 1,
                };
            }
            if shared.current_block.load(Ordering::Acquire) == seq {
                break;
            }
            std::hint::spin_loop();
        }
        let ack = &shared.acks[index];
        let t_start = Instant::now();
        let mut local = [0u64; 3];
        for phase in 0..3u8 {
            let t_phase = Instant::now();
            let status = if phase == 0 {
                worker_phase_b1(
                    &plan,
                    worker,
                    &view,
                    unsafe { state.mut_slice() },
                    unsafe { input.ro_slice() },
                    in_stride,
                    frames,
                )
            } else if phase == 1 {
                worker_phase_b2(
                    &plan,
                    worker,
                    &view,
                    unsafe { state.ro_slice() },
                    unsafe { partials.item_mut() },
                    frames,
                )
            } else {
                worker_phase_b3(
                    &plan,
                    worker,
                    &view,
                    unsafe { state.mut_slice() },
                    unsafe { partials.item() },
                    unsafe { output.mut_slice() },
                    out_stride,
                    frames,
                    &shared.bad_output,
                )
            };
            local[phase as usize] = t_phase.elapsed().as_nanos() as u64;
            if !status.is_ok() {
                err = 1;
                let _ = set_cutoff(ack);
                break;
            }
            let ack_status = ack_phase(ack, worker, phase);
            let wait_status = wait_phase_done(ack, phase, expected, SPIN_LIMIT);
            if !ack_status.is_ok() || !wait_status.is_ok() {
                err = 1;
                let _ = set_cutoff(ack);
                break;
            }
        }
        span[index] = t_start.elapsed().as_nanos() as u64;
        phase_ns[index] = local;
        busy[index] = local.iter().sum();
        executed += 1;
        if err != 0 {
            shared.failed.store(1, Ordering::Release);
            shared.stop.store(true, Ordering::Release);
            shared.done.fetch_add(1, Ordering::Release);
            break;
        }
        shared.done.fetch_add(1, Ordering::Release);
    }
    WorkerTimes {
        busy,
        span,
        phase: phase_ns,
        executed,
        err,
    }
}

struct BlockRec {
    busy_ns: [u64; MAX_WORKERS],
    span_ns: [u64; MAX_WORKERS],
}

struct DagRun {
    wall: Vec<u64>,
    records: Vec<BlockRec>,
    worker: Vec<WorkerTimes>,
    ok: bool,
    output_hash: u64,
    output_nonfinite: u64,
    executed_blocks: u64,
}

#[allow(clippy::too_many_arguments)]
fn run_dag_rep(
    graph: &Graph,
    plan: &WorkerDagPlan,
    block: usize,
    workers: u8,
    coeffs: Arc<CoeffData>,
    state: &mut [f32],
    input: &[f32],
    output: &mut [f32],
    partials: &mut WorkerPartials,
    total_blocks: usize,
    warmup: usize,
    hash_output: bool,
) -> DagRun {
    let expected = workers as u32;
    let mut acks = Vec::with_capacity(total_blocks);
    for index in 0..total_blocks {
        let mut ack = TspiBlockAckV1::new(index as u64, 1, 1, workers);
        ack.flags = WORKER_ACK_FLAG_VALID;
        acks.push(ack);
    }
    let shared = Arc::new(SharedCtl {
        acks,
        current_block: AtomicU64::new(0),
        done: AtomicU32::new(0),
        failed: AtomicU32::new(0),
        stop: AtomicBool::new(false),
        bad_output: AtomicU32::new(0),
    });
    let state_raw = RawBuf::wrap(state);
    let input_raw = RawBuf::wrap_ro(input);
    let output_raw = RawBuf::wrap(output);
    let partials_raw = RawBuf::wrap(std::slice::from_mut(partials));
    let mut handles = Vec::with_capacity(workers as usize);
    for worker in 0..workers {
        let plan = *plan;
        let coeffs = Arc::clone(&coeffs);
        let shared = Arc::clone(&shared);
        handles.push(thread::spawn(move || {
            dag_worker(
                worker,
                plan,
                coeffs,
                state_raw,
                partials_raw,
                input_raw,
                output_raw,
                shared,
                total_blocks,
                block,
                block,
            )
        }));
    }
    let mut wall = Vec::with_capacity(total_blocks);
    let mut ok = true;
    for index in 0..total_blocks {
        shared.done.store(0, Ordering::Release);
        shared.bad_output.store(0, Ordering::Release);
        let t0 = Instant::now();
        // 与 T09 execute_block_scheduled 相同：块开始前 master 清 partials（块内串行准备）。
        unsafe { partials_raw.item_mut() }.zero();
        shared
            .current_block
            .store((index + 1) as u64, Ordering::Release);
        let mut spin = 0u64;
        loop {
            if shared.failed.load(Ordering::Acquire) != 0 {
                ok = false;
                break;
            }
            if shared.done.load(Ordering::Acquire) >= expected {
                break;
            }
            spin += 1;
            if spin >= MASTER_SPIN_LIMIT {
                ok = false;
                shared.stop.store(true, Ordering::Release);
                break;
            }
            std::hint::spin_loop();
        }
        let dt = t0.elapsed().as_nanos() as u64;
        if index >= warmup {
            wall.push(dt);
        }
        if !ok {
            break;
        }
    }
    shared.stop.store(true, Ordering::Release);
    shared.current_block.store(SENTINEL, Ordering::Release);
    let mut worker_times = Vec::with_capacity(handles.len());
    for handle in handles {
        worker_times.push(handle.join().expect("worker thread join"));
    }
    if worker_times.iter().any(|w| w.err != 0) {
        ok = false;
    }
    let executed_blocks = worker_times.iter().map(|w| w.executed).min().unwrap_or(0);
    if executed_blocks < total_blocks as u64 {
        ok = false;
    }
    let out_stride = block;
    let containers = graph.topo.container_channels as usize;
    let mut output_hash = 0xcbf2_9ce4_8422_2325u64;
    let mut output_nonfinite = 0u64;
    if hash_output {
        for &value in output[..containers * out_stride].iter() {
            if !value.is_finite() {
                output_nonfinite += 1;
            }
        }
        output_hash = fnv1a_f32(output_hash, &output[..containers * out_stride]);
    }
    let records: Vec<BlockRec> = (0..wall.len())
        .filter_map(|pos| {
            let index = pos + warmup;
            let mut busy = [0u64; MAX_WORKERS];
            let mut span = [0u64; MAX_WORKERS];
            for worker in 0..workers as usize {
                busy[worker] = worker_times[worker].busy[index];
                span[worker] = worker_times[worker].span[index];
            }
            Some(BlockRec {
                busy_ns: busy,
                span_ns: span,
            })
        })
        .collect();
    DagRun {
        wall,
        records,
        worker: worker_times,
        ok,
        output_hash,
        output_nonfinite,
        executed_blocks,
    }
}

// ---------------------------------------------------------------------------
// DAG 结果聚合
// ---------------------------------------------------------------------------

struct DagOutcome {
    graph: &'static str,
    block: usize,
    workers: u8,
    wall: Vec<u64>,
    load: [f64; MAX_WORKERS],
    busy_fraction: f64,
    barrier_wait_fraction: f64,
    load_cv: f64,
    load_range: f64,
    max_phase_ns: u64,
    static_tasks: [[u8; 3]; MAX_WORKERS],
    b2_columns: [u8; MAX_WORKERS],
    executed_blocks: u64,
    tiles_per_block: u32,
    output_hash: u64,
    rep_p50s: Vec<u64>,
    rep_spread: f64,
    ok: bool,
    note: String,
}

fn aggregate_dag(
    graph_name: &'static str,
    plan: &WorkerDagPlan,
    block: usize,
    workers: u8,
    runs: &[DagRun],
) -> DagOutcome {
    let mut wall = Vec::new();
    let mut busy_sum = [0u64; MAX_WORKERS];
    let mut span_sum = [0u64; MAX_WORKERS];
    let mut max_phase_ns = 0u64;
    let mut executed = 0u64;
    let mut ok = true;
    let mut hash = 0u64;
    let mut nonfinite = 0u64;
    let mut rep_p50s = Vec::with_capacity(runs.len());
    for run in runs {
        rep_p50s.push(stats_of(&run.wall).p50_ns);
        wall.extend_from_slice(&run.wall);
        ok &= run.ok;
        executed += run.executed_blocks;
        hash = run.output_hash;
        nonfinite += run.output_nonfinite;
        for worker in 0..workers as usize {
            for rec in &run.records {
                busy_sum[worker] += rec.busy_ns[worker];
                span_sum[worker] += rec.span_ns[worker];
            }
            for item in &run.worker[worker].phase {
                for &value in item.iter() {
                    max_phase_ns = max_phase_ns.max(value);
                }
            }
        }
    }
    let _ = span_sum;
    let rep_spread = {
        let min = rep_p50s.iter().cloned().min().unwrap_or(0);
        let max = rep_p50s.iter().cloned().max().unwrap_or(0);
        if min == 0 {
            0.0
        } else {
            (max - min) as f64 / min as f64
        }
    };
    let wall_sum: u128 = wall.iter().map(|&v| v as u128).sum();
    let mut load = [0.0f64; MAX_WORKERS];
    let mut active = 0usize;
    for worker in 0..workers as usize {
        if busy_sum[worker] > 0 {
            load[worker] = if wall_sum == 0 {
                0.0
            } else {
                busy_sum[worker] as f64 / wall_sum as f64
            };
            active += 1;
        }
    }
    let (load_cv, load_range, busy_fraction, barrier_wait_fraction) = if active == 0
        || wall_sum == 0
    {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        let mut values = Vec::new();
        for worker in 0..workers as usize {
            if busy_sum[worker] > 0 {
                values.push(load[worker]);
            }
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let var = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64;
        let std = var.sqrt();
        let max = values.iter().cloned().fold(f64::MIN, f64::max);
        let min = values.iter().cloned().fold(f64::MAX, f64::min);
        (
            if mean == 0.0 { 0.0 } else { std / mean },
            if mean == 0.0 { 0.0 } else { (max - min) / mean },
            mean,
            1.0 - max,
        )
    };
    let mut static_tasks = [[0u8; 3]; MAX_WORKERS];
    let mut b2_columns = [0u8; MAX_WORKERS];
    for worker in 0..workers as usize {
        for phase in 0..3u8 {
            static_tasks[worker][phase as usize] = plan.worker_task_count(phase, worker as u8);
        }
    }
    let n_role = plan.topology.n_role as usize;
    for k in 0..n_role {
        let lane = plan.k_lane[k] as usize;
        let worker = plan.lane_worker[lane] as usize;
        if worker < MAX_WORKERS {
            b2_columns[worker] = b2_columns[worker].saturating_add(1);
        }
    }
    let note = if nonfinite > 0 {
        format!("nonfinite_output={nonfinite}")
    } else if !ok {
        "run_failed".to_string()
    } else {
        String::new()
    };
    DagOutcome {
        graph: graph_name,
        block,
        workers,
        wall,
        load,
        busy_fraction,
        barrier_wait_fraction,
        load_cv,
        load_range,
        max_phase_ns,
        static_tasks,
        b2_columns,
        executed_blocks: executed,
        tiles_per_block: plan.total_tasks() as u32,
        output_hash: hash,
        rep_p50s,
        rep_spread,
        ok,
        note,
    }
}

// ---------------------------------------------------------------------------
// 单线程 scalar kernel bench（direct / partitioned）
// ---------------------------------------------------------------------------

struct ScalarOutcome {
    graph: &'static str,
    kernel: &'static str,
    block: usize,
    wall: Vec<u64>,
    executed_blocks: u64,
    output_hash: u64,
    ok: bool,
    note: String,
}

fn bench_direct(
    graph: &Graph,
    block: usize,
    blocks: usize,
    warmup: usize,
    reps: usize,
) -> ScalarOutcome {
    let layout = DirectLayout::build(&graph.topo).expect("direct layout");
    let stride = block;
    let mut state = vec![0.0f32; layout.state_samples()];
    let input = fill_input(graph.topo.n_src as usize, block, 0x51ed_2701);
    let containers = graph.topo.container_channels as usize;
    let mut output = vec![0.0f32; containers * stride];
    let coeffs = graph.coeffs.view();
    let mut wall = Vec::with_capacity(blocks * reps);
    let mut ok = true;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut executed = 0u64;
    for rep in 0..reps {
        if rep > 0 {
            direct_state_zero_f32(&mut state);
        }
        for index in 0..blocks {
            let t0 = Instant::now();
            let status = direct_block_f32(
                &graph.topo,
                &layout,
                &coeffs,
                &mut state,
                &input,
                stride,
                &mut output,
                stride,
                block,
                block,
            );
            let dt = t0.elapsed().as_nanos() as u64;
            if !status.is_ok() {
                ok = false;
                break;
            }
            executed += 1;
            if index >= warmup {
                wall.push(dt);
            }
        }
        hash = fnv1a_f32(hash, &output);
    }
    ScalarOutcome {
        graph: "matrix_prod",
        kernel: "direct_scalar",
        block,
        wall,
        executed_blocks: executed,
        output_hash: hash,
        ok,
        note: String::new(),
    }
}

fn bench_partitioned(
    graph: &Graph,
    block: usize,
    blocks: usize,
    warmup: usize,
    reps: usize,
) -> ScalarOutcome {
    let layout = PartitionedLayout::build(&graph.topo, block).expect("partitioned layout");
    let mut plan_data = vec![0.0f32; layout.plan_samples()];
    let mut build_scratch = vec![0.0f32; layout.build_scratch_samples()];
    let plan = match PartitionedPlan::build_f32(
        &graph.topo,
        block,
        &graph.coeffs.view(),
        &mut plan_data,
        &mut build_scratch,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            return ScalarOutcome {
                graph: "matrix_prod",
                kernel: "partitioned_scalar",
                block,
                wall: Vec::new(),
                executed_blocks: 0,
                output_hash: 0,
                ok: false,
                note: format!("plan_build_failed:{err:?}"),
            };
        }
    };
    let stride = block;
    let mut state = vec![0.0f32; layout.state_samples()];
    let input = fill_input(graph.topo.n_src as usize, block, 0x2b7e_1516);
    let containers = graph.topo.container_channels as usize;
    let mut output = vec![0.0f32; containers * stride];
    let mut wall = Vec::with_capacity(blocks * reps);
    let mut ok = true;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut executed = 0u64;
    for rep in 0..reps {
        if rep > 0 {
            partitioned_state_zero_f32(&mut state);
        }
        for index in 0..blocks {
            let t0 = Instant::now();
            let status = partitioned_block_f32(
                &graph.topo,
                &plan,
                &plan_data,
                &mut state,
                &input,
                stride,
                &mut output,
                stride,
                block,
                block,
            );
            let dt = t0.elapsed().as_nanos() as u64;
            if !status.is_ok() {
                ok = false;
                break;
            }
            executed += 1;
            if index >= warmup {
                wall.push(dt);
            }
        }
        hash = fnv1a_f32(hash, &output);
    }
    ScalarOutcome {
        graph: "matrix_prod",
        kernel: "partitioned_scalar",
        block,
        wall,
        executed_blocks: executed,
        output_hash: hash,
        ok,
        note: String::new(),
    }
}
// ---------------------------------------------------------------------------
// CPU 预算核算（T56 schedule_plan 静态模型，与 DSP-HOST-CPU-BUDGET 对齐）
// ---------------------------------------------------------------------------

const CORE_RATES: [(&str, u64); 5] = [
    ("8GF", 4_000_000_000),
    ("9.23GF", 4_615_384_615),
    ("10GF", 5_000_000_000),
    ("12GF", 6_000_000_000),
    ("15GF", 7_500_000_000),
];

struct BudgetRow {
    kind: &'static str,
    profile: String,
    kernel: String,
    workers: u8,
    block: u16,
    core_gmac_s: u64,
    gate_ns: u64,
    work_per_block: u64,
    macs_per_second: u64,
    flops_per_second: u64,
    total_worst_ns: u64,
    worst_worker: u8,
    worker_work: [u64; MAX_WORKERS],
    plan_bytes: u64,
    dual_graph_bytes: u64,
    admitted: bool,
    serial_fraction_q16: [u32; 3],
    status: String,
}

fn budget_profiles() -> Vec<(&'static str, PlanProfile)> {
    let base = PlanProfile::default();
    let shared = |block: u16| PlanProfile {
        block_size: block,
        n_src: 5,
        n_role: 5,
        container_channels: 6,
        m: 5,
        l_a: 1,
        l_g: 4096,
        l_p: 1,
        d_samples: 0,
        matrix_paths: 25,
        diag_firs: 5,
        max_taps: 4096,
        extended_taps_admitted: false,
        fusion: FusionForm::FusedGp,
        kernel: KernelForm::SharedFft,
        workers: 4,
        total_delay_q16: 0,
        ..base
    };
    vec![
        ("p1_48k_b128_direct_w4", base),
        (
            "p1_48k_b64_direct_w4",
            PlanProfile {
                block_size: 64,
                ..base
            },
        ),
        ("p1_48k_b128_direct_w2", base.with_workers(2)),
        ("p1_48k_b128_direct_w1", base.with_workers(1)),
        (
            "p1_48k_b128_direct_w4_8192",
            PlanProfile {
                l_g: 8192,
                max_taps: 8192,
                extended_taps_admitted: true,
                ..base
            },
        ),
        (
            "p1_48k_b128_hybrid_w4_8192",
            PlanProfile {
                l_g: 8192,
                max_taps: 8192,
                extended_taps_admitted: true,
                kernel: KernelForm::Hybrid,
                ..base
            },
        ),
        ("p1_48k_b128_sharedfft_w4_4096", shared(128)),
        ("p1_48k_b64_sharedfft_w4_4096", shared(64)),
    ]
}

fn budget_deadline_rows() -> Vec<BudgetRow> {
    let mut rows = Vec::new();
    for (name, profile) in budget_profiles() {
        match build_schedule(&profile) {
            Ok(plan) => {
                for (rate_name, core_rate) in CORE_RATES {
                    let admission =
                        admit_deadline(&plan, &DeadlineBudgets::with_core_rate(core_rate));
                    rows.push(BudgetRow {
                        kind: "deadline",
                        profile: format!("{name}@{rate_name}"),
                        kernel: plan.kernel_form.as_str().to_string(),
                        workers: plan.workers(),
                        block: profile.block_size,
                        core_gmac_s: core_rate,
                        gate_ns: admission.gate_ns,
                        work_per_block: plan.work_per_block,
                        macs_per_second: plan.macs_per_second,
                        flops_per_second: plan.flops_per_second,
                        total_worst_ns: admission.total_worst_ns,
                        worst_worker: admission.worst_worker,
                        worker_work: [
                            plan.worker_work_total[0],
                            plan.worker_work_total[1],
                            plan.worker_work_total[2],
                            plan.worker_work_total[3],
                        ],
                        plan_bytes: plan.plan_bytes,
                        dual_graph_bytes: plan.dual_graph_bytes,
                        admitted: admission.admitted,
                        serial_fraction_q16: plan.serial_fraction_q16,
                        status: if admission.admitted {
                            "ADMITTED".to_string()
                        } else {
                            "REJECTED".to_string()
                        },
                    });
                }
            }
            Err(err) => rows.push(BudgetRow {
                kind: "deadline",
                profile: name.to_string(),
                kernel: profile.kernel.as_str().to_string(),
                workers: profile.workers,
                block: profile.block_size,
                core_gmac_s: 0,
                gate_ns: 0,
                work_per_block: 0,
                macs_per_second: 0,
                flops_per_second: 0,
                total_worst_ns: 0,
                worst_worker: 0,
                worker_work: [0; MAX_WORKERS],
                plan_bytes: 0,
                dual_graph_bytes: 0,
                admitted: false,
                serial_fraction_q16: [0; 3],
                status: format!("BUILD_ERR:{err:?}"),
            }),
        }
    }
    rows
}

struct SwapFallbackRow {
    kind: &'static str,
    profile: String,
    admitted: bool,
    peak_cpu_ns: u64,
    peak_ram_bytes: u64,
    gate_ns: u64,
    plan_bytes: u64,
    status: String,
}

fn budget_swap_fallback_rows() -> Vec<SwapFallbackRow> {
    let mut rows = Vec::new();
    let crossfade_profile = PlanProfile {
        crossfade: true,
        ..PlanProfile::default()
    };
    if let (Ok(old), Ok(new)) = (
        build_schedule(&crossfade_profile),
        build_schedule(&crossfade_profile),
    ) {
        let spec = HotSwapSpec {
            old_plan_version: 1,
            new_plan_version: 2,
            crossfade_frames: 128,
            available_cpu_ns: TBLOCK_128_48K_NS, // 一个完整块窗口
            available_ram_bytes: 64 * 1024 * 1024,
            core_macs_per_second: 0,
        };
        let verdict = evaluate_hot_swap(&old, &new, &spec);
        rows.push(SwapFallbackRow {
            kind: "hot_swap",
            profile: "p1_48k_b128_direct_w4_crossfade".to_string(),
            admitted: verdict.admitted,
            peak_cpu_ns: verdict.peak_cpu_ns,
            peak_ram_bytes: verdict.peak_ram_bytes,
            gate_ns: TBLOCK_128_48K_NS,
            plan_bytes: old.plan_bytes,
            status: verdict.disposition.as_str().to_string(),
        });
    }
    if let Ok(normal) = build_schedule(&PlanProfile::default()) {
        if let Ok(fallback) = build_fallback_schedule(&normal, 4) {
            let admission = evaluate_fallback(
                &normal,
                &FallbackBudgets {
                    deadline: DeadlineBudgets::default(),
                    available_ram_bytes: 4 * 1024 * 1024,
                    preallocated: true,
                    workers: 4,
                },
            );
            rows.push(SwapFallbackRow {
                kind: "fallback",
                profile: "p1_48k_b128_direct_w4_fallback_G=I".to_string(),
                admitted: admission.admitted,
                peak_cpu_ns: admission.deadline.total_worst_ns,
                peak_ram_bytes: admission.plan_bytes,
                gate_ns: TBLOCK_128_48K_NS,
                plan_bytes: fallback.plan_bytes,
                status: admission.reason.as_str().to_string(),
            });
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// 门判定（本地 x86 参考 vs 板端 only）
// ---------------------------------------------------------------------------

struct Gate {
    id: &'static str,
    requirement: String,
    threshold: String,
    measured: String,
    status: &'static str,
    source: &'static str,
    note: String,
}

fn find_outcome<'a>(
    outcomes: &'a [DagOutcome],
    graph: &str,
    block: usize,
    workers: u8,
) -> Option<&'a DagOutcome> {
    outcomes
        .iter()
        .find(|o| o.graph == graph && o.block == block && o.workers == workers)
}

// ---------------------------------------------------------------------------
// CSV / JSON 输出
// ---------------------------------------------------------------------------

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn write_csv(path: &Path, header: &[&str], rows: &[Vec<String>]) -> std::io::Result<()> {
    let mut text = String::new();
    text.push_str(&header.join(","));
    text.push('\n');
    for row in rows {
        let escaped: Vec<String> = row.iter().map(|v| csv_escape(v)).collect();
        text.push_str(&escaped.join(","));
        text.push('\n');
    }
    fs::write(path, text)
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn fmt_f64(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let cfg = parse_args();
    if let Err(err) = fs::create_dir_all(&cfg.out_dir) {
        eprintln!("cannot create out dir {:?}: {err}", cfg.out_dir);
        std::process::exit(2);
    }
    let parallelism = thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(0);
    println!(
        "TSPI perf harness {HARNESS_VERSION} | board=false | out={:?} | blocks={} warmup={} reps={} | avail_parallelism={}",
        cfg.out_dir, cfg.blocks, cfg.warmup, cfg.reps, parallelism
    );
    if cfg.reps < 2 {
        println!("WARN: reps < 2 -> 无重复间离散度，不能作为正式统计");
    }

    // ---------------- DAG kernel bench ----------------
    let graph_matrix = Arc::new(graph_4_1_2(4096, 1, 0));
    let graph_diag = Arc::new(graph_4_1_2(1, 1024, 0));
    let graph_gate128 = Arc::new(graph_4_1_2(256, 1, 0));
    let graph_gate64 = Arc::new(graph_4_1_2(128, 1, 0));
    let configs: Vec<(&'static str, Arc<Graph>, usize, u8)> = vec![
        ("matrix_prod_g4096_p1", Arc::clone(&graph_matrix), 128, 1),
        ("matrix_prod_g4096_p1", Arc::clone(&graph_matrix), 128, 2),
        ("matrix_prod_g4096_p1", Arc::clone(&graph_matrix), 128, 4),
        ("matrix_prod_g4096_p1", Arc::clone(&graph_matrix), 64, 1),
        ("matrix_prod_g4096_p1", Arc::clone(&graph_matrix), 64, 2),
        ("matrix_prod_g4096_p1", Arc::clone(&graph_matrix), 64, 4),
        ("diag_p1024_g1", Arc::clone(&graph_diag), 128, 1),
        ("diag_p1024_g1", Arc::clone(&graph_diag), 128, 4),
        ("gate_selfcheck_g256", Arc::clone(&graph_gate128), 128, 4),
        ("gate_selfcheck_g128", Arc::clone(&graph_gate64), 64, 4),
    ];
    let mut outcomes: Vec<DagOutcome> = Vec::new();
    let mut dag_executed_blocks = 0u64;
    for (graph_name, graph, block, workers) in &configs {
        let plan = match WorkerDagPlan::build(&graph.topo, *block as u16, *workers, 1) {
            Ok(plan) => plan,
            Err(err) => {
                println!("DAG BUILD FAIL {graph_name} B{block} W{workers}: {err:?}");
                continue;
            }
        };
        let layout = DirectLayout::build(&graph.topo).expect("layout");
        let mut state = vec![0.0f32; layout.state_samples()];
        let input = fill_input(graph.topo.n_src as usize, *block, 0x9e37_79b1);
        let containers = graph.topo.container_channels as usize;
        let mut output = vec![0.0f32; containers * *block];
        let mut partials = Box::new(WorkerPartials::new());
        let coeffs = Arc::new(CoeffData {
            a: graph.coeffs.a.clone(),
            g: graph.coeffs.g.clone(),
            p: graph.coeffs.p.clone(),
        });
        let total_blocks = cfg.warmup + cfg.blocks;
        let mut runs = Vec::with_capacity(cfg.reps);
        for _rep in 0..cfg.reps {
            runs.push(run_dag_rep(
                graph,
                &plan,
                *block,
                *workers,
                Arc::clone(&coeffs),
                &mut state,
                &input,
                &mut output,
                &mut partials,
                total_blocks,
                cfg.warmup,
                true,
            ));
        }
        let outcome = aggregate_dag(graph_name, &plan, *block, *workers, &runs);
        dag_executed_blocks += outcome.executed_blocks * (*workers as u64);
        let wall_stats = stats_of(&outcome.wall);
        println!(
            "DAG {graph_name} B{block} W{workers}: p50={}ns p99={}ns max={}ns cv={:.3} load_cv={:.3} load_range={:.3} busy={:.3} ok={} exec_blocks={}",
            wall_stats.p50_ns,
            wall_stats.p99_ns,
            wall_stats.max_ns,
            wall_stats.cv,
            outcome.load_cv,
            outcome.load_range,
            outcome.busy_fraction,
            outcome.ok,
            outcome.executed_blocks
        );
        outcomes.push(outcome);
    }

    // bit-exact W1 vs W4（同输入/同系数/同 plan 语义；防止 bench 工装静默跑错）
    let bit_exact: String;
    let bit_exact_hashes: (u64, u64);
    {
        let graph = &graph_matrix;
        let block = 128usize;
        let mut hashes = [0u64; 2];
        let mut ok_all = true;
        for workers in [1u8, 4u8] {
            let plan = WorkerDagPlan::build(&graph.topo, block as u16, workers, 1).expect("plan");
            let layout = DirectLayout::build(&graph.topo).expect("layout");
            let mut state = vec![0.0f32; layout.state_samples()];
            let input = fill_input(graph.topo.n_src as usize, block, 0x0bad_f00d);
            let containers = graph.topo.container_channels as usize;
            let mut output = vec![0.0f32; containers * block];
            let mut partials = Box::new(WorkerPartials::new());
            let coeffs = Arc::new(CoeffData {
                a: graph.coeffs.a.clone(),
                g: graph.coeffs.g.clone(),
                p: graph.coeffs.p.clone(),
            });
            let mut run_hash = 0xcbf2_9ce4_8422_2325u64;
            let reps = 3;
            for _ in 0..reps {
                let run = run_dag_rep(
                    graph,
                    &plan,
                    block,
                    workers,
                    Arc::clone(&coeffs),
                    &mut state,
                    &input,
                    &mut output,
                    &mut partials,
                    1,
                    0,
                    true,
                );
                ok_all &= run.ok;
                run_hash = run.output_hash ^ run_hash.rotate_left(13);
            }
            hashes[if workers == 1 { 0 } else { 1 }] = run_hash;
            dag_executed_blocks += reps as u64 * workers as u64;
        }
        bit_exact_hashes = (hashes[0], hashes[1]);
        bit_exact = if !ok_all {
            "RUN_FAILED".to_string()
        } else if hashes[0] == hashes[1] {
            "PASS_BIT_EXACT_W1_W4".to_string()
        } else {
            "FAIL_MISMATCH_W1_W4".to_string()
        };
        println!("BITEXACT w1_vs_w4: {bit_exact}");
    }

    // ---------------- 单线程 scalar kernel（direct / partitioned） ----------------
    let mut scalar_outcomes = Vec::new();
    let mut scalar_executed = 0u64;
    for (block, blocks, warmup, reps) in [
        (128usize, cfg.blocks, cfg.warmup, cfg.reps),
        (64usize, cfg.blocks, cfg.warmup, cfg.reps),
    ] {
        let direct = bench_direct(&graph_matrix, block, blocks, warmup, reps);
        println!(
            "DIRECT B{block}: p50={}ns max={}ns executed={} ok={}",
            stats_of(&direct.wall).p50_ns,
            stats_of(&direct.wall).max_ns,
            direct.executed_blocks,
            direct.ok
        );
        scalar_executed += direct.executed_blocks;
        scalar_outcomes.push(direct);
        let partitioned = bench_partitioned(&graph_matrix, block, blocks, warmup, reps);
        println!(
            "PARTITIONED B{block}: p50={}ns max={}ns executed={} ok={} note={}",
            stats_of(&partitioned.wall).p50_ns,
            stats_of(&partitioned.wall).max_ns,
            partitioned.executed_blocks,
            partitioned.ok,
            partitioned.note
        );
        scalar_executed += partitioned.executed_blocks;
        scalar_outcomes.push(partitioned);
    }

    // ---------------- CPU 预算核算 ----------------
    let budget_rows = budget_deadline_rows();
    let swap_rows = budget_swap_fallback_rows();
    println!(
        "BUDGET deadline_rows={} swap_fallback_rows={}",
        budget_rows.len(),
        swap_rows.len()
    );

    // ---------------- 门判定 ----------------
    // 本机非 RT 且可能与其他会话共享：任一条正式矩阵配置的 rep_p50 离散度 >25% 视为受扰，
    // 本地指示性判定降级为 INCONCLUSIVE，避免把噪声写成 PASS/FAIL。
    let run_contended = outcomes
        .iter()
        .any(|o| o.graph.starts_with("matrix_prod") && o.rep_spread > 0.25);
    let run_quality = if run_contended { "CONTENDED" } else { "OK" };
    println!("RUN_QUALITY {run_quality} (rep_p50_spread>25% on matrix_prod would mark CONTENDED)");
    let mut gates: Vec<Gate> = Vec::new();
    let matrix128_w1 = find_outcome(&outcomes, "matrix_prod_g4096_p1", 128, 1);
    let matrix128_w2 = find_outcome(&outcomes, "matrix_prod_g4096_p1", 128, 2);
    let matrix128_w4 = find_outcome(&outcomes, "matrix_prod_g4096_p1", 128, 4);
    let matrix64_w4 = find_outcome(&outcomes, "matrix_prod_g4096_p1", 64, 4);
    if let (Some(w1), Some(w4)) = (matrix128_w1, matrix128_w4) {
        let s1 = stats_of(&w1.wall);
        let s4 = stats_of(&w4.wall);
        let mut speedup = if s4.p50_ns == 0 {
            0.0
        } else {
            s1.p50_ns as f64 / s4.p50_ns as f64
        };
        let mut speedup2 = 0.0;
        if let Some(w2) = matrix128_w2 {
            let s2 = stats_of(&w2.wall);
            if s2.p50_ns != 0 {
                speedup2 = s1.p50_ns as f64 / s2.p50_ns as f64;
            }
        }
        speedup = if speedup.is_finite() { speedup } else { 0.0 };
        let speedup_best = {
            let w1_best = w1.rep_p50s.iter().cloned().min().unwrap_or(0);
            let w4_best = w4.rep_p50s.iter().cloned().min().unwrap_or(0);
            if w4_best == 0 {
                0.0
            } else {
                w1_best as f64 / w4_best as f64
            }
        };
        gates.push(Gate {
            id: "T46_speedup_1_vs_4_median",
            requirement: "同 kernel/precision/coeff/input/block，只改 1/2/4 worker；speedup=T1_wall/T4_wall >= 3.2".to_string(),
            threshold: ">=3.2".to_string(),
            measured: format!(
                "T1_median={}ns T2_median={}ns T4_median={}ns speedup4={:.3} speedup2={:.3} speedup4_best_rep={:.3}",
                s1.p50_ns,
                matrix128_w2
                    .map(|o| stats_of(&o.wall).p50_ns)
                    .unwrap_or(0),
                s4.p50_ns,
                speedup,
                speedup2,
                speedup_best
            ),
            status: if run_contended {
                "INCONCLUSIVE_LOCAL_X86_CONTENDED"
            } else if speedup >= 3.2 {
                "PASS_LOCAL_X86_INDICATIVE"
            } else {
                "UNDER_3.2_LOCAL_X86_INDICATIVE"
            },
            source: "WBS T46 / DECISION §4 后续正式性能门",
            note: "x86 本机参考（非 A72、非 RT 内核、无 NUMA/热控/IRQ 干扰控制）；板端另测才是裁决".to_string(),
        });
        gates.push(Gate {
            id: "T30_worst_128_frame",
            requirement:
                "四 worker 完整生产图 128 帧每块最大完成时间 worst < 50% deadline = 1.3333ms"
                    .to_string(),
            threshold: "<1,333,333ns".to_string(),
            measured: format!("T4_max={}ns p99.9={}ns", s4.max_ns, s4.p999_ns),
            status: if run_contended {
                "INCONCLUSIVE_LOCAL_X86_CONTENDED"
            } else if s4.max_ns < GATE_48K_128_NS {
                "PASS_LOCAL_X86_REFERENCE"
            } else {
                "OVER_GATE_LOCAL_X86_REFERENCE"
            },
            source: "WBS T30 / DSP-HOST-CPU-BUDGET §2.1 / DECISION §4",
            note:
                "内核是当前 scalar f32 direct（x86 自动向量化），无 NEON A72 证明；板端门 NOT_RUN"
                    .to_string(),
        });
        if let Some(w4) = matrix64_w4 {
            let s = stats_of(&w4.wall);
            gates.push(Gate {
                id: "T30_worst_64_frame",
                requirement: "64 帧独立窗口每块最大完成时间 worst < 50% deadline = 0.6667ms"
                    .to_string(),
                threshold: "<666,666ns".to_string(),
                measured: format!("T4_max={}ns p99.9={}ns", s.max_ns, s.p999_ns),
                status: if run_contended {
                    "INCONCLUSIVE_LOCAL_X86_CONTENDED"
                } else if s.max_ns < GATE_48K_64_NS {
                    "PASS_LOCAL_X86_REFERENCE"
                } else {
                    "OVER_GATE_LOCAL_X86_REFERENCE"
                },
                source: "WBS T30 / DECISION §4",
                note: "x86 参考；板端独立窗口 NOT_RUN".to_string(),
            });
        }
    }
    for (graph, block, gate_ns, label) in [
        (
            "gate_selfcheck_g256",
            128usize,
            GATE_48K_128_NS,
            "harness_selfcheck_128",
        ),
        (
            "gate_selfcheck_g128",
            64usize,
            GATE_48K_64_NS,
            "harness_selfcheck_64",
        ),
    ] {
        if let Some(outcome) = find_outcome(&outcomes, graph, block, 4) {
            let s = stats_of(&outcome.wall);
            gates.push(Gate {
                id: if block == 128 {
                    "harness_selfcheck_gate_128"
                } else {
                    "harness_selfcheck_gate_64"
                },
                requirement: format!(
                    "{label}: 缩小 taps 的 harness 自检图（G=256/128 tap）在 4 worker 下 worst < 门；用于验证门计算链路，不是生产性能结论"
                ),
                threshold: format!("<{gate_ns}ns"),
                measured: format!(
                    "T4_p50={}ns p99={}ns max={}ns",
                    s.p50_ns, s.p99_ns, s.max_ns
                ),
                status: if s.p50_ns < gate_ns {
                    "PASS_HARNESS_SELFCHECK"
                } else {
                    "FAIL_HARNESS_SELFCHECK"
                },
                source: "本工装自检（非生产门）",
                note: format!(
                    "executed_blocks={} ok={}",
                    outcome.executed_blocks, outcome.ok
                ),
            });
        }
    }
    gates.push(Gate {
        id: "T29_rt_irq_cyclictest_100us",
        requirement:
            "P1 后另约正式 RT/IRQ/cyclictest/osnoise 满载专窗，100us 工程门（idle/hackbench 满载）"
                .to_string(),
        threshold: "cyclictest max < 100us（工程门）".to_string(),
        measured: "NOT_RUN".to_string(),
        status: "NOT_RUN_BOARD_ONLY",
        source: "WBS T29",
        note: "需要板端独占窗口 + RT 内核 + IRQ 亲和/温控；本工装不触板".to_string(),
    });
    gates.push(Gate {
        id: "T46_board_same_kernel_speedup",
        requirement: "板端同 kernel/系数/输入的 1/2/4 wall speedup>=3.2、CV/range、busy_compute/phase/worst 验收".to_string(),
        threshold: ">=3.2 且 CV<=0.15/range<=0.30（长滤波 steady 默认）".to_string(),
        measured: "NOT_RUN".to_string(),
        status: "NOT_RUN_BOARD_ONLY",
        source: "WBS T46",
        note: "x86 结果只作工装/趋势参考，不冒充 A72 板端".to_string(),
    });
    gates.push(Gate {
        id: "T30_1h_soak_zero_xrun",
        requirement: "已校准生产图四 worker 128/48k 1h 0 xrun，worst<1.3333ms；64 独立<0.6667ms；冷启/切换恢复全记".to_string(),
        threshold: "0 xrun / 1h".to_string(),
        measured: "NOT_RUN".to_string(),
        status: "NOT_RUN_BOARD_ONLY",
        source: "WBS T30",
        note: "需要真实 ALSA/MOTU 与板端独占窗口；本任务只借 bench 工装，不触板".to_string(),
    });
    gates.push(Gate {
        id: "G_performance",
        requirement: "同 kernel 1/2/4>=3.2、128 满图 worst<1.3333ms/1h0xrun、64 独立窗<0.6667ms 及正式负载矩阵".to_string(),
        threshold: "全部满足".to_string(),
        measured: "NOT_RUN".to_string(),
        status: "NOT_RUN_BOARD_ONLY",
        source: "DECISION §7 G-performance",
        note: "P1 完成后另行验收；当前不签 PASS".to_string(),
    });
    gates.push(Gate {
        id: "bit_exact_w1_vs_w4",
        requirement:
            "同 plan 语义下 W=1/2/4 输出逐位一致（T09 口径），证明 bench 工装没有静默跑错图"
                .to_string(),
        threshold: "hash 相等".to_string(),
        measured: format!(
            "{} hash_w1={:016x} hash_w4={:016x}",
            bit_exact, bit_exact_hashes.0, bit_exact_hashes.1
        ),
        status: if bit_exact == "PASS_BIT_EXACT_W1_W4" {
            "PASS_LOCAL"
        } else {
            "FAIL_LOCAL"
        },
        source: "T09 workers 确定性契约 / 本工装自检",
        note: "只比较 W=1 与 W=4，同一输入/系数/block；不替代 T07 对拍".to_string(),
    });

    // ---------------- 写 CSV / JSON ----------------
    let mut kernel_rows: Vec<Vec<String>> = Vec::new();
    let mut medians_w1: BTreeMap<(String, usize), u64> = BTreeMap::new();
    let mut best_rep_w1: BTreeMap<(String, usize), u64> = BTreeMap::new();
    for outcome in &outcomes {
        if outcome.workers == 1 {
            medians_w1.insert(
                (outcome.graph.to_string(), outcome.block),
                stats_of(&outcome.wall).p50_ns,
            );
            best_rep_w1.insert(
                (outcome.graph.to_string(), outcome.block),
                outcome.rep_p50s.iter().cloned().min().unwrap_or(0),
            );
        }
    }
    for outcome in &outcomes {
        let s = stats_of(&outcome.wall);
        let speedup = if outcome.workers == 4 {
            match medians_w1.get(&(outcome.graph.to_string(), outcome.block)) {
                Some(w1) if s.p50_ns != 0 => fmt_f64(*w1 as f64 / s.p50_ns as f64, 3),
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let speedup_best = if outcome.workers == 4 {
            let w4_best = outcome.rep_p50s.iter().cloned().min().unwrap_or(0);
            match best_rep_w1.get(&(outcome.graph.to_string(), outcome.block)) {
                Some(w1) if w4_best != 0 => fmt_f64(*w1 as f64 / w4_best as f64, 3),
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let rep_p50_spread = fmt_f64(outcome.rep_spread, 4);
        let rep_p50_list = outcome
            .rep_p50s
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(";");
        let load_list = (0..outcome.workers as usize)
            .map(|worker| fmt_f64(outcome.load[worker], 4))
            .collect::<Vec<_>>()
            .join(";");
        let mut planned_mac = 0u64;
        let mut executed_mac = 0u64;
        if let Some((_, graph, block, _)) = configs.iter().find(|(name, _, b, w)| {
            *name == outcome.graph && *b == outcome.block && *w == outcome.workers
        }) {
            let topo = &graph.topo;
            let b = *block as u64;
            let n_role = topo.n_role as u64;
            let m = topo.m as u64;
            planned_mac = m * m * topo.l_g as u64 * b
                + n_role * topo.l_p as u64 * b
                + n_role * topo.l_a as u64 * b;
            executed_mac = n_role * n_role * topo.l_g as u64 * b
                + n_role * topo.l_p as u64 * b
                + n_role * topo.l_a as u64 * b;
        }
        let effective_gflops = if s.p50_ns == 0 {
            0.0
        } else {
            (2 * planned_mac) as f64 / s.p50_ns as f64
        };
        let executed_gflops = if s.p50_ns == 0 {
            0.0
        } else {
            (2 * executed_mac) as f64 / s.p50_ns as f64
        };
        let gate_ns = if outcome.block == 64 {
            GATE_48K_64_NS
        } else {
            GATE_48K_128_NS
        };
        let gate_verdict = if outcome.graph.starts_with("gate_selfcheck") {
            if s.p50_ns < gate_ns {
                "PASS_HARNESS_SELFCHECK"
            } else {
                "FAIL_HARNESS_SELFCHECK"
            }
        } else if s.max_ns < gate_ns {
            "PASS_LOCAL_X86_REFERENCE"
        } else {
            "OVER_GATE_LOCAL_X86_REFERENCE"
        };
        kernel_rows.push(vec![
            "dag".to_string(),
            outcome.graph.to_string(),
            "worker_dag_scalar_f32".to_string(),
            outcome.block.to_string(),
            outcome.workers.to_string(),
            outcome.workers.to_string(),
            s.n.to_string(),
            s.min_ns.to_string(),
            s.p50_ns.to_string(),
            s.p95_ns.to_string(),
            s.p99_ns.to_string(),
            s.p999_ns.to_string(),
            s.max_ns.to_string(),
            s.mean_ns.to_string(),
            fmt_f64(s.cv, 4),
            speedup,
            speedup_best,
            rep_p50_spread,
            rep_p50_list,
            load_list,
            fmt_f64(outcome.busy_fraction, 4),
            fmt_f64(outcome.barrier_wait_fraction, 4),
            fmt_f64(outcome.load_cv, 4),
            fmt_f64(outcome.load_range, 4),
            outcome.max_phase_ns.to_string(),
            format!(
                "{};{};{}",
                outcome.static_tasks[0][1], outcome.static_tasks[1][1], outcome.static_tasks[2][1]
            ),
            format!(
                "{};{};{};{}",
                outcome.b2_columns[0],
                outcome.b2_columns[1],
                outcome.b2_columns[2],
                outcome.b2_columns[3]
            ),
            planned_mac.to_string(),
            executed_mac.to_string(),
            fmt_f64(effective_gflops, 3),
            fmt_f64(executed_gflops, 3),
            gate_ns.to_string(),
            gate_verdict.to_string(),
            outcome.executed_blocks.to_string(),
            outcome.tiles_per_block.to_string(),
            format!("{:016x}", outcome.output_hash),
            outcome.note.clone(),
        ]);
    }
    let kernel_header = [
        "section",
        "graph",
        "kernel",
        "block",
        "workers",
        "label_workers",
        "samples",
        "min_ns",
        "p50_ns",
        "p95_ns",
        "p99_ns",
        "p99_9_ns",
        "max_ns",
        "mean_ns",
        "cv",
        "speedup_vs_w1_median",
        "speedup_vs_w1_best_rep_median",
        "rep_p50_spread",
        "rep_p50_ns_list",
        "load_by_worker_w0_w3",
        "busy_fraction",
        "barrier_wait_fraction",
        "load_cv",
        "load_range",
        "max_phase_ns",
        "static_phase0_1_2_tasks_worker0",
        "b2_columns_w0_w1_w2_w3",
        "planned_mac_per_block",
        "executed_mac_per_block_dense",
        "effective_gflops_planned",
        "executed_gflops_dense",
        "gate_ns",
        "gate_verdict",
        "executed_blocks",
        "tiles_per_block",
        "output_hash",
        "note",
    ];
    if let Err(err) = write_csv(
        &cfg.out_dir.join("bench-kernel.csv"),
        &kernel_header,
        &kernel_rows,
    ) {
        eprintln!("write bench-kernel.csv failed: {err}");
        std::process::exit(2);
    }

    let mut scalar_rows: Vec<Vec<String>> = Vec::new();
    for outcome in &scalar_outcomes {
        let s = stats_of(&outcome.wall);
        let gate_ns = if outcome.block == 64 {
            GATE_48K_64_NS
        } else {
            GATE_48K_128_NS
        };
        let verdict = if !outcome.ok {
            "RUN_FAILED"
        } else if s.max_ns < gate_ns {
            "PASS_LOCAL_X86_REFERENCE"
        } else {
            "OVER_GATE_LOCAL_X86_REFERENCE"
        };
        scalar_rows.push(vec![
            "scalar".to_string(),
            outcome.graph.to_string(),
            outcome.kernel.to_string(),
            outcome.block.to_string(),
            "1".to_string(),
            "1".to_string(),
            s.n.to_string(),
            s.min_ns.to_string(),
            s.p50_ns.to_string(),
            s.p95_ns.to_string(),
            s.p99_ns.to_string(),
            s.p999_ns.to_string(),
            s.max_ns.to_string(),
            s.mean_ns.to_string(),
            fmt_f64(s.cv, 4),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            gate_ns.to_string(),
            verdict.to_string(),
            outcome.executed_blocks.to_string(),
            "0".to_string(),
            format!("{:016x}", outcome.output_hash),
            outcome.note.clone(),
        ]);
    }
    if let Err(err) = write_csv(
        &cfg.out_dir.join("bench-scalar.csv"),
        &kernel_header,
        &scalar_rows,
    ) {
        eprintln!("write bench-scalar.csv failed: {err}");
        std::process::exit(2);
    }

    let budget_header = [
        "kind",
        "profile",
        "kernel",
        "workers",
        "block",
        "core_gmac_s",
        "gate_ns",
        "work_per_block_mac",
        "macs_per_second",
        "flops_per_second",
        "total_worst_ns",
        "worst_worker",
        "worker_work_0",
        "worker_work_1",
        "worker_work_2",
        "worker_work_3",
        "plan_bytes",
        "dual_graph_bytes",
        "admitted",
        "serial_fraction_q16_phase0",
        "serial_fraction_q16_phase1",
        "serial_fraction_q16_phase2",
        "status",
    ];
    let mut deadline_rows_csv: Vec<Vec<String>> = Vec::new();
    for row in &budget_rows {
        deadline_rows_csv.push(vec![
            row.kind.to_string(),
            row.profile.clone(),
            row.kernel.clone(),
            row.workers.to_string(),
            row.block.to_string(),
            row.core_gmac_s.to_string(),
            row.gate_ns.to_string(),
            row.work_per_block.to_string(),
            row.macs_per_second.to_string(),
            row.flops_per_second.to_string(),
            row.total_worst_ns.to_string(),
            row.worst_worker.to_string(),
            row.worker_work[0].to_string(),
            row.worker_work[1].to_string(),
            row.worker_work[2].to_string(),
            row.worker_work[3].to_string(),
            row.plan_bytes.to_string(),
            row.dual_graph_bytes.to_string(),
            row.admitted.to_string(),
            row.serial_fraction_q16[0].to_string(),
            row.serial_fraction_q16[1].to_string(),
            row.serial_fraction_q16[2].to_string(),
            row.status.clone(),
        ]);
    }
    if let Err(err) = write_csv(
        &cfg.out_dir.join("bench-budget-deadline.csv"),
        &budget_header,
        &deadline_rows_csv,
    ) {
        eprintln!("write bench-budget-deadline.csv failed: {err}");
        std::process::exit(2);
    }
    let mut swap_rows_csv: Vec<Vec<String>> = Vec::new();
    for row in &swap_rows {
        swap_rows_csv.push(vec![
            row.kind.to_string(),
            row.profile.clone(),
            row.admitted.to_string(),
            row.peak_cpu_ns.to_string(),
            row.peak_ram_bytes.to_string(),
            row.gate_ns.to_string(),
            row.plan_bytes.to_string(),
            row.status.clone(),
        ]);
    }
    if let Err(err) = write_csv(
        &cfg.out_dir.join("bench-budget-swap-fallback.csv"),
        &[
            "kind",
            "profile",
            "admitted",
            "peak_or_worst_cpu_ns",
            "peak_ram_bytes",
            "gate_ns",
            "plan_bytes",
            "status",
        ],
        &swap_rows_csv,
    ) {
        eprintln!("write bench-budget-swap-fallback.csv failed: {err}");
        std::process::exit(2);
    }

    // gates.json
    let mut gates_json = String::from("[\n");
    for (index, gate) in gates.iter().enumerate() {
        let sep = if index + 1 == gates.len() { "" } else { "," };
        gates_json.push_str(&format!(
            "  {{\"id\":\"{}\",\"requirement\":\"{}\",\"threshold\":\"{}\",\"measured\":\"{}\",\"status\":\"{}\",\"source\":\"{}\",\"note\":\"{}\"}}{}\n",
            json_escape(gate.id),
            json_escape(&gate.requirement),
            json_escape(&gate.threshold),
            json_escape(&gate.measured),
            json_escape(gate.status),
            json_escape(gate.source),
            json_escape(&gate.note),
            sep
        ));
    }
    gates_json.push_str("]\n");
    if let Err(err) = fs::write(cfg.out_dir.join("gates.json"), gates_json) {
        eprintln!("write gates.json failed: {err}");
        std::process::exit(2);
    }

    // summary.json
    let env_raw = cfg
        .env_json
        .as_ref()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|text| {
            text.trim()
                .trim_start_matches('\u{feff}')
                .trim()
                .to_string()
        })
        .filter(|text| text.starts_with('{'))
        .unwrap_or_else(|| "{}".to_string());
    let not_run = [
        "board RT/IRQ/cyclictest/osnoise 实测（T29）：NOT_RUN —— 需板端独占窗口",
        "板端同 kernel 1/2/4 wall speedup>=3.2（T46）：NOT_RUN —— x86 只是工装/趋势参考",
        "板端 128 帧 worst<1.3333ms / 64 帧<0.6667ms（T30）：NOT_RUN —— 需 A72 真实算力与 RT 内核",
        "已校准生产图 1h 0 xrun soak（T30）：NOT_RUN —— 需真实 ALSA/MOTU 与长窗口",
        "真实 ALSA/MOTU/CueMix/PTP/AoIP 链（T26/T29/T30）：NOT_RUN —— 本工装不触板",
        "PMU cache-miss/cycles/instructions、worker 迁移计数：NOT_RUN —— 本工装无 PMU 接口",
        "dsp_cpus 亲和/FIFO/IRQ 实写（T45/T46）：NOT_RUN —— 本机 Windows 不写内核；板端 host 启动期执行",
        "git 操作：NOT_RUN —— 任务纪律禁止",
    ];
    let counts_dag: u64 = dag_executed_blocks;
    let counts_scalar: u64 = scalar_executed;
    let summary = format!(
        concat!(
            "{{\n",
            "  \"harness\": \"{}\",\n",
            "  \"timestamp\": \"{}\",\n",
            "  \"board_measurement\": false,\n",
            "  \"run_quality\": \"{}\",\n",
            "  \"environment\": {},\n",
            "  \"config\": {{\"blocks\": {}, \"warmup\": {}, \"reps\": {}, \"available_parallelism\": {}}},\n",
            "  \"counts\": {{\"dag_block_executions\": {}, \"scalar_block_executions\": {}, \"dag_configs\": {}, \"scalar_configs\": {}, \"deadline_rows\": {}, \"swap_fallback_rows\": {}}},\n",
            "  \"model\": {{\"matrix_macs_per_sample\": 102400, \"matrix_gflops_per_second\": 9830400000, \"diag_p1024_macs_per_sample\": 7168, \"graph_41_2_4096_total_gflops_per_second\": 10518528000, \"gate_128_ns\": {}, \"gate_64_ns\": {}}},\n",
            "  \"artifacts\": [\"bench-kernel.csv\", \"bench-scalar.csv\", \"bench-budget-deadline.csv\", \"bench-budget-swap-fallback.csv\", \"gates.json\", \"summary.json\"],\n",
            "  \"not_run\": [{}]\n",
            "}}\n"
        ),
        HARNESS_VERSION,
        json_escape(&cfg.timestamp),
        run_quality,
        env_raw,
        cfg.blocks,
        cfg.warmup,
        cfg.reps,
        parallelism,
        counts_dag,
        counts_scalar,
        outcomes.len(),
        scalar_outcomes.len(),
        budget_rows.len(),
        swap_rows.len(),
        GATE_48K_128_NS,
        GATE_48K_64_NS,
        not_run
            .iter()
            .map(|item| format!("\"{}\"", json_escape(item)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Err(err) = fs::write(cfg.out_dir.join("summary.json"), summary) {
        eprintln!("write summary.json failed: {err}");
        std::process::exit(2);
    }

    println!(
        "EXECUTED dag_block_executions={counts_dag} scalar_block_executions={counts_scalar} dag_configs={} scalar_configs={}",
        outcomes.len(),
        scalar_outcomes.len()
    );
    println!("BITEXACT {bit_exact}");
    for gate in &gates {
        println!(
            "GATE {} status={} measured={}",
            gate.id, gate.status, gate.measured
        );
    }
    println!("OUT {} (csv/json written)", cfg.out_dir.display());
}
