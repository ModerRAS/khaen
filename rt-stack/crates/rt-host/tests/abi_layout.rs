//! T06 ABI/类型/时限自证测试（executed，非 RT）。
//!
//! 对照 core-v1 §11 + 附录 B.4（size/align/offset）、§13.2（needs_drop == false）、
//! §3.5/附录 B.1（gate/T_block）。命令：cargo test -p khaen-rt-host abi_layout

use core::mem::{align_of, needs_drop, offset_of, size_of};

use khaen_core::abi::{
    KhaenBlockAckV1, KhaenBlockDescV1, KhaenDiagFirV1, KhaenEpochInfoV1, KhaenEventV1,
    KhaenHwParamsV1, KhaenPathV1, KhaenPlanHeaderV1, KhaenRoleV1, KhaenSpscHeaderV1,
    KhaenWorkerAffinityV1, KhaenWorkerStatsV1,
};
use khaen_core::rt::{
    gate_ns_floor, t_block_ns_ceil, BlockIo, PlanRef, PlanView, RtControl as CoreRtControl,
    RtStatus, WorkerRtCtx,
};
use khaen_rt_host::admission::{AdmissionEnv, AdmissionReport};
use khaen_rt_host::fault::{EventRingLane, SafetyAction, WatchdogAction};
use khaen_rt_host::format::FormatStats;
use khaen_rt_host::swap::RtControl;

#[test]
fn abi_layout_matches_core_v1_appendix_b() {
    // 64 B，align 64 结构
    assert_eq!(size_of::<KhaenBlockDescV1>(), 64);
    assert_eq!(align_of::<KhaenBlockDescV1>(), 64);
    assert_eq!(offset_of!(KhaenBlockDescV1, epoch_id), 8);
    assert_eq!(offset_of!(KhaenBlockDescV1, block_seq), 16);
    assert_eq!(offset_of!(KhaenBlockDescV1, frame_index), 24);
    assert_eq!(offset_of!(KhaenBlockDescV1, plan_version), 32);
    assert_eq!(offset_of!(KhaenBlockDescV1, flags), 36);
    assert_eq!(offset_of!(KhaenBlockDescV1, valid_frames), 40);
    assert_eq!(offset_of!(KhaenBlockDescV1, slot_index), 44);
    assert_eq!(offset_of!(KhaenBlockDescV1, mono_publish_ns), 48);
    assert_eq!(offset_of!(KhaenBlockDescV1, play_deadline_ns), 56);

    assert_eq!(size_of::<KhaenEpochInfoV1>(), 64);
    assert_eq!(align_of::<KhaenEpochInfoV1>(), 64);
    assert_eq!(offset_of!(KhaenEpochInfoV1, plan_version), 8);
    assert_eq!(offset_of!(KhaenEpochInfoV1, flags), 24);
    assert_eq!(offset_of!(KhaenEpochInfoV1, start_mono_ns), 32);

    assert_eq!(size_of::<KhaenEventV1>(), 64);
    assert_eq!(align_of::<KhaenEventV1>(), 64);
    assert_eq!(offset_of!(KhaenEventV1, kind), 32);
    assert_eq!(offset_of!(KhaenEventV1, value_a), 36);
    assert_eq!(offset_of!(KhaenEventV1, payload), 48);

    assert_eq!(size_of::<KhaenBlockAckV1>(), 64);
    assert_eq!(align_of::<KhaenBlockAckV1>(), 64);
    assert_eq!(offset_of!(KhaenBlockAckV1, phase_done), 16);
    assert_eq!(offset_of!(KhaenBlockAckV1, cutoff), 28);
    assert_eq!(offset_of!(KhaenBlockAckV1, plan_version), 32);
    assert_eq!(offset_of!(KhaenBlockAckV1, deadline_mono_ns), 40);

    assert_eq!(size_of::<KhaenSpscHeaderV1>(), 64);
    assert_eq!(align_of::<KhaenSpscHeaderV1>(), 64);
    assert_eq!(offset_of!(KhaenSpscHeaderV1, write_pos), 16);
    assert_eq!(offset_of!(KhaenSpscHeaderV1, dropped), 32);

    assert_eq!(size_of::<KhaenWorkerStatsV1>(), 64);
    assert_eq!(align_of::<KhaenWorkerStatsV1>(), 64);

    assert_eq!(size_of::<KhaenHwParamsV1>(), 64);
    assert_eq!(align_of::<KhaenHwParamsV1>(), 64);
    assert_eq!(offset_of!(KhaenHwParamsV1, sample_rate), 8);
    assert_eq!(offset_of!(KhaenHwParamsV1, flags), 28);

    // 128 B，align 64
    assert_eq!(size_of::<KhaenPlanHeaderV1>(), 128);
    assert_eq!(align_of::<KhaenPlanHeaderV1>(), 64);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, magic), 0);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, header_size), 6);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, graph_id), 36);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, total_coeff_bytes), 68);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, role_table_offset), 88);
    // T105/v1.1 I15 + errata E1：repr(C) 合法对齐版（spec 字面 94/98/102/106 的 u32 不可表达）。
    assert_eq!(offset_of!(KhaenPlanHeaderV1, role_table_count), 92);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, reserved1), 94);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, affinity_table_offset), 96);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, affinity_count), 100);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, reserved2), 102);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, ctl_meta_offset), 104);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, ctl_meta_len), 108);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, reserved3), 112);
    assert_eq!(offset_of!(KhaenPlanHeaderV1, header_crc32c), 124);

    // KhaenWorkerAffinityV1：§11.13 I15 / B-18，32 B / align 8（plan POD 表项，align 例外）。
    assert_eq!(size_of::<KhaenWorkerAffinityV1>(), 32);
    assert_eq!(align_of::<KhaenWorkerAffinityV1>(), 8);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, worker_id), 0);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, sched_priority), 1);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, flags), 2);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, cpu_index), 4);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, reserved0), 6);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, irq_dependency_mask), 8);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, reserved1), 16);
    assert_eq!(offset_of!(KhaenWorkerAffinityV1, reserved2), 24);

    // 32 B，align 8
    assert_eq!(size_of::<KhaenPathV1>(), 32);
    assert_eq!(align_of::<KhaenPathV1>(), 8);
    assert_eq!(offset_of!(KhaenPathV1, coeff_offset_bytes), 16);
    assert_eq!(offset_of!(KhaenPathV1, coeff_len_bytes), 24);

    assert_eq!(size_of::<KhaenDiagFirV1>(), 32);
    assert_eq!(align_of::<KhaenDiagFirV1>(), 8);
    assert_eq!(offset_of!(KhaenDiagFirV1, coeff_offset_bytes), 16);

    assert_eq!(size_of::<KhaenRoleV1>(), 32);
    assert_eq!(align_of::<KhaenRoleV1>(), 8);
    assert_eq!(offset_of!(KhaenRoleV1, delay_samples), 8);
    assert_eq!(offset_of!(KhaenRoleV1, gain_q15), 12);
    assert_eq!(offset_of!(KhaenRoleV1, name_hash), 20);
}

