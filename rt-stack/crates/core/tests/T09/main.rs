//! T09 workers 私有测试（WBS T09；写域 crates/core/tests/T09/）。
//!
//! 覆盖 core-v1 §7（四 worker 静态 DAG / B1-B3 / worker-ack / epoch / 2 核重准入）与
//! §13.6 T09 专项（block_start_seq 配方/Acquire-Release、expected=2/4 ack_mask、
//! 迟到 worker 旧 ack 槽标签检测），并用 T07 direct f32 kernel 做语义对拍：
//! - W=1/2/4 同一 plan 的输出逐位一致（to_bits 相等）；
//! - W=4 与 T07 direct 单线程 f32 在 T07 冻结工程门内一致；identity 图精确相等；
//! - 有界 phase barrier / cutoff / 旧槽标签 / ack 槽耗尽 / quarantine all-ack / 2 核重准入；
//! - 2 核重准入的版本发布与失败安全语义（T09-review I01/I02）。
//!
//! 证明强度（T09-review I04）：workers_rt_module_source_scan 只是 token 黑名单 + code_only
//! 整文件扫描的第一道防线，不是 RT 调用图/类型/dataflow 证明；经 crate::direct 的调用与
//! 条件分支内的分配语义未由此证明，需 T06/T26 的 rt-check/MIR/类型级 lint 覆盖调用图。
//!
//! Cargo 自动把 tests/T09/main.rs 发现为集成测试 target（不需要 public_registration 注册）。
//! 运行：cargo test -p khaen-core workers -- --nocapture

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread;

use khaen_core::abi::{KhaenBlockAckV1, KhaenBlockDescV1, ABI_VERSION};
use khaen_core::direct::{
    direct_block_f32, DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_PARTICIPANTS,
    DIRECT_MAX_ROLES, DIRECT_NO_SOURCE,
};
use khaen_core::rt::RtStatus;
use khaen_core::workers::{
    ack_all, ack_mask, ack_phase, ack_phase_labeled, consume_block_start, execute_block_scheduled,
    master_start_block, publish_block_start, read_block_start, set_cutoff, wait_block_start,
    wait_phase_done, wait_phases, worker_begin_block, worker_phase_b1, worker_phase_barrier,
    worker_record_block_done, worker_record_late, worker_state_zero, KhaenRtControlV1,
    WorkerAckSlotPool, WorkerBlockStart, WorkerDagPlan, WorkerMode, WorkerPartials,
    WorkerQuarantine, WorkerReAdmission, WorkerScheduleStats, RT_E_ACK_SLOT_EXHAUSTED,
    WORKER_ACK_FLAG_VALID, WORKER_ACK_SLOTS, WORKER_ACK_SLOT_ALL_ACK, WORKER_ACK_SLOT_FREE,
    WORKER_ACK_SLOT_INIT, WORKER_ACK_SLOT_RECYCLABLE, WORKER_ACK_SLOT_WAIT, WORKER_BLOCK_MAX,
    WORKER_CONTROL_FLAG_TWO_WORKER, WORKER_COUNT_MAX, WORKER_PARTIAL_LANE_STRIDE,
    WORKER_PARTIAL_ROLE_STRIDE, WORKER_PARTIAL_SAMPLES, WORKER_PHASE_COUNT,
    WORKER_QUARANTINE_SLOTS, WORKER_REDUCE_LANES, WORKER_ROLES_MAX,
    WORKER_STAT_FLAG_LAST_BLOCK_LATE, WORKER_SWITCH_ABORT,
};

// ---------------------------------------------------------------------------
// 合成参考图（与 T03 fixture 同语义；测试内构造，不依赖 JSON fixture）
// ---------------------------------------------------------------------------

struct Graph {
    topo: DirectTopology,
    a: Vec<f32>,
    g: Vec<f32>,
    p: Vec<f32>,
}

impl Graph {
    fn coeffs(&self) -> DirectCoeffsF32<'_> {
        DirectCoeffsF32 {
            a_fir: &self.a,
            g: &self.g,
            p: &self.p,
        }
    }
    /// 完整 factorized 图长度 L_total = L_A + L_G + L_P - 2。
    fn total_taps(&self) -> usize {
        self.topo.l_a as usize + self.topo.l_g as usize + self.topo.l_p as usize - 2
    }
    fn flush(&self) -> usize {
        self.total_taps() - 1
    }
    fn expected_len(&self, input_len: usize) -> usize {
        input_len + self.total_taps() - 1
    }
}

fn blank_topo(
    n_src: u16,
    n_role: u16,
    container: u16,
    d: u32,
    l_a: u16,
    l_g: u16,
    l_p: u16,
    m: u16,
) -> DirectTopology {
    DirectTopology {
        n_src,
        n_role,
        container_channels: container,
        d_samples: d,
        l_a,
        l_g,
        l_p,
        m,
        src_of_role: [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES],
        participants: [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS],
        scatter: [0u16; DIRECT_MAX_ROLES],
    }
}

fn g_set(g: &mut [f32], m: usize, l_g: usize, row: usize, col: usize, tap: usize, value: f32) {
    g[(row * m + col) * l_g + tap] = value;
}

/// 4.1.2 目标布局：7 角色（FL/FR/SL/SR/HFL/HFR/SUB1）、8 容器、ch6 空、D=2、5 参与者。
fn graph_4_1_2() -> Graph {
    let m = 5usize;
    let l_g = 3usize;
    let l_p = 2usize;
    let mut topo = blank_topo(7, 7, 8, 2, 1, l_g as u16, l_p as u16, m as u16);
    let scatter = [0u16, 1, 2, 3, 4, 5, 7];
    let participants = [0u16, 1, 2, 3, 6];
    let mut j = 0usize;
    while j < 7 {
        topo.src_of_role[j] = j as u16;
        topo.scatter[j] = scatter[j];
        j += 1;
    }
    let mut index = 0usize;
    while index < m {
        topo.participants[index] = participants[index];
        index += 1;
    }
    let a = vec![1.0f32; 7];
    let mut g = vec![0.0f32; m * m * l_g];
    g_set(&mut g, m, l_g, 0, 0, 0, 0.5);
    g_set(&mut g, m, l_g, 1, 1, 1, 0.25);
    g_set(&mut g, m, l_g, 1, 3, 2, -0.4);
    g_set(&mut g, m, l_g, 2, 0, 1, 0.2);
    g_set(&mut g, m, l_g, 4, 4, 0, 0.75);
    g_set(&mut g, m, l_g, 4, 1, 2, 0.125);
    g_set(&mut g, m, l_g, 3, 2, 0, -0.3);
    // P：每输出角色对角 FIR（role0 [1,-1]、role2 增益 2，其余 [1,0]）。
    let mut p = vec![0.0f32; 7 * l_p];
    let mut j = 0usize;
    while j < 7 {
        p[j * l_p] = if j == 2 { 2.0 } else { 1.0 };
        j += 1;
    }
    p[1] = -1.0;
    Graph { topo, a, g, p }
}

/// 2x2 offdiag + D=1 + 尾部（T03 C07 同构图）。
fn graph_2x2_offdiag() -> Graph {
    let m = 2usize;
    let l_g = 2usize;
    let l_p = 2usize;
    let mut topo = blank_topo(2, 2, 2, 1, 1, l_g as u16, l_p as u16, m as u16);
    topo.src_of_role[0] = 0;
    topo.src_of_role[1] = 1;
    topo.scatter[0] = 0;
    topo.scatter[1] = 1;
    topo.participants[0] = 0;
    topo.participants[1] = 1;
    let a = vec![1.0f32, 1.0];
    let mut g = vec![0.0f32; m * m * l_g];
    g_set(&mut g, m, l_g, 0, 1, 0, 0.5);
    g_set(&mut g, m, l_g, 1, 0, 0, 0.25);
    g_set(&mut g, m, l_g, 1, 1, 1, 0.5);
    let p = vec![1.0f32, -1.0, 1.0, 0.0];
    Graph { topo, a, g, p }
}

/// 4.1.2 identity 图（G=I、保护基线 D=2、ch6 恒 0；T03 C13 同构图）。
fn graph_identity_7ch() -> Graph {
    let m = 5usize;
    let l_g = 3usize;
    let mut topo = blank_topo(7, 7, 8, 2, 1, l_g as u16, 1, m as u16);
    let scatter = [0u16, 1, 2, 3, 4, 5, 7];
    let participants = [0u16, 1, 2, 3, 6];
    let mut j = 0usize;
    while j < 7 {
        topo.src_of_role[j] = j as u16;
        topo.scatter[j] = scatter[j];
        j += 1;
    }
    let mut index = 0usize;
    while index < m {
        topo.participants[index] = participants[index];
        index += 1;
    }
    Graph {
        topo,
        a: vec![1.0f32; 7],
        g: vec![0.0f32; m * m * l_g],
        p: vec![1.0f32; 7],
    }
}

// ---------------------------------------------------------------------------
// 流式驱动/对拍工具
// ---------------------------------------------------------------------------

fn lcg_input(channels: usize, len: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed | 1;
    let mut out = vec![vec![0.0f32; len]; channels];
    let mut c = 0usize;
    while c < channels {
        let mut n = 0usize;
        while n < len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = ((state >> 11) as f64) / ((1u64 << 53) as f64);
            out[c][n] = (unit * 1.8 - 0.9) as f32;
            n += 1;
        }
        c += 1;
    }
    out
}

fn pad_planar(input: &[Vec<f32>], flush: usize, block: usize) -> (Vec<f32>, usize) {
    let channels = input.len();
    let raw_len = input[0].len() + flush;
    let stride = ((raw_len + block - 1) / block) * block;
    let mut planar = vec![0.0f32; channels * stride];
    let mut c = 0usize;
    while c < channels {
        let mut n = 0usize;
        while n < input[c].len() {
            planar[c * stride + n] = input[c][n];
            n += 1;
        }
        c += 1;
    }
    (planar, stride)
}

