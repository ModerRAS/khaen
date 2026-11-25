//! abi-layout：core-v1 §11 结构布局 + §3 时限/容量向量自检（T06）。
//!
//! 期望值来源：core-v1 **v1.1.2** 附录 B.1/B.3/B.4（Python struct 独立复算）+ §11.2
//! 布局常量表（errata E1/E2）。本文件断言数 = `RT_CHECK_ABI_LAYOUT_SCANNED`（§11.2）。
//!
//! v2 迁移说明：BlockDesc 已由 [`khaen_core::rt::BlockDesc`] 提供（v2 冻结布局与 v1 逐字段
//! 一致），本检查直接校验真实 core 类型；其余 v1 结构仍对 [`crate::contract`] 的 v1.1.2 冻结
//! 镜像运行（TODO(core)：v2 对应类型落地后替换，预期值按 v2 契约重写）。

/// core-v1 §11.2 v1.1.2 期望的 abi-layout 检查项数（旧 109；errata E1 +11 → 120）。
///
/// 新增/删除检查必须同步改本常量与 spec §11.2（否则 cli_smoke 断言会失败）。
pub const RT_CHECK_ABI_LAYOUT_SCANNED: usize = 120;

use core::mem::{align_of, offset_of, size_of};

use khaen_core::rt::{self as core_rt, BlockDesc};

use crate::contract::abi::{
    KhaenBlockAckV1, KhaenDiagFirV1, KhaenEpochInfoV1, KhaenEventV1, KhaenHwParamsV1, KhaenPathV1,
    KhaenPlanHeaderV1, KhaenRoleV1, KhaenSpscHeaderV1, KhaenWorkerAffinityV1, KhaenWorkerStatsV1,
};
use crate::contract::rt;

use crate::Report;

macro_rules! check_size_align {
    ($report:expr, $name:literal, $t:ty, $size:expr, $align:expr) => {{
        $report.scanned += 1;
        let got_size = size_of::<$t>();
        let got_align = align_of::<$t>();
        if got_size != $size {
            $report.push($name, 0, "B-02", format!("size {} != {}", got_size, $size));
        }
        if got_align != $align {
            $report.push(
                $name,
                0,
                "B-02",
                format!("align {} != {}", got_align, $align),
            );
        }
    }};
}

macro_rules! check_offset {
    ($report:expr, $name:literal, $t:ty, $field:ident, $want:expr) => {{
        $report.scanned += 1;
        let got = offset_of!($t, $field);
        if got != $want {
            $report.push(
                $name,
                0,
                "B-02",
                format!("offset {} {} != {}", stringify!($field), got, $want),
            );
        }
    }};
}

