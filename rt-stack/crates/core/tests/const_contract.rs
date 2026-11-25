//! T06 契约常量/公式测试（executed，非 RT）：对照 core-v1 §3 与附录 B.1/B.3。
//!
//! 本测试是 T06 scaffold 自证的一部分：证明常量/公式被真实编译并执行断言，不是 0 tests。

use khaen_core::abi::{TspiPlanHeaderV1, TspiWorkerAffinityV1};
use khaen_core::events::{
    event_name, EV_HOST_INIT, EV_STOPPED, EV_WORKER_LOST, LANE_CONTROL, LANE_WORKER0,
    SEVERITY_FATAL, SEVERITY_INFO,
};
use khaen_core::rt::{
    fft_size, fused_len_max, gate_ns_floor, max_partitions, t_block_ns_ceil, ACK_MASK_ALL,
    BLOCK_DEFAULT, BLOCK_SIZES, EVENT_LANES, EVENT_LANE_SLOTS, EVENT_RING_SLOTS, GRAPH_ID_BYTES,
    MAX_DIAG_FIRS, MAX_INPUT_CHANNELS, MAX_INPUT_ROLES, MAX_LOW_BAND_PARTICIPANTS,
    MAX_MATRIX_PATHS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, MAX_PARTITIONS, MAX_PLAN_NODES,
    MAX_WORKER_STACK_BYTES, PARALLEL_PHASES, QUARANTINE_SLOTS, RT_WORKERS, RT_WORKERS_BENCH,
    RT_WORKERS_FALLBACK, SAMPLE_RATES_HZ, SPSC_CAPTURE_SLOTS, SPSC_PLAYBACK_SLOTS,
    TAPS_EXTENDED_MAX, TAPS_NORMAL_MAX, TICK_READS_PER_BLOCK_MAX, WORKER_BITS,
};

#[test]
fn contract_constants_match_core_v1() {
    assert_eq!(MAX_INPUT_CHANNELS, 20);
    assert_eq!(MAX_OUTPUT_CHANNELS, 22);
    assert_eq!(MAX_INPUT_ROLES, 16);
    assert_eq!(MAX_OUTPUT_ROLES, 16);
    assert_eq!(MAX_LOW_BAND_PARTICIPANTS, 8);
    assert_eq!(MAX_MATRIX_PATHS, 128);
    assert_eq!(MAX_DIAG_FIRS, 22);
    assert_eq!(MAX_PLAN_NODES, 256);
    assert_eq!(SAMPLE_RATES_HZ, [48_000, 96_000]);
    assert_eq!(BLOCK_SIZES, [64, 128]);
    assert_eq!(BLOCK_DEFAULT, 128);
    assert_eq!(TAPS_NORMAL_MAX, 4096);
    assert_eq!(TAPS_EXTENDED_MAX, 16_384);
    assert_eq!(MAX_PARTITIONS, 1500);
    assert_eq!(RT_WORKERS, 4);
    assert_eq!(RT_WORKERS_FALLBACK, 2);
    assert_eq!(RT_WORKERS_BENCH, [1, 2, 4]);
    assert_eq!(PARALLEL_PHASES, 3);
    assert_eq!(SPSC_CAPTURE_SLOTS, 4);
    assert_eq!(SPSC_PLAYBACK_SLOTS, 4);
    assert_eq!(QUARANTINE_SLOTS, 8);
    assert_eq!(EVENT_LANES, 8);
    assert_eq!(EVENT_LANE_SLOTS, 128);
    assert_eq!(EVENT_RING_SLOTS, 1024);
    assert_eq!(MAX_WORKER_STACK_BYTES, 512 * 1024);
    assert_eq!(WORKER_BITS, 4);
    assert_eq!(ACK_MASK_ALL, 0xF);
    assert_eq!(GRAPH_ID_BYTES, 32);
    assert_eq!(TICK_READS_PER_BLOCK_MAX, 3);
}

/// core-v1 v1.1 I04（§3.4/§6.4/§7.3）：master 每 block 预算内 tick 读取上限 = 3，
/// 正常路径只读 2 次（B1 进入 + B3 退出），仅 spin 耗尽判 hard_cutoff 时可选第 3 次（t_cut）。
/// 纯 worker 0 次；`rt_tick_budgeted` 的预算上界不得低于本常量。
/// T105：T06 scaffold 曾误写 2，现按 spec 对齐并固定断言。
#[test]
fn tick_budget_matches_core_v1_i04() {
    // spec 值 = 3；正常路径只读 2 次（预算内不超 3，纯 worker 0 次）。
    assert_eq!(TICK_READS_PER_BLOCK_MAX, 3, "I04: master tick budget = 3");
    assert!(
        TICK_READS_PER_BLOCK_MAX > 2,
        "预算需容纳可选 hard_cutoff 第 3 次 tick"
    );
}