fn make_ack(
    block_seq: u64,
    epoch_id: u64,
    plan_version: u32,
    expected_workers: u8,
) -> KhaenBlockAckV1 {
    KhaenBlockAckV1 {
        flags: WORKER_ACK_FLAG_VALID,
        ..KhaenBlockAckV1::new(block_seq, epoch_id, plan_version, expected_workers)
    }
}

fn run_dag_with_plan(
    graph: &Graph,
    plan: &WorkerDagPlan,
    block: usize,
    input: &[Vec<f32>],
    flush: usize,
    epoch_id: u64,
) -> Result<Vec<Vec<f32>>, RtStatus> {
    let coeffs = graph.coeffs();
    let layout = plan.layout;
    let mut state = vec![0.0f32; layout.state_samples()];
    let (planar, stride) = pad_planar(input, flush, block);
    let n_blocks = stride / block;
    let containers = graph.topo.container_channels as usize;
    let out_stride = block;
    let mut output = vec![0.0f32; containers * out_stride];
    let mut partials = Box::new(WorkerPartials::new());
    let bad = AtomicU32::new(0);
    let mut stats = WorkerScheduleStats::default();
    let mut collected: Vec<Vec<f32>> = vec![Vec::new(); containers];
    let mut seq = 0u64;
    while seq < n_blocks as u64 {
        let ack = make_ack(seq, epoch_id, plan.plan_version, plan.expected_workers);
        let status = execute_block_scheduled(
            plan,
            &coeffs,
            &mut state,
            &planar,
            stride,
            &mut output,
            out_stride,
            epoch_id,
            seq,
            block,
            &ack,
            &mut partials,
            &bad,
            1_000_000,
            &mut stats,
        );
        if !status.is_ok() {
            return Err(status);
        }
        let mut c = 0usize;
        while c < containers {
            let mut n = 0usize;
            while n < block {
                collected[c].push(output[c * out_stride + n]);
                n += 1;
            }
            c += 1;
        }
        seq += 1;
    }
    let total = graph.expected_len(input[0].len());
    let mut c = 0usize;
    while c < containers {
        if collected[c].len() > total {
            collected[c].truncate(total);
        }
        c += 1;
    }
    Ok(collected)
}

fn run_dag_stream(
    graph: &Graph,
    workers: u8,
    block: usize,
    input: &[Vec<f32>],
    flush: usize,
    plan_version: u32,
    epoch_id: u64,
) -> Result<Vec<Vec<f32>>, RtStatus> {
    let plan = match WorkerDagPlan::build(&graph.topo, block as u16, workers, plan_version) {
        Ok(value) => value,
        Err(err) => return Err(err),
    };
    run_dag_with_plan(graph, &plan, block, input, flush, epoch_id)
}

fn run_direct_stream(
    graph: &Graph,
    block: usize,
    input: &[Vec<f32>],
    flush: usize,
) -> Vec<Vec<f32>> {
    let topo = &graph.topo;
    let layout = match DirectLayout::build(topo) {
        Ok(value) => value,
        Err(_) => panic!("direct layout build failed"),
    };
    let coeffs = graph.coeffs();
    let mut state = vec![0.0f32; layout.state_samples()];
    let (planar, stride) = pad_planar(input, flush, block);
    let n_blocks = stride / block;
    let containers = topo.container_channels as usize;
    let mut output = vec![0.0f32; containers * block];
    let mut collected: Vec<Vec<f32>> = vec![Vec::new(); containers];
    let mut b = 0usize;
    while b < n_blocks {
        let status = direct_block_f32(
            topo,
            &layout,
            &coeffs,
            &mut state,
            &planar,
            stride,
            &mut output,
            block,
            block,
            block,
        );
        assert!(status.is_ok(), "direct block failed: {:?}", status);
        let mut c = 0usize;
        while c < containers {
            let mut n = 0usize;
            while n < block {
                collected[c].push(output[c * block + n]);
                n += 1;
            }
            c += 1;
        }
        b += 1;
    }
    let total = graph.expected_len(input[0].len());
    let mut c = 0usize;
    while c < containers {
        if collected[c].len() > total {
            collected[c].truncate(total);
        }
        c += 1;
    }
    collected
}

fn assert_bits_equal(label: &str, left: &[Vec<f32>], right: &[Vec<f32>]) {
    assert_eq!(left.len(), right.len(), "{}: channel count", label);
    let mut c = 0usize;
    while c < left.len() {
        assert_eq!(left[c].len(), right[c].len(), "{}: ch{} length", label, c);
        let mut n = 0usize;
        while n < left[c].len() {
            assert_eq!(
                left[c][n].to_bits(),
                right[c][n].to_bits(),
                "{}: ch{} n{} {} vs {}",
                label,
                c,
                n,
                left[c][n],
                right[c][n]
            );
            n += 1;
        }
        c += 1;
    }
}

fn compare_tolerance(
    label: &str,
    got: &[Vec<f32>],
    reference: &[Vec<f32>],
    rel_l2_max: f64,
    max_abs_factor: f64,
) -> (f64, f64) {
    assert_eq!(got.len(), reference.len(), "{}: channel count", label);
    let mut diff_sq = 0.0f64;
    let mut ref_sq = 0.0f64;
    let mut max_abs_diff = 0.0f64;
    let mut peak = 0.0f64;
    let mut c = 0usize;
    while c < got.len() {
        assert_eq!(
            got[c].len(),
            reference[c].len(),
            "{}: ch{} length",
            label,
            c
        );
        let mut n = 0usize;
        while n < got[c].len() {
            let g = got[c][n] as f64;
            let r = reference[c][n] as f64;
            let d = (g - r).abs();
            diff_sq += d * d;
            ref_sq += r * r;
            if d > max_abs_diff {
                max_abs_diff = d;
            }
            if r.abs() > peak {
                peak = r.abs();
            }
            n += 1;
        }
        c += 1;
    }
    let rel_l2 = if ref_sq > 0.0 {
        (diff_sq / ref_sq).sqrt()
    } else {
        diff_sq.sqrt()
    };
    let allowed_abs = max_abs_factor * (1.0f64).max(peak);
    assert!(
        rel_l2 <= rel_l2_max,
        "{}: rel L2 {} > {}",
        label,
        rel_l2,
        rel_l2_max
    );
    assert!(
        max_abs_diff <= allowed_abs,
        "{}: max abs diff {} > {}",
        label,
        max_abs_diff,
        allowed_abs
    );
    (rel_l2, max_abs_diff)
}

fn base_desc(epoch_id: u64, block_seq: u64, plan_version: u32, block: u16) -> KhaenBlockDescV1 {
    KhaenBlockDescV1 {
        epoch_id,
        block_seq,
        frame_index: block_seq * block as u64,
        plan_version,
        flags: 1,
        valid_frames: block as u32,
        block_size: block,
        ..KhaenBlockDescV1::ZERO
    }
}

// ---------------------------------------------------------------------------
// 单 block 驱动（测试用；返回 status/state/output/stats，便于负向断言）
// ---------------------------------------------------------------------------

fn one_block(
    graph: &Graph,
    plan: &WorkerDagPlan,
    block: usize,
    input: &[Vec<f32>],
    epoch_id: u64,
    block_seq: u64,
    ack: &KhaenBlockAckV1,
    output_fill: f32,
    spin_max_iters: u32,
) -> (RtStatus, Vec<f32>, Vec<f32>, WorkerScheduleStats) {
    let coeffs = graph.coeffs();
    let layout = plan.layout;
    let mut state = vec![0.0f32; layout.state_samples()];
    let (planar, stride) = pad_planar(input, graph.flush(), block);
    let containers = graph.topo.container_channels as usize;
    let mut output = vec![output_fill; containers * block];
    let mut partials = Box::new(WorkerPartials::new());
    let bad = AtomicU32::new(0);
    let mut stats = WorkerScheduleStats::default();
    let status = execute_block_scheduled(
        plan,
        &coeffs,
        &mut state,
        &planar,
        stride,
        &mut output,
        block,
        epoch_id,
        block_seq,
        block,
        ack,
        &mut partials,
        &bad,
        spin_max_iters,
        &mut stats,
    );
    (status, state, output, stats)
}

// ---------------------------------------------------------------------------
// 1. plan 任务表/容量/负向
// ---------------------------------------------------------------------------