#[test]
fn rt_visible_types_have_no_drop() {
    // core-v1 §13.2 清单：RT 可见类型必须 needs_drop == false。
    assert!(!needs_drop::<RtStatus>());
    assert!(!needs_drop::<PlanRef>());
    assert!(!needs_drop::<PlanView<'static>>());
    assert!(!needs_drop::<BlockIo<'static>>());
    assert!(!needs_drop::<WorkerRtCtx<'static>>());
    assert!(!needs_drop::<KhaenBlockDescV1>());
    assert!(!needs_drop::<KhaenEpochInfoV1>());
    assert!(!needs_drop::<KhaenEventV1>());
    assert!(!needs_drop::<KhaenHwParamsV1>());
    assert!(!needs_drop::<KhaenPlanHeaderV1>());
    assert!(!needs_drop::<KhaenWorkerAffinityV1>());
    assert!(!needs_drop::<KhaenBlockAckV1>());
    assert!(!needs_drop::<KhaenSpscHeaderV1>());
    assert!(!needs_drop::<KhaenWorkerStatsV1>());
    assert!(!needs_drop::<KhaenPathV1>());
    assert!(!needs_drop::<KhaenDiagFirV1>());
    assert!(!needs_drop::<KhaenRoleV1>());
    assert!(!needs_drop::<RtControl>());
    assert!(!needs_drop::<CoreRtControl>());
    assert!(!needs_drop::<EventRingLane>());
    assert!(!needs_drop::<SafetyAction>());
    assert!(!needs_drop::<WatchdogAction>());
    assert!(!needs_drop::<FormatStats>());
    assert!(!needs_drop::<AdmissionReport>());
    assert!(!needs_drop::<AdmissionEnv>());
}

#[test]
fn timing_and_capacity_vectors_match_appendix_b() {
    // 附录 B.1：gate = T_block/2，向下取整 ns；T_block 向上取整 ns。
    assert_eq!(t_block_ns_ceil(48_000, 128), 2_666_667);
    assert_eq!(t_block_ns_ceil(48_000, 64), 1_333_334);
    assert_eq!(t_block_ns_ceil(96_000, 128), 1_333_334);
    assert_eq!(t_block_ns_ceil(96_000, 64), 666_667);
    assert_eq!(gate_ns_floor(48_000, 128), 1_333_333);
    assert_eq!(gate_ns_floor(48_000, 64), 666_666);
    assert_eq!(gate_ns_floor(96_000, 128), 666_666);
    assert_eq!(gate_ns_floor(96_000, 64), 333_333);
    // 附录 B.3：payload/环/事件容量。
    let capture_payload = 20 * 128 * 4;
    let playback_payload = 22 * 128 * 4;
    assert_eq!(capture_payload, 10_240);
    assert_eq!(playback_payload, 11_264);
    assert_eq!(4 * capture_payload, 40_960);
    assert_eq!(4 * playback_payload, 45_056);
    assert_eq!(1024 * 64, 65_536);
    assert_eq!(8 * playback_payload, 90_112);
}