/// T105：v1.1 affinity ABI —— `TspiWorkerAffinityV1`（§11.13，32 B/align 8）提升到 core::abi，
/// 且 `TspiPlanHeaderV1` 具备 v1.1 affinity/ctl 字段（core-v1 v1.1.2 errata E1 定稿：
/// reserved1@94、affinity@96/100、reserved2@102、ctl@104/108、reserved3@112、crc@124）。
#[test]
fn abi_v11_affinity_layout_is_repr_c_legal() {
    use core::mem::{align_of, offset_of, size_of};
    // TspiWorkerAffinityV1：B-18，32 B / align 8（plan POD 表项，align 例外）。
    assert_eq!(size_of::<TspiWorkerAffinityV1>(), 32);
    assert_eq!(align_of::<TspiWorkerAffinityV1>(), 8);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, worker_id), 0);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, sched_priority), 1);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, flags), 2);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, cpu_index), 4);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, reserved0), 6);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, irq_dependency_mask), 8);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, reserved1), 16);
    assert_eq!(offset_of!(TspiWorkerAffinityV1, reserved2), 24);
    assert!(!core::mem::needs_drop::<TspiWorkerAffinityV1>());

    // TspiPlanHeaderV1：v1.1 affinity/ctl 字段（errata E1 定稿：96/100/104/108，size 128）。
    assert_eq!(
        size_of::<TspiPlanHeaderV1>(),
        khaen_core::abi::PLAN_HEADER_SIZE
    );
    assert_eq!(
        align_of::<TspiPlanHeaderV1>(),
        khaen_core::abi::PLAN_HEADER_ALIGN
    );
    assert_eq!(
        offset_of!(TspiPlanHeaderV1, affinity_table_offset),
        khaen_core::abi::PLAN_HEADER_OFFSET_AFFINITY_TABLE
    );
    assert_eq!(
        offset_of!(TspiPlanHeaderV1, affinity_count),
        khaen_core::abi::PLAN_HEADER_OFFSET_AFFINITY_COUNT
    );
    assert_eq!(
        offset_of!(TspiPlanHeaderV1, ctl_meta_offset),
        khaen_core::abi::PLAN_HEADER_OFFSET_CTL_META
    );
    assert_eq!(
        offset_of!(TspiPlanHeaderV1, ctl_meta_len),
        khaen_core::abi::PLAN_HEADER_OFFSET_CTL_META_LEN
    );
    assert_eq!(size_of::<TspiPlanHeaderV1>(), 128);
    assert_eq!(align_of::<TspiPlanHeaderV1>(), 64);
    assert_eq!(offset_of!(TspiPlanHeaderV1, role_table_count), 92);
    assert_eq!(offset_of!(TspiPlanHeaderV1, affinity_table_offset), 96);
    assert_eq!(offset_of!(TspiPlanHeaderV1, affinity_count), 100);
    assert_eq!(offset_of!(TspiPlanHeaderV1, ctl_meta_offset), 104);
    assert_eq!(offset_of!(TspiPlanHeaderV1, ctl_meta_len), 108);
    assert_eq!(offset_of!(TspiPlanHeaderV1, header_crc32c), 124);
}

#[test]
fn timing_formulas_match_appendix_b() {
    assert_eq!(t_block_ns_ceil(48_000, 128), 2_666_667);
    assert_eq!(t_block_ns_ceil(48_000, 64), 1_333_334);
    assert_eq!(t_block_ns_ceil(96_000, 128), 1_333_334);
    assert_eq!(t_block_ns_ceil(96_000, 64), 666_667);
    assert_eq!(gate_ns_floor(48_000, 128), 1_333_333);
    assert_eq!(gate_ns_floor(48_000, 64), 666_666);
    assert_eq!(gate_ns_floor(96_000, 128), 666_666);
    assert_eq!(gate_ns_floor(96_000, 64), 333_333);
    assert_eq!(fft_size(64), 128);
    assert_eq!(fft_size(128), 256);
    assert_eq!(max_partitions(16_384, 64), 256);
    assert_eq!(max_partitions(16_384, 128), 128);
    assert_eq!(max_partitions(24_000, 16), 1500);
    assert_eq!(fused_len_max(4096, 1024), 5119);
    assert_eq!(fused_len_max(8192, 4096), 12287);
}

#[test]
fn event_kinds_are_frozen_and_named() {
    assert_eq!(EV_HOST_INIT, 1);
    assert_eq!(EV_STOPPED, 35);
    assert_eq!(event_name(EV_HOST_INIT), "EV_HOST_INIT");
    assert_eq!(event_name(EV_WORKER_LOST), "EV_WORKER_LOST");
    assert_eq!(event_name(EV_STOPPED), "EV_STOPPED");
    assert_eq!(event_name(999), "EV_UNKNOWN");
    assert_eq!(LANE_WORKER0, 0);
    assert_eq!(LANE_CONTROL, 7);
    assert_eq!(SEVERITY_INFO, 1);
    assert_eq!(SEVERITY_FATAL, 4);
}