#[test]
fn workers_plan_contract_and_task_tables() {
    let graph = graph_4_1_2();
    let plan4 = WorkerDagPlan::build(&graph.topo, 128, 4, 7).expect("build w4");
    let plan2 = WorkerDagPlan::build(&graph.topo, 128, 2, 8).expect("build w2");
    let plan1 = WorkerDagPlan::build(&graph.topo, 128, 1, 9).expect("build w1");
    fn assert_copy<T: Copy>(_: T) {}
    assert_copy(plan4);

    assert_eq!(plan4.expected_workers, 4);
    assert_eq!(plan4.reduce_lanes as usize, WORKER_REDUCE_LANES);
    assert_eq!(plan4.phase_task_count(0), 7);
    assert_eq!(plan4.phase_task_count(1), 7);
    assert_eq!(plan4.phase_task_count(2), 7);
    assert_eq!(plan4.phase_task_count(3), 0);
    assert_eq!(plan4.total_tasks(), 21);

    // B2 输入列 -> reduce lane 的静态连续分块（7 列 / 4 lane）。
    let expected_lane = [0u8, 0, 1, 1, 2, 2, 3];
    let mut k = 0usize;
    while k < 7 {
        assert_eq!(plan4.k_lane[k], expected_lane[k], "k_lane[{}]", k);
        k += 1;
    }
    assert_eq!(plan4.lane_worker, [0u8, 1, 2, 3]);
    assert_eq!(plan2.lane_worker, [0u8, 1, 0, 1]);
    assert_eq!(plan1.lane_worker, [0u8, 0, 0, 0]);
    assert_eq!(plan4.lane_active, [1u8, 1, 1, 1]);
    assert_eq!(plan2.lane_active, [1u8, 1, 1, 1]);
    assert_eq!(plan1.lane_active, [1u8, 1, 1, 1]);
    let mut j = 0usize;
    while j < 7 {
        assert_eq!(plan4.u_owner[j] as usize, j % 4);
        assert_eq!(plan4.p_owner[j] as usize, j % 4);
        assert_eq!(plan2.u_owner[j] as usize, j % 2);
        assert_eq!(plan1.u_owner[j], 0);
        j += 1;
    }

    // 每个 tile 恰有一个 owner；task_owner 越界返回 None（不 panic）。
    let mut phase = 0u8;
    while phase < WORKER_PHASE_COUNT as u8 {
        let mut task = 0u8;
        let mut total = 0u32;
        while task < 7 {
            assert!(plan4.task_owner(phase, task).is_some());
            total += 1;
            task += 1;
        }
        assert_eq!(total, plan4.phase_task_count(phase) as u32);
        assert!(plan4.task_owner(phase, 7).is_none());
        assert!(plan4.task_owner(3, 0).is_none());
        let mut worker = 0u8;
        let mut sum = 0u32;
        while worker < 4 {
            sum += plan4.worker_task_count(phase, worker) as u32;
            worker += 1;
        }
        assert_eq!(sum, 7, "phase {} worker task sum", phase);
        phase += 1;
    }
    // B2 tile 数按 lane：lane0/1/2 各 2 列，lane3 1 列 -> worker 0..3 = [2,2,2,1]。
    let mut worker = 0u8;
    let expected_b2 = [2u8, 2, 2, 1];
    while worker < 4 {
        assert_eq!(
            plan4.worker_task_count(1, worker),
            expected_b2[worker as usize]
        );
        worker += 1;
    }
    // B1/B3 按角色 round-robin：7 角色 -> [2,2,2,1]。
    worker = 0;
    while worker < 4 {
        assert_eq!(
            plan4.worker_task_count(0, worker),
            expected_b2[worker as usize]
        );
        assert_eq!(
            plan4.worker_task_count(2, worker),
            expected_b2[worker as usize]
        );
        worker += 1;
    }

    // 串行比例如实记录：tasks=7 > W 时 (7-W)/7；1/2 核 profile。
    assert_eq!(
        plan4.serial_fraction_q16(0),
        (((7u64 - 4) * 65536) / 7) as u32
    );
    assert_eq!(
        plan2.serial_fraction_q16(0),
        (((7u64 - 2) * 65536) / 7) as u32
    );
    assert_eq!(
        plan1.serial_fraction_q16(0),
        (((7u64 - 1) * 65536) / 7) as u32
    );
    assert_eq!(plan4.phase_parallel_workers(0), 4);
    assert_eq!(plan2.phase_parallel_workers(0), 2);
    assert_eq!(plan1.phase_parallel_workers(0), 1);
    assert!(plan2.is_two_core_profile());
    assert!(!plan4.is_two_core_profile());
    assert_eq!(plan1.worker_task_count(0, 0), 7);

    // 负向：worker 数/block 大小/拓扑容量/build 校验。
    assert_eq!(
        WorkerDagPlan::build(&graph.topo, 128, 3, 1).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );
    assert_eq!(
        WorkerDagPlan::build(&graph.topo, 0, 4, 1).unwrap_err(),
        RtStatus::ERR_BLOCK_SIZE
    );
    assert_eq!(
        WorkerDagPlan::build(&graph.topo, 129, 4, 1).unwrap_err(),
        RtStatus::ERR_BLOCK_SIZE
    );
    let mut bad_topo = graph_2x2_offdiag().topo;
    bad_topo.l_g = 2;
    bad_topo.d_samples = 2;
    assert_eq!(
        WorkerDagPlan::build(&bad_topo, 128, 4, 1).unwrap_err(),
        RtStatus::ERR_TAPS
    );
    let mut corrupted = plan4;
    corrupted.expected_workers = 3;
    assert_eq!(corrupted.validate(), RtStatus::ERR_ADMISSION_SCHED);
    corrupted = plan4;
    corrupted.k_lane[0] = 3;
    assert_eq!(corrupted.validate(), RtStatus::ERR_BAD_LAYOUT);
    corrupted = plan4;
    corrupted.lane_active[0] = 0;
    assert_eq!(corrupted.validate(), RtStatus::ERR_BAD_LAYOUT);
    corrupted = plan4;
    corrupted.u_owner[1] = 4;
    assert_eq!(corrupted.validate(), RtStatus::ERR_BAD_LAYOUT);
}

// ---------------------------------------------------------------------------
// 2. ACK_MASK 参数化 + 有界 phase 等待
// ---------------------------------------------------------------------------

#[test]
fn workers_ack_mask_parameterized_and_bounded_waits() {
    assert_eq!(ack_mask(1), 0x1);
    assert_eq!(ack_mask(2), 0x3);
    assert_eq!(ack_mask(4), 0xF);
    assert_eq!(ack_mask(4), khaen_core::rt::ACK_MASK_ALL);

    // expected=2：0x3 到齐才算 all-ack；先缺一位必须有界失败。
    let ack2 = make_ack(0, 1, 1, 2);
    assert!(ack_phase(&ack2, 0, 0).is_ok());
    assert!(ack_phase(&ack2, 1, 0).is_ok());
    assert!(ack_phase(&ack2, 0, 1).is_ok());
    assert!(ack_phase(&ack2, 1, 2).is_ok());
    assert_eq!(wait_phases(&ack2, 2, 8), RtStatus::RT_E_PARTIAL_LATE);
    assert!(ack_phase(&ack2, 1, 1).is_ok());
    assert!(ack_phase(&ack2, 0, 2).is_ok());
    assert!(wait_phases(&ack2, 2, 0).is_ok());
    assert!(ack_all(&ack2));
    // 位图越出 expected 掩码 -> ABI runtime（其余位必须 0）。
    let stale = make_ack(0, 1, 1, 2);
    stale.phase_done[0].fetch_or(0xC, Ordering::Release);
    assert_eq!(wait_phases(&stale, 2, 0), RtStatus::RT_E_ABI_RUNTIME);
    // 越权 worker / 非法 phase。
    assert_eq!(ack_phase(&ack2, 2, 0), RtStatus::ERR_ADMISSION_SCHED);
    assert_eq!(ack_phase(&ack2, 0, 3), RtStatus::ERR_BAD_LAYOUT);

    // expected=4：必须 0xF（0x7 不够；0xF 到齐后才 OK）。
    let ack4 = make_ack(0, 1, 1, 4);
    let mut worker = 0u8;
    while worker < 3 {
        let mut phase = 0u8;
        while phase < 3 {
            assert!(ack_phase(&ack4, worker, phase).is_ok());
            phase += 1;
        }
        worker += 1;
    }
    assert_eq!(wait_phases(&ack4, 4, 16), RtStatus::RT_E_PARTIAL_LATE);
    let mut phase = 0u8;
    while phase < 3 {
        assert!(ack_phase(&ack4, 3, phase).is_ok());
        phase += 1;
    }
    assert!(wait_phases(&ack4, 4, 0).is_ok());
    assert_eq!(ack_phase(&ack4, 4, 0), RtStatus::ERR_ADMISSION_SCHED);
    assert_eq!(
        wait_phase_done(&ack4, 0, 2, 8),
        RtStatus::RT_E_PLAN_MISMATCH
    );

    // cutoff 后即使位图到齐也必须返回过期（master 不发布半图）。
    assert_eq!(set_cutoff(&ack4), RtStatus::OK);
    assert_eq!(wait_phases(&ack4, 4, 0), RtStatus::RT_E_PARTIAL_LATE);
    assert!(!ack_all(&ack4));
}

// ---------------------------------------------------------------------------
// 3. block_start_seq 配方（Release/Acquire + 序号 +1 + 安全请求）
// ---------------------------------------------------------------------------