/// 运行全部 abi-layout 检查。
pub fn run() -> Report {
    let mut report = Report::default();

    check_size_align!(report, "BlockDesc", BlockDesc, 64, 64);
    check_offset!(report, "BlockDesc", BlockDesc, epoch_id, 8);
    check_offset!(report, "BlockDesc", BlockDesc, block_seq, 16);
    check_offset!(report, "BlockDesc", BlockDesc, frame_index, 24);
    check_offset!(report, "BlockDesc", BlockDesc, plan_version, 32);
    check_offset!(report, "BlockDesc", BlockDesc, flags, 36);
    check_offset!(report, "BlockDesc", BlockDesc, valid_frames, 40);
    check_offset!(report, "BlockDesc", BlockDesc, slot_index, 44);
    check_offset!(report, "BlockDesc", BlockDesc, mono_publish_ns, 48);
    check_offset!(report, "BlockDesc", BlockDesc, play_deadline_ns, 56);

    check_size_align!(report, "KhaenEpochInfoV1", KhaenEpochInfoV1, 64, 64);
    check_offset!(
        report,
        "KhaenEpochInfoV1",
        KhaenEpochInfoV1,
        plan_version,
        8
    );
    check_offset!(
        report,
        "KhaenEpochInfoV1",
        KhaenEpochInfoV1,
        sample_rate,
        12
    );
    check_offset!(report, "KhaenEpochInfoV1", KhaenEpochInfoV1, block_size, 16);
    check_offset!(report, "KhaenEpochInfoV1", KhaenEpochInfoV1, n_in, 18);
    check_offset!(report, "KhaenEpochInfoV1", KhaenEpochInfoV1, n_out, 20);
    check_offset!(report, "KhaenEpochInfoV1", KhaenEpochInfoV1, mode, 22);
    check_offset!(report, "KhaenEpochInfoV1", KhaenEpochInfoV1, flags, 24);
    check_offset!(
        report,
        "KhaenEpochInfoV1",
        KhaenEpochInfoV1,
        start_mono_ns,
        32
    );

    check_size_align!(report, "KhaenHwParamsV1", KhaenHwParamsV1, 64, 64);
    check_offset!(report, "KhaenHwParamsV1", KhaenHwParamsV1, sample_format, 2);
    check_offset!(report, "KhaenHwParamsV1", KhaenHwParamsV1, n_in, 4);
    check_offset!(report, "KhaenHwParamsV1", KhaenHwParamsV1, n_out, 6);
    check_offset!(report, "KhaenHwParamsV1", KhaenHwParamsV1, sample_rate, 8);
    check_offset!(
        report,
        "KhaenHwParamsV1",
        KhaenHwParamsV1,
        period_frames,
        12
    );
    check_offset!(
        report,
        "KhaenHwParamsV1",
        KhaenHwParamsV1,
        buffer_frames,
        16
    );
    check_offset!(
        report,
        "KhaenHwParamsV1",
        KhaenHwParamsV1,
        in_stride_bytes,
        20
    );
    check_offset!(
        report,
        "KhaenHwParamsV1",
        KhaenHwParamsV1,
        out_stride_bytes,
        24
    );
    check_offset!(report, "KhaenHwParamsV1", KhaenHwParamsV1, flags, 28);

    check_size_align!(report, "KhaenPlanHeaderV1", KhaenPlanHeaderV1, 128, 64);
    check_offset!(report, "KhaenPlanHeaderV1", KhaenPlanHeaderV1, magic, 0);
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        header_size,
        6
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        plan_version,
        8
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        sample_rate,
        12
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        block_size,
        16
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        low_band_count,
        22
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        path_count,
        24
    );
    check_offset!(report, "KhaenPlanHeaderV1", KhaenPlanHeaderV1, max_taps, 28);
    check_offset!(report, "KhaenPlanHeaderV1", KhaenPlanHeaderV1, workers, 32);
    check_offset!(report, "KhaenPlanHeaderV1", KhaenPlanHeaderV1, graph_id, 36);
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        total_coeff_bytes,
        68
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        path_table_offset,
        72
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        diag_table_offset,
        76
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        in_stride,
        80
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        out_stride,
        84
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        role_table_offset,
        88
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        role_table_count,
        92
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        reserved1,
        94
    );
    // T105 errata E1（repr(C) 合法对齐版）：spec 字面 94/98/102/106 的 u32 不可表达。
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        affinity_table_offset,
        96
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        affinity_count,
        100
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        reserved2,
        102
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        ctl_meta_offset,
        104
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        ctl_meta_len,
        108
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        reserved3,
        112
    );
    check_offset!(
        report,
        "KhaenPlanHeaderV1",
        KhaenPlanHeaderV1,
        header_crc32c,
        124
    );

    check_size_align!(
        report,
        "KhaenWorkerAffinityV1",
        KhaenWorkerAffinityV1,
        32,
        8
    );
    check_offset!(
        report,
        "KhaenWorkerAffinityV1",
        KhaenWorkerAffinityV1,
        worker_id,
        0
    );
    check_offset!(
        report,
        "KhaenWorkerAffinityV1",
        KhaenWorkerAffinityV1,
        sched_priority,
        1
    );
    check_offset!(
        report,
        "KhaenWorkerAffinityV1",
        KhaenWorkerAffinityV1,
        flags,
        2
    );
    check_offset!(
        report,
        "KhaenWorkerAffinityV1",
        KhaenWorkerAffinityV1,
        cpu_index,
        4
    );
    check_offset!(
        report,
        "KhaenWorkerAffinityV1",
        KhaenWorkerAffinityV1,
        irq_dependency_mask,
        8
    );

    check_size_align!(report, "KhaenEventV1", KhaenEventV1, 64, 64);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, mono_ns, 8);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, epoch_id, 16);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, block_seq, 24);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, kind, 32);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, lane, 34);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, severity, 35);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, value_a, 36);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, value_b, 40);
    check_offset!(report, "KhaenEventV1", KhaenEventV1, payload, 48);

    check_size_align!(report, "KhaenBlockAckV1", KhaenBlockAckV1, 64, 64);
    check_offset!(report, "KhaenBlockAckV1", KhaenBlockAckV1, phase_done, 16);
    check_offset!(report, "KhaenBlockAckV1", KhaenBlockAckV1, cutoff, 28);
    check_offset!(report, "KhaenBlockAckV1", KhaenBlockAckV1, plan_version, 32);
    check_offset!(
        report,
        "KhaenBlockAckV1",
        KhaenBlockAckV1,
        expected_workers,
        36
    );
    check_offset!(report, "KhaenBlockAckV1", KhaenBlockAckV1, flags, 37);
    check_offset!(
        report,
        "KhaenBlockAckV1",
        KhaenBlockAckV1,
        deadline_mono_ns,
        40
    );
    check_offset!(
        report,
        "KhaenBlockAckV1",
        KhaenBlockAckV1,
        compute_ticks,
        48
    );

    check_size_align!(report, "KhaenSpscHeaderV1", KhaenSpscHeaderV1, 64, 64);
    check_offset!(report, "KhaenSpscHeaderV1", KhaenSpscHeaderV1, capacity, 8);
    check_offset!(
        report,
        "KhaenSpscHeaderV1",
        KhaenSpscHeaderV1,
        entry_stride,
        12
    );
    check_offset!(
        report,
        "KhaenSpscHeaderV1",
        KhaenSpscHeaderV1,
        write_pos,
        16
    );
    check_offset!(report, "KhaenSpscHeaderV1", KhaenSpscHeaderV1, read_pos, 24);
    check_offset!(report, "KhaenSpscHeaderV1", KhaenSpscHeaderV1, dropped, 32);
    check_offset!(report, "KhaenSpscHeaderV1", KhaenSpscHeaderV1, epoch_id, 40);

    check_size_align!(report, "KhaenWorkerStatsV1", KhaenWorkerStatsV1, 64, 64);

    check_size_align!(report, "KhaenPathV1", KhaenPathV1, 32, 8);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, in_index, 0);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, out_index, 2);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, taps, 4);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, delay_samples, 8);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, polarity, 12);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, flags, 13);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, coeff_offset_bytes, 16);
    check_offset!(report, "KhaenPathV1", KhaenPathV1, coeff_len_bytes, 24);

    check_size_align!(report, "KhaenDiagFirV1", KhaenDiagFirV1, 32, 8);
    check_offset!(report, "KhaenDiagFirV1", KhaenDiagFirV1, out_index, 0);
    check_offset!(report, "KhaenDiagFirV1", KhaenDiagFirV1, flags, 2);
    check_offset!(report, "KhaenDiagFirV1", KhaenDiagFirV1, taps, 4);
    check_offset!(report, "KhaenDiagFirV1", KhaenDiagFirV1, delay_samples, 8);
    check_offset!(
        report,
        "KhaenDiagFirV1",
        KhaenDiagFirV1,
        coeff_offset_bytes,
        16
    );
    check_offset!(
        report,
        "KhaenDiagFirV1",
        KhaenDiagFirV1,
        coeff_len_bytes,
        24
    );

    check_size_align!(report, "KhaenRoleV1", KhaenRoleV1, 32, 8);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, role_id, 0);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, out_index, 2);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, flags, 4);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, delay_samples, 8);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, gain_q15, 12);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, polarity, 14);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, peq_count, 16);
    check_offset!(report, "KhaenRoleV1", KhaenRoleV1, name_hash, 20);

    // 附录 B.1：时限公式（floor gate / ceil T_block）。
    let timing: [(u32, u16, u64, u64); 4] = [
        (48_000, 128, 1_333_333, 2_666_667),
        (48_000, 64, 666_666, 1_333_334),
        (96_000, 128, 666_666, 1_333_334),
        (96_000, 64, 333_333, 666_667),
    ];
    for (fs, block, gate, tblock) in timing {
        report.scanned += 1;
        let got_gate = rt::gate_ns_floor(fs, block);
        let got_tblock = rt::t_block_ns_ceil(fs, block);
        if got_gate != gate {
            report.push(
                "timing",
                0,
                "B-01",
                format!("gate_ns({fs},{block}) {got_gate} != {gate}"),
            );
        }
        if got_tblock != tblock {
            report.push(
                "timing",
                0,
                "B-01",
                format!("t_block_ns({fs},{block}) {got_tblock} != {tblock}"),
            );
        }
    }

    // 附录 B.2/B.3：融合长度与容量边界。
    let fused: [(u32, u32, u32); 3] = [(4096, 1024, 5119), (8192, 4096, 12287), (2048, 2048, 4095)];
    for (l_g, l_p, want) in fused {
        report.scanned += 1;
        let got = rt::fused_len_max(l_g, l_p);
        if got != want {
            report.push(
                "fused",
                0,
                "B-02",
                format!("fused_len_max({l_g},{l_p}) {got} != {want}"),
            );
        }
    }
    report.scanned += 1;
    // 附录 B.3：128 帧 f32 的 capture/playback payload 字节 = 通道数 × 128 × 4。
    // 通道上限直接取自 khaen-core 的冻结契约常量。
    let capture_bytes = core_rt::MAX_INPUT_CHANNELS as u64 * 128 * 4;
    let playback_bytes = core_rt::MAX_OUTPUT_CHANNELS as u64 * 128 * 4;
    if capture_bytes != 10_240 || playback_bytes != 11_264 {
        report.push(
            "capacity",
            0,
            "B-03",
            "capture/playback payload 字节不等于附录 B.3".to_string(),
        );
    }

    report
}