#[test]
fn workers_block_start_seq_recipe() {
    let ctl = KhaenRtControlV1::new();
    assert_eq!(ctl.block_start_seq.load(Ordering::Acquire), 0);
    assert_eq!(wait_block_start(&ctl, 0, 8), Err(RtStatus::RT_E_CUTOFF));

    assert_eq!(publish_block_start(&ctl, 3, 10, 7, 1), RtStatus::OK);
    assert_eq!(ctl.block_start_seq.load(Ordering::Acquire), 1);
    let start = read_block_start(&ctl);
    assert_eq!(
        start,
        WorkerBlockStart {
            epoch_id: 3,
            frame_index: 10,
            plan_version: 7,
            slot: 1
        }
    );
    assert_eq!(wait_block_start(&ctl, 0, 8), Ok(1));
    assert_eq!(consume_block_start(&ctl, 0, 8), Ok((1, start)));

    assert_eq!(publish_block_start(&ctl, 3, 138, 7, 2), RtStatus::OK);
    let second = consume_block_start(&ctl, 1, 8).expect("second block start");
    assert_eq!(second.0, 2);
    assert_eq!(second.1.slot, 2);
    assert_eq!(second.1.frame_index, 138);

    // 跳变（master 不该连续发布两块）必须被 worker 拒绝，不静默跳读。
    assert_eq!(publish_block_start(&ctl, 3, 266, 7, 3), RtStatus::OK);
    assert_eq!(publish_block_start(&ctl, 3, 394, 7, 3), RtStatus::OK);
    assert_eq!(
        consume_block_start(&ctl, 2, 8).unwrap_err(),
        RtStatus::RT_E_INVARIANT
    );

    // watchdog/control 置 safety_request 后 worker 有界退出（不得继续处理）。
    ctl.safety_request.store(1, Ordering::Release);
    assert_eq!(wait_block_start(&ctl, 4, 8), Err(RtStatus::RT_E_WATCHDOG));

    // 结构校验：魔数/版本/大小非法 -> ERR_BAD_LAYOUT（不 panic）。
    let mut bad = KhaenRtControlV1::new();
    bad.magic = 0;
    assert_eq!(wait_block_start(&bad, 0, 8), Err(RtStatus::ERR_BAD_LAYOUT));
    assert_eq!(
        publish_block_start(&bad, 1, 0, 1, 0),
        RtStatus::ERR_BAD_LAYOUT
    );

    // worker_begin_block：plan 版本/two_worker profile 必须与 ctl 一致。
    let graph = graph_4_1_2();
    let plan4 = WorkerDagPlan::build(&graph.topo, 128, 4, 11).expect("build");
    let ctl2 = KhaenRtControlV1::new();
    assert_eq!(publish_block_start(&ctl2, 5, 0, 11, 0), RtStatus::OK);
    let ready = worker_begin_block(&ctl2, &plan4, 0, 8).expect("worker begin");
    assert_eq!(ready.0, 1);
    assert_eq!(ready.1.plan_version, 11);
    assert_eq!(publish_block_start(&ctl2, 5, 128, 11, 0), RtStatus::OK);
    ctl2.flags
        .store(WORKER_CONTROL_FLAG_TWO_WORKER, Ordering::Release);
    assert_eq!(
        worker_begin_block(&ctl2, &plan4, 1, 8).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
}

// ---------------------------------------------------------------------------
// 4. 真实线程 Acquire/Release 握手（producer=master, consumer=worker）
// ---------------------------------------------------------------------------

#[test]
fn workers_block_start_thread_handoff() {
    let ctl = KhaenRtControlV1::new();
    let done = AtomicU64::new(0);
    let errors = AtomicU32::new(0);
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut last = 0u64;
            let mut index = 1u64;
            while index <= 256 {
                match consume_block_start(&ctl, last, 200_000_000) {
                    Ok((seq, start)) => {
                        if seq != index
                            || start.epoch_id != 42
                            || start.frame_index != (index - 1) * 128
                            || start.plan_version != 9
                            || start.slot != 3
                        {
                            errors.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        last = seq;
                        done.store(seq, Ordering::Release);
                    }
                    Err(_) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
                index += 1;
            }
        });
        let mut index = 1u64;
        while index <= 256 {
            let mut spins = 0u64;
            while done.load(Ordering::Acquire) + 1 < index {
                spins += 1;
                if spins > 100_000_000 {
                    panic!("consumer stalled before block {}", index);
                }
                std::thread::yield_now();
            }
            assert_eq!(
                publish_block_start(&ctl, 42, (index - 1) * 128, 9, 3),
                RtStatus::OK
            );
            index += 1;
        }
    });
    assert_eq!(errors.load(Ordering::Acquire), 0, "handoff errors");
    assert_eq!(done.load(Ordering::Acquire), 256);
}

// ---------------------------------------------------------------------------
// 5. 迟到 worker 旧 ack 槽标签检测（§7.3/I08）
// ---------------------------------------------------------------------------

#[test]
fn workers_ack_phase_and_late_slot_label() {
    let ack = make_ack(9, 5, 4, 4);
    assert!(ack_phase_labeled(&ack, 0, 0, 5, 9, 4).is_ok());
    assert_eq!(ack.phase_done[0].load(Ordering::Acquire), 0x1);
    // 旧 block 的迟到 worker：不得改当前槽，返回迟到错误。
    assert_eq!(
        ack_phase_labeled(&ack, 1, 0, 5, 8, 4),
        RtStatus::RT_E_PARTIAL_LATE
    );
    assert_eq!(ack.phase_done[0].load(Ordering::Acquire), 0x1);
    // 旧 epoch / 旧 plan_version 同样拒绝。
    assert_eq!(
        ack_phase_labeled(&ack, 2, 1, 4, 9, 4),
        RtStatus::RT_E_EPOCH_MISMATCH
    );
    assert_eq!(
        ack_phase_labeled(&ack, 2, 1, 5, 9, 3),
        RtStatus::RT_E_PARTIAL_LATE
    );
    assert_eq!(ack.phase_done[1].load(Ordering::Acquire), 0);
    // 标签正确后正常写位。
    assert!(ack_phase_labeled(&ack, 1, 0, 5, 9, 4).is_ok());
    assert_eq!(ack.phase_done[0].load(Ordering::Acquire), 0x3);
    // 迟到 worker 只记本地 stats（不写 ack 槽）。
    let mut stats = khaen_core::abi::KhaenWorkerStatsV1::default();
    stats.flags = 0;
    worker_record_late(&mut stats);
    assert_eq!(stats.late_blocks, 1);
    assert_eq!(
        stats.flags & WORKER_STAT_FLAG_LAST_BLOCK_LATE,
        WORKER_STAT_FLAG_LAST_BLOCK_LATE
    );
    worker_record_block_done(&mut stats);
    assert_eq!(stats.blocks_done, 1);
    assert_eq!(stats.flags & WORKER_STAT_FLAG_LAST_BLOCK_LATE, 0);
    assert_eq!(ack.flags, WORKER_ACK_FLAG_VALID);
}

// ---------------------------------------------------------------------------
// 6. ack 槽生命周期 + 耗尽（§7.3/§7.6）
// ---------------------------------------------------------------------------

#[test]
fn workers_ack_slot_lifecycle_and_exhaustion() {
    let mut pool = WorkerAckSlotPool::new();
    assert_eq!(pool.occupied(), 0);
    let mut acquired = [0usize; WORKER_ACK_SLOTS];
    let mut index = 0usize;
    while index < WORKER_ACK_SLOTS {
        let slot = pool
            .try_acquire(index as u64, 7, 3, 4, 1000 + index as u64)
            .expect("acquire");
        assert_eq!(slot, index);
        acquired[index] = slot;
        assert_eq!(pool.state(slot), WORKER_ACK_SLOT_INIT);
        assert_eq!(pool.slot(slot).expect("slot").block_seq, index as u64);
        index += 1;
    }
    assert_eq!(pool.occupied(), WORKER_ACK_SLOTS as u32);
    assert_eq!(
        pool.try_acquire(9, 7, 3, 4, 0).unwrap_err(),
        RT_E_ACK_SLOT_EXHAUSTED
    );
    assert_eq!(
        pool.try_acquire(9, 7, 3, 3, 0).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );

    // 槽 0 生命周期：INIT -> WAIT -> ALL_ACK -> RECYCLABLE -> FREE。
    assert_eq!(pool.mark_published(0), RtStatus::OK);
    assert_eq!(pool.state(0), WORKER_ACK_SLOT_WAIT);
    // 未 all-ack 时不能置 ALL_ACK。
    assert_eq!(pool.mark_all_ack(0), RtStatus::RT_E_SLOT_STATE);
    let slot = pool.slot(0).expect("slot 0");
    let mut worker = 0u8;
    while worker < 4 {
        let mut phase = 0u8;
        while phase < 3 {
            assert!(ack_phase(slot, worker, phase).is_ok());
            phase += 1;
        }
        worker += 1;
    }
    assert_eq!(pool.mark_all_ack(0), RtStatus::OK);
    assert_eq!(pool.state(0), WORKER_ACK_SLOT_ALL_ACK);
    assert_eq!(pool.release_recyclable(0), RtStatus::OK);
    assert_eq!(pool.state(0), WORKER_ACK_SLOT_RECYCLABLE);
    assert_eq!(pool.recycle_free(0), RtStatus::OK);
    assert_eq!(pool.state(0), WORKER_ACK_SLOT_FREE);
    assert!(pool.try_acquire(10, 7, 3, 4, 0).is_ok());
    // 非法索引/非法状态迁移不 panic。
    assert_eq!(pool.mark_published(9), RtStatus::ERR_BAD_LAYOUT);
    assert_eq!(pool.recycle_free(1), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(pool.state(9), u32::MAX);
}

// ---------------------------------------------------------------------------
// 7. 有界 phase barrier（cutoff/迟到/spin 上界）
// ---------------------------------------------------------------------------

#[test]
fn workers_phase_barrier_bounded_and_cutoff() {
    let ack = make_ack(0, 2, 3, 4);
    assert!(ack_phase(&ack, 0, 0).is_ok());
    assert!(ack_phase(&ack, 1, 0).is_ok());
    // 0 次 spin 的等待立即揭示未到齐（有界、不阻塞）。
    assert_eq!(wait_phase_done(&ack, 0, 4, 0), RtStatus::RT_E_PARTIAL_LATE);
    assert!(ack_phase(&ack, 2, 0).is_ok());
    assert_eq!(wait_phase_done(&ack, 0, 4, 8), RtStatus::RT_E_PARTIAL_LATE);
    assert!(ack_phase(&ack, 3, 0).is_ok());
    assert_eq!(wait_phase_done(&ack, 0, 4, 8), RtStatus::OK);
    // worker_phase_barrier 是同一语义别名。
    assert_eq!(worker_phase_barrier(&ack, 0, 4, 8), RtStatus::OK);
    // cutoff 置位后即使已到齐也整块过期。
    assert_eq!(set_cutoff(&ack), RtStatus::OK);
    assert_eq!(
        worker_phase_barrier(&ack, 0, 4, 0),
        RtStatus::RT_E_PARTIAL_LATE
    );
    // expected 与 ack 标签不一致 -> plan mismatch（不能拿 2 核掩码等 4 核）。
    let ack2 = make_ack(0, 2, 3, 2);
    assert_eq!(
        wait_phase_done(&ack2, 0, 4, 0),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert_eq!(
        wait_phase_done(&ack2, 0, 3, 0),
        RtStatus::ERR_ADMISSION_SCHED
    );
}

// ---------------------------------------------------------------------------
// 8. W=1/2/4 同 plan 逐位一致（固定 reduce 顺序与 worker 数无关）
// ---------------------------------------------------------------------------

#[test]
fn workers_dag_w1_w2_w4_bit_identical() {
    let graph = graph_4_1_2();
    let input = lcg_input(7, 130, 0x5151);
    let flush = graph.flush();
    let out1 = run_dag_stream(&graph, 1, 128, &input, flush, 7, 21).expect("w1");
    let out2 = run_dag_stream(&graph, 2, 128, &input, flush, 7, 21).expect("w2");
    let out4 = run_dag_stream(&graph, 4, 128, &input, flush, 7, 21).expect("w4");
    assert_bits_equal("w4-vs-w1", &out4, &out1);
    assert_bits_equal("w2-vs-w1", &out2, &out1);
    println!(
        "T09 w-equality PASS: 4.1.2 graph, 8 containers, {} samples/channel; W1==W2==W4 bit-identical",
        out4[0].len()
    );
    // 未映射容器通道 ch6 必须严格 +0.0（不复制节目、不造声道）。
    let mut n = 0usize;
    while n < out4[6].len() {
        assert_eq!(out4[6][n].to_bits(), 0.0f32.to_bits(), "ch6 n{}", n);
        n += 1;
    }
    assert!(out4.iter().any(|ch| ch.iter().any(|v| *v != 0.0)));
    // 同图同输入、不同 epoch 结果一致（epoch 只影响标签/历史归属，不改变算法）。
    let out4b = run_dag_stream(&graph, 4, 128, &input, flush, 7, 22).expect("w4 epoch 22");
    assert_bits_equal("epoch-independent", &out4b, &out4);
}

// ---------------------------------------------------------------------------
// 9. 与 T07 direct f32 对拍（T07 冻结工程门；identity 图精确）
// ---------------------------------------------------------------------------

#[test]
fn workers_dag_matches_t07_direct_f32() {
    let graph = graph_4_1_2();
    let input = lcg_input(7, 256, 0x9E37);
    let dag = run_dag_stream(&graph, 4, 128, &input, graph.flush(), 3, 5).expect("dag");
    let direct = run_direct_stream(&graph, 128, &input, graph.flush());
    let (rel, max_abs) = compare_tolerance("4.1.2 w4-vs-direct", &dag, &direct, 1e-5, 1e-5);
    println!(
        "T09 direct-match 4.1.2 PASS: rel_l2={:.3e} max_abs={:.3e}",
        rel, max_abs
    );

    // identity 图（G=I、D=2）：DAG 与 direct 应精确相等（IEEE ==，零符号不判）。
    let ident = graph_identity_7ch();
    let ident_input = lcg_input(7, 128, 0xBEEF);
    let dag_id = run_dag_stream(&ident, 4, 128, &ident_input, ident.flush(), 3, 5).expect("id dag");
    let ref_id = run_direct_stream(&ident, 128, &ident_input, ident.flush());
    let mut c = 0usize;
    while c < dag_id.len() {
        let mut n = 0usize;
        while n < dag_id[c].len() {
            assert_eq!(dag_id[c][n], ref_id[c][n], "identity ch{} n{}", c, n);
            n += 1;
        }
        c += 1;
    }

    // 2x2 offdiag + 尾图。
    let g2 = graph_2x2_offdiag();
    let in2 = lcg_input(2, 130, 0x1234);
    let dag2 = run_dag_stream(&g2, 4, 64, &in2, g2.flush(), 4, 6).expect("2x2 dag");
    let ref2 = run_direct_stream(&g2, 64, &in2, g2.flush());
    let (rel2, max_abs2) = compare_tolerance("2x2 w4-vs-direct", &dag2, &ref2, 1e-5, 1e-5);
    println!(
        "T09 direct-match 2x2 PASS: rel_l2={:.3e} max_abs={:.3e}",
        rel2, max_abs2
    );
}

// ---------------------------------------------------------------------------
// 10. 流式 block 大小（1/2/3/64/128）：W 不变 + 与 direct 一致
// ---------------------------------------------------------------------------

#[test]
fn workers_dag_streaming_block_sizes() {
    let graph = graph_2x2_offdiag();
    let input = lcg_input(2, 130, 0x7777);
    let flush = graph.flush();
    for block in [1usize, 2, 3, 64, 128] {
        let out4 = run_dag_stream(&graph, 4, block, &input, flush, 2, 9)
            .unwrap_or_else(|err| panic!("w4 B={} failed: {:?}", block, err));
        let out1 = run_dag_stream(&graph, 1, block, &input, flush, 2, 9)
            .unwrap_or_else(|err| panic!("w1 B={} failed: {:?}", block, err));
        assert_bits_equal(&format!("streaming B={} w4-vs-w1", block), &out4, &out1);
        let direct = run_direct_stream(&graph, block, &input, flush);
        let (rel, max_abs) = compare_tolerance(
            &format!("streaming B={} vs direct", block),
            &out4,
            &direct,
            1e-5,
            1e-5,
        );
        println!(
            "T09 streaming B={} PASS: W bit-identical; direct rel_l2={:.3e} max_abs={:.3e}",
            block, rel, max_abs
        );
    }
}

// ---------------------------------------------------------------------------
// 11. 静态 tile owner/无 stealing + 每 tile 恰执行一次
// ---------------------------------------------------------------------------

#[test]
fn workers_phase_task_ledger_no_stealing() {
    let graph = graph_4_1_2();
    let plan = WorkerDagPlan::build(&graph.topo, 128, 4, 7).expect("plan");
    let coeffs = graph.coeffs();
    let input = lcg_input(7, 128, 0xABCD);
    let (planar, stride) = pad_planar(&input, graph.flush(), 128);
    let mut state = vec![0.0f32; plan.layout.state_samples()];
    assert!(worker_phase_b1(&plan, 0, &coeffs, &mut state, &planar, stride, 128).is_ok());
    let u_cur_off = plan.layout.u_cur_off as usize;
    let mut role = 0usize;
    while role < 7 {
        let base = u_cur_off + role * WORKER_BLOCK_MAX;
        let mut nonzero = false;
        let mut n = 0usize;
        while n < 128 {
            if state[base + n] != 0.0 {
                nonzero = true;
            }
            n += 1;
        }
        if role % 4 == 0 {
            assert!(nonzero, "worker0 未计算自己的 role {}", role);
        } else {
            assert!(!nonzero, "worker0 越权执行 role {}（stealing）", role);
        }
        role += 1;
    }
    // 非 owner 的 worker id 越界 -> 固定错误码，不执行任何 tile。
    assert_eq!(
        worker_phase_b1(&plan, 4, &coeffs, &mut state, &planar, stride, 128),
        RtStatus::ERR_ADMISSION_SCHED
    );

    let (status, _state, _output, stats) = one_block(
        &graph,
        &plan,
        128,
        &input,
        3,
        0,
        &make_ack(0, 3, 7, 4),
        0.0,
        8,
    );
    assert!(status.is_ok(), "{:?}", status);
    assert_eq!(stats.executed_tiles, plan.total_tasks() as u32);
    let mut worker = 0u8;
    let mut sum = 0u32;
    while worker < WORKER_COUNT_MAX as u8 {
        let mut expected = 0u32;
        let mut phase = 0u8;
        while phase < WORKER_PHASE_COUNT as u8 {
            expected += plan.worker_task_count(phase, worker) as u32;
            phase += 1;
        }
        assert_eq!(stats.worker_tasks_total[worker as usize], expected);
        sum += stats.worker_tasks_total[worker as usize];
        worker += 1;
    }
    assert_eq!(sum, plan.total_tasks() as u32);
    assert_eq!(stats.worker_tasks_total, [6u32, 6, 6, 3]);
}

// ---------------------------------------------------------------------------
// 12. tile 数不足 worker 数：如实并行度/串行比例（不 spin 凑忙）
// ---------------------------------------------------------------------------

#[test]
fn workers_cardinality_serial_fraction() {
    let graph = graph_2x2_offdiag();
    let plan = WorkerDagPlan::build(&graph.topo, 128, 4, 3).expect("plan");
    assert_eq!(plan.phase_task_count(0), 2);
    assert_eq!(plan.phase_parallel_workers(0), 2);
    assert_eq!(plan.serial_fraction_q16(0), 0);
    assert_eq!(plan.worker_task_count(0, 0), 1);
    assert_eq!(plan.worker_task_count(0, 1), 1);
    assert_eq!(plan.worker_task_count(0, 2), 0);
    assert_eq!(plan.worker_task_count(0, 3), 0);
    // B2：k0 -> lane0 -> w0；k1 -> lane2 -> w2；w1/w3 无 tile。
    assert_eq!(plan.worker_task_count(1, 0), 1);
    assert_eq!(plan.worker_task_count(1, 1), 0);
    assert_eq!(plan.worker_task_count(1, 2), 1);
    assert_eq!(plan.worker_task_count(1, 3), 0);
    let input = lcg_input(2, 128, 0x55);
    let (status, _state, _output, stats) = one_block(
        &graph,
        &plan,
        128,
        &input,
        1,
        0,
        &make_ack(0, 1, 3, 4),
        0.0,
        8,
    );
    assert!(status.is_ok(), "{:?}", status);
    assert_eq!(stats.executed_tiles, 6);
    assert_eq!(stats.phase_tasks, [2, 2, 2]);
    assert_eq!(stats.parallel_workers, [2, 2, 2]);
    assert_eq!(stats.serial_fraction_q16, [0, 0, 0]);
    assert_eq!(stats.worker_phase_tasks[0], [1, 1, 0, 0]);
    assert_eq!(stats.worker_phase_tasks[1], [1, 0, 1, 0]);
    assert_eq!(stats.worker_phase_tasks[2], [1, 1, 0, 0]);
    // 1 worker 时全部 tile 串行在一个 worker 上（benchmark/对拍口径）。
    let plan1 = WorkerDagPlan::build(&graph.topo, 128, 1, 3).expect("plan1");
    assert_eq!(plan1.worker_task_count(0, 0), 2);
    assert_eq!(plan1.phase_parallel_workers(0), 1);
    assert_eq!(
        plan1.serial_fraction_q16(0),
        (((2u64 - 1) * 65536) / 2) as u32
    );
    let (status1, _state1, _output1, stats1) = one_block(
        &graph,
        &plan1,
        128,
        &input,
        1,
        0,
        &make_ack(0, 1, 3, 1),
        0.0,
        8,
    );
    assert!(status1.is_ok(), "{:?}", status1);
    assert_eq!(stats1.executed_tiles, 6);
    assert_eq!(stats1.worker_tasks_total[0], 6);
}

// ---------------------------------------------------------------------------
// 13. cutoff：整块 invalid、不产生半图输出、迟到 worker 仍可 ack（供 quarantine）
// ---------------------------------------------------------------------------

#[test]
fn workers_cutoff_invalidates_block_without_half_output() {
    let graph = graph_4_1_2();
    let plan = WorkerDagPlan::build(&graph.topo, 128, 4, 7).expect("plan");
    let input = lcg_input(7, 128, 0xCAFE);
    // master 已判超时（cutoff）：整块 invalid，输出清零。
    let ack = make_ack(0, 3, 7, 4);
    assert_eq!(set_cutoff(&ack), RtStatus::OK);
    let (status, state, output, stats) = one_block(&graph, &plan, 128, &input, 3, 0, &ack, 7.0, 8);
    assert_eq!(status, RtStatus::RT_E_CUTOFF);
    assert!(output.iter().all(|v| v.to_bits() == 0.0f32.to_bits()));
    assert!(state.iter().all(|v| *v == 0.0));
    assert_eq!(stats.executed_tiles, 0);
    assert_eq!(stats.cutoff_blocks, 1);
    assert!(!ack_all(&ack));

    // 半图：B1 已执行并 ack，master 置 cutoff；B2/B3 不得产生输出。
    let ack2 = make_ack(1, 3, 7, 4);
    let coeffs = graph.coeffs();
    let mut state2 = vec![0.0f32; plan.layout.state_samples()];
    let (planar, stride) = pad_planar(&input, graph.flush(), 128);
    let mut worker = 0u8;
    while worker < 4 {
        assert!(worker_phase_b1(&plan, worker, &coeffs, &mut state2, &planar, stride, 128).is_ok());
        assert!(ack_phase(&ack2, worker, 0).is_ok());
        worker += 1;
    }
    assert!(wait_phase_done(&ack2, 0, 4, 8).is_ok());
    assert_eq!(set_cutoff(&ack2), RtStatus::OK);
    // 迟到 worker 仍 ack（quarantine all-ack 不计输出有效性），但块结果绝不发布。
    assert!(ack_phase_labeled(&ack2, 0, 1, 3, 1, 7).is_ok());
    assert_eq!(ack_phase_labeled(&ack2, 1, 1, 3, 1, 7), RtStatus::OK);
    let (status2, _state2, output2, stats2) =
        one_block(&graph, &plan, 128, &input, 3, 1, &ack2, 9.0, 8);
    assert_eq!(status2, RtStatus::RT_E_CUTOFF);
    assert!(output2.iter().all(|v| v.to_bits() == 0.0f32.to_bits()));
    assert_eq!(stats2.cutoff_blocks, 1);
    assert!(!ack_all(&ack2));
}

// ---------------------------------------------------------------------------
// 14. quarantine：all-ack 前不复用；失联 worker 永不回收；容量耗尽
// ---------------------------------------------------------------------------

#[test]
fn workers_quarantine_all_ack_and_lost_worker() {
    let quarantine = WorkerQuarantine::new();
    let slot = quarantine.enter(5, 9, 3, 4).expect("enter");
    assert!(quarantine.is_occupied(slot));
    assert_eq!(
        quarantine.note_ack(slot, 0, 5, 8, 3),
        RtStatus::RT_E_PARTIAL_LATE
    );
    assert!(quarantine.note_ack(slot, 0, 5, 9, 3).is_ok());
    assert!(quarantine.note_ack(slot, 1, 5, 9, 3).is_ok());
    assert_eq!(quarantine.try_release(slot), RtStatus::RT_E_SLOT_STATE);
    assert!(quarantine.note_ack(slot, 2, 5, 9, 3).is_ok());
    assert_eq!(quarantine.try_release(slot), RtStatus::RT_E_SLOT_STATE);
    assert!(quarantine.note_ack(slot, 3, 5, 9, 3).is_ok());
    assert_eq!(quarantine.try_release(slot), RtStatus::OK);
    assert!(!quarantine.is_occupied(slot));

    // 失联 worker：永不 ack -> 永不复用/回收（INV-L5）。
    let lost = quarantine.enter(6, 10, 4, 4).expect("enter lost");
    assert!(quarantine.note_ack(lost, 0, 6, 10, 4).is_ok());
    assert!(quarantine.note_ack(lost, 1, 6, 10, 4).is_ok());
    assert!(quarantine.note_ack(lost, 2, 6, 10, 4).is_ok());
    let mut probe = 0u32;
    while probe < 4 {
        assert_eq!(quarantine.try_release(lost), RtStatus::RT_E_SLOT_STATE);
        probe += 1;
    }
    assert!(quarantine.is_occupied(lost));

    // 容量 8：第 9 个返回 ERR_QUARANTINE_EXHAUSTED（禁止无界分配）。
    let full = WorkerQuarantine::new();
    let mut index = 0usize;
    while index < WORKER_QUARANTINE_SLOTS {
        full.enter(1, index as u64, 1, 4).expect("fill");
        index += 1;
    }
    assert_eq!(
        full.enter(1, 99, 1, 4).unwrap_err(),
        RtStatus::ERR_QUARANTINE_EXHAUSTED
    );
    assert_eq!(full.in_use(), WORKER_QUARANTINE_SLOTS as u32);
    assert_eq!(full.note_ack(0, 4, 1, 0, 1), RtStatus::ERR_BAD_LAYOUT);
}

// ---------------------------------------------------------------------------
// 15. 2 核重准入：旧 epoch 停止 -> drain -> 独立 2 核 plan + 新 epoch（不复用半图）
// ---------------------------------------------------------------------------

#[test]
fn workers_two_core_re_admission_fresh_epoch() {
    let graph = graph_4_1_2();
    let plan4 = WorkerDagPlan::build(&graph.topo, 128, 4, 1).expect("plan4");
    let ctl = KhaenRtControlV1::new();
    ctl.current_plan_version.store(1, Ordering::Release);
    let mut pool = WorkerAckSlotPool::new();
    let desc = base_desc(100, 0, 1, 128);
    let slot = master_start_block(&ctl, &mut pool, &plan4, &desc).expect("start 4core");
    let slot_ref = pool.slot(slot as usize).expect("slot ref");
    let mut worker = 0u8;
    while worker < 4 {
        let mut phase = 0u8;
        while phase < 3 {
            assert!(ack_phase(slot_ref, worker, phase).is_ok());
            phase += 1;
        }
        worker += 1;
    }
    assert_eq!(pool.mark_all_ack(slot as usize), RtStatus::OK);
    assert_eq!(pool.release_recyclable(slot as usize), RtStatus::OK);
    assert_eq!(pool.recycle_free(slot as usize), RtStatus::OK);
    assert_eq!(pool.occupied(), 0);

    let mut state_machine = WorkerReAdmission::new(plan4, 100).expect("state machine");
    assert!(state_machine.accepts_new_blocks(&ctl));
    assert_eq!(ctl.current_plan_version.load(Ordering::Acquire), 1);
    assert_eq!(
        ctl.flags.load(Ordering::Acquire) & WORKER_CONTROL_FLAG_TWO_WORKER,
        0
    );
    assert_eq!(state_machine.request_degrade(&ctl), RtStatus::OK);
    assert_eq!(
        ctl.switch_state.load(Ordering::Acquire),
        WORKER_SWITCH_ABORT
    );
    assert!(!state_machine.accepts_new_blocks(&ctl));
    assert_eq!(
        master_start_block(&ctl, &mut pool, &plan4, &desc).unwrap_err(),
        RtStatus::ERR_SWITCH_BUSY
    );

    let new_epoch = state_machine
        .re_admit(&graph.topo, 128, &pool, &ctl)
        .expect("re-admit");
    assert_eq!(new_epoch, 101);
    assert_eq!(state_machine.mode, WorkerMode::TwoCore);
    assert_eq!(state_machine.expected_workers(), 2);
    assert_eq!(state_machine.plan.plan_version, 2);
    assert_eq!(state_machine.plan.expected_workers, 2);
    assert_eq!(
        ctl.flags.load(Ordering::Acquire) & WORKER_CONTROL_FLAG_TWO_WORKER,
        WORKER_CONTROL_FLAG_TWO_WORKER
    );
    assert!(state_machine.accepts_new_blocks(&ctl));
    // I01：re_admit 自身发布新 plan_version/flags（不靠测试手动 store），
    // 且不伪造 current_plan_ptr（plan arena 由 host/T26 发布）。
    assert_eq!(ctl.current_plan_version.load(Ordering::Acquire), 2);
    assert_eq!(ctl.current_plan_ptr.load(Ordering::Acquire), 0);
    // 旧 4 核 plan 绝不能在 two_worker 模式继续（半图拒绝）。
    assert_eq!(
        master_start_block(&ctl, &mut pool, &plan4, &desc).unwrap_err(),
        RtStatus::ERR_PLAN_STALE
    );
    let desc2 = base_desc(101, 0, 2, 128);
    assert!(master_start_block(&ctl, &mut pool, &state_machine.plan, &desc2).is_ok());

    // 新 epoch 从零状态跑 2 核 plan，与同图 1 worker 基线逐位一致（不是半图拼接）。
    let input = lcg_input(7, 128, 0x0D15_EA5E);
    let out2 = run_dag_with_plan(&graph, &state_machine.plan, 128, &input, graph.flush(), 101)
        .expect("w2 fresh");
    let plan1 = WorkerDagPlan::build(&graph.topo, 128, 1, 2).expect("plan1");
    let out1 =
        run_dag_with_plan(&graph, &plan1, 128, &input, graph.flush(), 101).expect("w1 fresh");
    assert_bits_equal("re-admit two-core vs one-core", &out2, &out1);

    // drain 未完成（存在未终态 ack 槽）时禁止重准入。
    let plan4b = WorkerDagPlan::build(&graph.topo, 128, 4, 1).expect("plan4b");
    let mut blocked = WorkerReAdmission::new(plan4b, 5).expect("state2");
    let ctl2 = KhaenRtControlV1::new();
    assert_eq!(blocked.request_degrade(&ctl2), RtStatus::OK);
    let mut pool2 = WorkerAckSlotPool::new();
    assert!(pool2.try_acquire(0, 5, 1, 4, 0).is_ok());
    assert_eq!(
        blocked
            .re_admit(&graph.topo, 128, &pool2, &ctl2)
            .unwrap_err(),
        RtStatus::ERR_SWITCH_BUSY
    );
    assert_eq!(blocked.mode, WorkerMode::Degrading);
    assert_eq!(blocked.expected_workers(), 4);
}

// ---------------------------------------------------------------------------
// 16. 重准入失败：进入 SafetyMute，绝不生成 2 核半图
// ---------------------------------------------------------------------------

#[test]
fn workers_two_core_re_admission_failure_safety_mute() {
    let graph = graph_4_1_2();
    let plan4 = WorkerDagPlan::build(&graph.topo, 128, 4, 1).expect("plan4");
    let mut state_machine = WorkerReAdmission::new(plan4, 7).expect("state");
    let ctl = KhaenRtControlV1::new();
    // 未降级不能直接重准入。
    let scratch_pool = WorkerAckSlotPool::new();
    assert_eq!(
        state_machine
            .re_admit(&graph.topo, 128, &scratch_pool, &ctl)
            .unwrap_err(),
        RtStatus::ERR_SWITCH_BUSY
    );
    assert_eq!(state_machine.request_degrade(&ctl), RtStatus::OK);
    let pool = WorkerAckSlotPool::new();
    let err = state_machine
        .re_admit(&graph.topo, 0, &pool, &ctl)
        .unwrap_err();
    assert_eq!(err, RtStatus::ERR_BLOCK_SIZE);
    assert_eq!(state_machine.mode, WorkerMode::SafetyMute);
    assert_eq!(state_machine.expected_workers(), 4);
    assert!(!state_machine.accepts_new_blocks(&ctl));
    // I02：终态失败（plan 构建/epoch 溢出）立即置 safety_request，不要求调用方
    // 额外再调 to_safety_mute；to_safety_mute 仍可幂等调用。
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 1);
    assert_eq!(state_machine.to_safety_mute(&ctl), RtStatus::OK);
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 1);
    assert_eq!(
        ctl.switch_state.load(Ordering::Acquire),
        WORKER_SWITCH_ABORT
    );
    // 坏 plan 构造被拒。
    let mut bad = plan4;
    bad.expected_workers = 3;
    assert_eq!(
        WorkerReAdmission::new(bad, 1).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );
}

// ---------------------------------------------------------------------------
// 17. epoch/plan 标签不匹配：不得触碰输出/不得消费旧槽
// ---------------------------------------------------------------------------

#[test]
fn workers_epoch_and_plan_label_mismatch_rejected() {
    let graph = graph_4_1_2();
    let plan = WorkerDagPlan::build(&graph.topo, 128, 4, 7).expect("plan");
    let input = lcg_input(7, 128, 0xE0E0);
    let ack = make_ack(0, 99, 7, 4);
    let (status, _state, output, stats) =
        one_block(&graph, &plan, 128, &input, 21, 0, &ack, 7.0, 8);
    assert_eq!(status, RtStatus::RT_E_EPOCH_MISMATCH);
    assert!(output.iter().all(|v| *v == 7.0), "label 校验前不得写输出");
    assert_eq!(stats.executed_tiles, 0);
    let ack = make_ack(3, 21, 7, 4);
    assert_eq!(
        one_block(&graph, &plan, 128, &input, 21, 0, &ack, 7.0, 8).0,
        RtStatus::RT_E_EPOCH_MISMATCH
    );
    let ack = make_ack(0, 21, 6, 4);
    assert_eq!(
        one_block(&graph, &plan, 128, &input, 21, 0, &ack, 7.0, 8).0,
        RtStatus::RT_E_PLAN_MISMATCH
    );
    let ack = make_ack(0, 21, 7, 2);
    assert_eq!(
        one_block(&graph, &plan, 128, &input, 21, 0, &ack, 7.0, 8).0,
        RtStatus::RT_E_PLAN_MISMATCH
    );
    let ack = KhaenBlockAckV1::new(0, 21, 7, 4);
    assert_eq!(
        one_block(&graph, &plan, 128, &input, 21, 0, &ack, 7.0, 8).0,
        RtStatus::RT_E_ABI_RUNTIME
    );
}

// ---------------------------------------------------------------------------
// 18. 输出非有限：安全兜底（整块清零 + fault 级错误码），不得发布
// ---------------------------------------------------------------------------

#[test]
fn workers_nonfinite_output_guard() {
    let graph = graph_4_1_2();
    let plan = WorkerDagPlan::build(&graph.topo, 128, 4, 7).expect("plan");
    let mut input = lcg_input(7, 128, 0xF00D);
    input[0][5] = f32::NAN;
    let ack = make_ack(0, 4, 7, 4);
    let (status, _state, output, stats) = one_block(&graph, &plan, 128, &input, 4, 0, &ack, 0.0, 8);
    assert_eq!(status, RtStatus::RT_E_OUTPUT_NONFINITE);
    assert!(output.iter().all(|v| v.to_bits() == 0.0f32.to_bits()));
    assert_eq!(stats.invalid_blocks, 1);
    let input_ok = lcg_input(7, 128, 0xF00D);
    let ack = make_ack(0, 4, 7, 4);
    assert!(one_block(&graph, &plan, 128, &input_ok, 4, 0, &ack, 0.0, 8)
        .0
        .is_ok());
}

// ---------------------------------------------------------------------------
// 19. RT 可见类型布局/无析构/常量
// ---------------------------------------------------------------------------

#[test]
fn workers_rt_visible_types_and_layout() {
    use core::mem::{align_of, needs_drop, offset_of, size_of};
    assert_eq!(size_of::<KhaenRtControlV1>(), 128);
    assert_eq!(align_of::<KhaenRtControlV1>(), 64);
    assert_eq!(offset_of!(KhaenRtControlV1, magic), 0);
    assert_eq!(offset_of!(KhaenRtControlV1, block_start_seq), 8);
    assert_eq!(offset_of!(KhaenRtControlV1, block_start_epoch), 16);
    assert_eq!(offset_of!(KhaenRtControlV1, block_start_frame), 24);
    assert_eq!(offset_of!(KhaenRtControlV1, block_start_plan_version), 32);
    assert_eq!(offset_of!(KhaenRtControlV1, block_start_slot), 36);
    assert_eq!(offset_of!(KhaenRtControlV1, current_plan_ptr), 40);
    assert_eq!(offset_of!(KhaenRtControlV1, switch_state), 64);
    assert_eq!(offset_of!(KhaenRtControlV1, safety_request), 68);
    assert_eq!(offset_of!(KhaenRtControlV1, heartbeat_seq), 80);
    assert_eq!(offset_of!(KhaenRtControlV1, spin_max_iters), 88);
    assert_eq!(offset_of!(KhaenRtControlV1, capture_ring_offset), 104);
    assert_eq!(offset_of!(KhaenRtControlV1, playback_ring_offset), 112);
    assert_eq!(offset_of!(KhaenRtControlV1, reserved), 120);
    assert_eq!(align_of::<WorkerPartials>(), 64);
    assert_eq!(size_of::<WorkerPartials>(), WORKER_PARTIAL_SAMPLES * 4);
    assert_eq!(align_of::<WorkerAckSlotPool>(), 64);
    assert_eq!(align_of::<WorkerQuarantine>(), 64);
    assert_eq!(size_of::<WorkerBlockStart>(), 24);
    assert!(!needs_drop::<KhaenRtControlV1>());
    assert!(!needs_drop::<WorkerDagPlan>());
    assert!(!needs_drop::<WorkerPartials>());
    assert!(!needs_drop::<WorkerAckSlotPool>());
    assert!(!needs_drop::<WorkerQuarantine>());
    assert!(!needs_drop::<WorkerMode>());
    assert!(!needs_drop::<WorkerReAdmission>());
    assert!(!needs_drop::<WorkerScheduleStats>());
    assert!(!needs_drop::<KhaenBlockAckV1>());
    assert!(!needs_drop::<RtStatus>());
    let ctl = KhaenRtControlV1::new();
    assert_eq!(ctl.magic, khaen_core::abi::KHAEN_RING_MAGIC);
    assert_eq!(ctl.abi_version, ABI_VERSION);
    assert_eq!(ctl.size, 128);
    assert_eq!(ctl.block_start_seq.load(Ordering::Acquire), 0);
    let mut buffer = vec![5.0f32; 32];
    worker_state_zero(&mut buffer);
    assert!(buffer.iter().all(|v| *v == 0.0));
}

// ---------------------------------------------------------------------------
// 20. RT-MODULE 源扫描（F-01..F-44 子集；与 rt-check forbid/drop-audit 同口径，
//     仅 token 黑名单第一道防线，不是 RT 调用图/类型/dataflow 证明——见文件头 I04）
// ---------------------------------------------------------------------------

fn code_only(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_const_assert = false;
    for raw in src.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with("const _: () = {") {
            in_const_assert = true;
            out.push(String::new());
            continue;
        }
        if in_const_assert {
            if trimmed == "};" {
                in_const_assert = false;
            }
            out.push(String::new());
            continue;
        }
        let trimmed_start = raw.trim_start();
        if trimmed_start.starts_with("//") {
            out.push(String::new());
            continue;
        }
        let code = match raw.find("//") {
            Some(pos) => &raw[..pos],
            None => raw,
        };
        out.push(code.to_string());
    }
    out
}

#[test]
fn workers_rt_module_source_scan() {
    const SRC: &str = include_str!("../../src/workers.rs");
    let mut header_ok = false;
    let mut count = 0usize;
    for line in SRC.lines() {
        if count >= 64 {
            break;
        }
        if line.contains("RT-MODULE:") {
            header_ok = true;
            break;
        }
        count += 1;
    }
    assert!(header_ok, "workers.rs 缺 RT-MODULE 文件头（R-DROP-17）");
    let needles = [
        "Vec<",
        "Vec::",
        "vec![",
        "String",
        "Box::new",
        "Box<",
        "Arc<",
        "Rc<",
        "HashMap",
        "BTreeMap",
        "Cow<",
        "format!(",
        "to_string(",
        "panic!",
        "unwrap(",
        "expect(",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "assert!(",
        "Mutex",
        "RwLock",
        "Condvar",
        "thread::",
        "Instant::now",
        "SystemTime::now",
        "println!",
        "eprintln!",
        "std::fs::",
        "std::net::",
        "std::process::",
        "std::env",
        "drop(",
        "drop_in_place",
        "unsafe",
        "extern \"C\"",
        "rayon",
        "work_steal",
        "thread_pool",
        "catch_unwind",
        "mem::forget",
        "ManuallyDrop",
        "libc::",
        "clock_gettime",
        "impl Drop",
    ];
    let parsed = code_only(SRC);
    let mut line_number = 0usize;
    for line in parsed {
        line_number += 1;
        if line.trim().is_empty() {
            continue;
        }
        for needle in needles {
            assert!(
                !line.contains(needle),
                "workers.rs:{} 命中禁止符号 {}（line: {}）",
                line_number,
                needle,
                line.trim()
            );
        }
    }
    assert!(SRC.contains("worker_phase_b1"));
    assert!(SRC.contains("worker_phase_b2"));
    assert!(SRC.contains("worker_phase_b3"));
    // 正控（I04）：确认同一黑名单对注入文本确实命中，证明该扫描本身是活的；
    // 但它是 token 级第一道防线，不是 RT 调用图/类型/dataflow 证明（见文件头）。
    let injected = "fn injected() { let v: Vec<u8> = Vec::new(); }";
    let mut injected_hits = 0usize;
    for line in code_only(injected) {
        if line.trim().is_empty() {
            continue;
        }
        for needle in needles.iter() {
            if line.contains(needle) {
                injected_hits += 1;
            }
        }
    }
    assert!(
        injected_hits >= 2,
        "黑名单正控应命中 Vec</Vec::，实际 {} 次",
        injected_hits
    );
}

// ---------------------------------------------------------------------------
// 21. partial cache line 隔离（F-34）
// ---------------------------------------------------------------------------

#[test]
fn workers_partial_cacheline_isolation() {
    use core::mem::align_of;
    assert_eq!(align_of::<WorkerPartials>(), 64);
    assert_eq!(
        (WORKER_PARTIAL_ROLE_STRIDE * core::mem::size_of::<f32>()) % 64,
        0
    );
    assert_eq!(
        (WORKER_PARTIAL_LANE_STRIDE * core::mem::size_of::<f32>()) % 64,
        0
    );
    let partials = WorkerPartials::new();
    let mut ranges: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut lane = 0usize;
    while lane < WORKER_REDUCE_LANES {
        let mut role = 0usize;
        while role < WORKER_ROLES_MAX {
            let slice = partials.lane_role(lane, role).expect("lane/role slice");
            let start = slice.as_ptr() as usize;
            assert_eq!(start % 64, 0, "lane{} role{} 未 64 B 对齐", lane, role);
            ranges.push((
                start,
                start + WORKER_PARTIAL_ROLE_STRIDE * core::mem::size_of::<f32>(),
                lane,
                role,
            ));
            role += 1;
        }
        lane += 1;
    }
    assert_eq!(ranges.len(), WORKER_REDUCE_LANES * WORKER_ROLES_MAX);
    ranges.sort_by_key(|entry| entry.0);
    let mut index = 1usize;
    while index < ranges.len() {
        assert!(
            ranges[index - 1].1 <= ranges[index].0,
            "partial 区间重叠: {:?} vs {:?}",
            ranges[index - 1],
            ranges[index]
        );
        index += 1;
    }
    assert!(partials.lane_role(0, WORKER_ROLES_MAX).is_none());
    assert!(partials.lane_role(WORKER_REDUCE_LANES, 0).is_none());
}

// ---------------------------------------------------------------------------
// 22. re_admit 版本发布契约 + 失败安全语义（T09-review I01/I02）
// ---------------------------------------------------------------------------

#[test]
fn workers_re_admission_version_publish_contract() {
    let graph = graph_4_1_2();
    let plan4 = WorkerDagPlan::build(&graph.topo, 128, 4, 1).expect("plan4");

    // --- 失败路径（终态）：不发布 version/flags、保持旧 plan，但立即置 safety_request ---
    let mut failed = WorkerReAdmission::new(plan4, 30).expect("failed machine");
    let mut fail_pool = WorkerAckSlotPool::new();
    let ctl_fail = KhaenRtControlV1::new();
    ctl_fail.current_plan_version.store(1, Ordering::Release);
    assert_eq!(failed.request_degrade(&ctl_fail), RtStatus::OK);
    assert_eq!(
        failed
            .re_admit(&graph.topo, 0, &fail_pool, &ctl_fail)
            .unwrap_err(),
        RtStatus::ERR_BLOCK_SIZE
    );
    assert_eq!(failed.mode, WorkerMode::SafetyMute);
    assert_eq!(failed.plan.plan_version, 1);
    assert_eq!(failed.expected_workers(), 4);
    assert_eq!(failed.epoch_id, 30);
    assert_eq!(ctl_fail.current_plan_version.load(Ordering::Acquire), 1);
    assert_eq!(
        ctl_fail.flags.load(Ordering::Acquire) & WORKER_CONTROL_FLAG_TWO_WORKER,
        0
    );
    assert_eq!(ctl_fail.safety_request.load(Ordering::Acquire), 1);
    assert_eq!(
        ctl_fail.switch_state.load(Ordering::Acquire),
        WORKER_SWITCH_ABORT
    );
    // 失败后仍拒绝启动旧 plan（ABORT 不放行，不进入半图）。
    assert_eq!(
        master_start_block(&ctl_fail, &mut fail_pool, &plan4, &base_desc(30, 0, 1, 128))
            .unwrap_err(),
        RtStatus::ERR_SWITCH_BUSY
    );

    // --- 成功路径：re_admit 自己发布 new plan_version/two_worker flag，再清 ABORT ---
    let plan4b = WorkerDagPlan::build(&graph.topo, 128, 4, 1).expect("plan4b");
    let mut ok = WorkerReAdmission::new(plan4b, 40).expect("ok machine");
    let mut pool = WorkerAckSlotPool::new();
    let ctl = KhaenRtControlV1::new();
    ctl.current_plan_version.store(1, Ordering::Release);
    assert_eq!(ok.request_degrade(&ctl), RtStatus::OK);
    let new_epoch = ok
        .re_admit(&graph.topo, 128, &pool, &ctl)
        .expect("re-admit");
    assert_eq!(new_epoch, 41);
    assert_eq!(ok.mode, WorkerMode::TwoCore);
    // I01：re_admit 内部提交 version/flags；测试不再手动 store(2)。
    assert_eq!(ok.plan.plan_version, 2);
    assert_eq!(ctl.current_plan_version.load(Ordering::Acquire), 2);
    assert_eq!(
        ctl.flags.load(Ordering::Acquire) & WORKER_CONTROL_FLAG_TWO_WORKER,
        WORKER_CONTROL_FLAG_TWO_WORKER
    );
    assert_eq!(ctl.switch_state.load(Ordering::Acquire), 0);
    assert!(master_start_block(&ctl, &mut pool, &ok.plan, &base_desc(41, 0, 2, 128)).is_ok());
    // 旧 desc/plan_version 仍被拒（不得消费旧槽）。
    assert_eq!(
        master_start_block(&ctl, &mut pool, &ok.plan, &base_desc(41, 0, 1, 128)).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    // 旧 4 核 plan 在 two_worker 模式被拒（半图防护）。
    assert_eq!(
        master_start_block(&ctl, &mut pool, &plan4b, &base_desc(41, 0, 1, 128)).unwrap_err(),
        RtStatus::ERR_PLAN_STALE
    );
    // 该 2 核 plan 从零状态执行与 1 worker 基线逐位一致（新 epoch，无旧图拼接）。
    let input = lcg_input(7, 128, 0x71_71);
    let out2 = run_dag_with_plan(&graph, &ok.plan, 128, &input, graph.flush(), 41).expect("w2");
    let plan1 = WorkerDagPlan::build(&graph.topo, 128, 1, 2).expect("plan1");
    let out1 = run_dag_with_plan(&graph, &plan1, 128, &input, graph.flush(), 41).expect("w1");
    assert_bits_equal("version-publish two-core vs one-core", &out2, &out1);
}
