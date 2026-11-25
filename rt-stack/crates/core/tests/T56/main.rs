//! T56 schedule_plan 私有测试（WBS T56；写域 crates/core/tests/T56/）。
//!
//! 覆盖 WBS T56 DoD 与依赖口径：
//! - 全图静态 hybrid DAG：B1 pre/A/input-FFT、B2 矩阵 G/频点 MAC、B3 reduce/diag P/IFFT/
//!   protect/scatter；含 G、对角 P、FFT、reduce，固定 4 reduce lane 与 ACK_MASK 参数化；
//! - taps profile：常规 4096 / extended 8192 / extended 16384 显式准入 / >16384 仅 offline；
//!   4097..8191 等未列出值不得放行（T55 TapsProfile 同口径）；
//! - 每 route 支撑、融合长度 L_F=L_G+L_P-1、完整路径 L_A+L_G+L_P-2 与降级（禁止静默截尾）；
//! - worker 数 4/2 独立 profile：2 核重准入门、ACK_MASK(2)=0x3、固定 reduce 顺序；
//! - 非固定 25 路径：25/36/64/128 路径与 7/8/10/16 角色都能建图并如实分配负载；
//! - 成本模型对拍 DSP-HOST-CPU-BUDGET 第 2/3/10 节：4.1.2/4096 direct 9,830.4 GFLOP/s、
//!   25 条 5119 fused 12.2856 GFLOP/s、共享 FFT B128/4096 328.8 MFLOP/s、B64/8192 1264.8 MFLOP/s；
//! - deadline 预算准入（50% 门 1,333,333 ns / 666,666 ns）与算力敏感性；
//! - 热切 crossfade 准入：同形态 + D 对齐 + 双图 CPU/RAM 峰值；不足 -> 硬切+静音/新 epoch；
//! - fallback 图（G=I 保 P_post 保护）预分配/时限准入与 safety mute 结论；
//! - 内存模型（系数/表/历史/scratch，T08/T18 口径）与确定性（同输入同计划）。
//!
//! Cargo 自动把 tests/T56/main.rs 发现为集成测试 target（不需要 public_registration 注册）。
//! 运行：cargo test -p tspi-dsp-core schedule_plan -- --nocapture
//!
//! 本测试只消费 T09/T12/T55 已合入的只读 API；不依赖板端/ALSA/RT 实测（NOT_RUN）。

use tspi_dsp_core::abi::{TspiDiagFirV1, TspiPathV1, TspiRoleV1};
use tspi_dsp_core::compile::FusionForm;
use tspi_dsp_core::direct::{DirectTopology, DIRECT_NO_SOURCE};
use tspi_dsp_core::rt::RtStatus;
use tspi_dsp_core::schedule_plan::{
    admit_deadline, admit_hot_swap, admit_runtime, build_fallback_schedule, build_offline_schedule,
    build_schedule, evaluate_fallback, evaluate_hot_swap, fft_size_for, fft_transform_flops,
    fft_transform_macs, full_path_len, fused_len, recommended_kernel, route_support,
    taps_profile_of, DeadlineBudgets, FallbackBudgets, FallbackReason, GraphClass,
    HotSwapDisposition, HotSwapSpec, KernelForm, PlanProfile, RuntimeBudgets, SchedulePlan,
    StageKind, TapsProfile, ERR_CROSSFADE_BUDGET, RT_E_CROSSFADE_ALIGN, SCHEDULE_MAX_TASKS,
    SCHEDULE_PHASES, SCHEDULE_STAGE_KINDS, SCHEDULE_WORKERS_FALLBACK, SCHEDULE_WORKERS_MAX,
};
use tspi_dsp_core::workers::{ack_mask, WorkerDagPlan};

const GATE_48K_128: u64 = 1_333_333;
const GATE_48K_64: u64 = 666_666;

fn profile_default() -> PlanProfile {
    PlanProfile::default()
}

fn align64(bytes: u64) -> u64 {
    let rem = bytes % 64;
    if rem == 0 {
        bytes
    } else {
        bytes + (64 - rem)
    }
}

#[test]
fn schedule_plan_taps_profile_boundaries() {
    assert_eq!(TapsProfile::for_taps(0), TapsProfile::Regular);
    assert_eq!(TapsProfile::for_taps(1), TapsProfile::Regular);
    assert_eq!(TapsProfile::for_taps(4096), TapsProfile::Regular);
    assert_eq!(TapsProfile::for_taps(4097), TapsProfile::NotAdmitted);
    assert_eq!(TapsProfile::for_taps(6000), TapsProfile::NotAdmitted);
    assert_eq!(TapsProfile::for_taps(8191), TapsProfile::NotAdmitted);
    assert_eq!(TapsProfile::for_taps(8192), TapsProfile::Extended);
    assert_eq!(TapsProfile::for_taps(8193), TapsProfile::NotAdmitted);
    assert_eq!(TapsProfile::for_taps(16383), TapsProfile::NotAdmitted);
    assert_eq!(TapsProfile::for_taps(16384), TapsProfile::Extended);
    assert_eq!(TapsProfile::for_taps(16385), TapsProfile::OfflineOnly);
    assert_eq!(TapsProfile::for_taps(262_144), TapsProfile::OfflineOnly);
    // 字符串稳定（证据/报告消费）。
    assert_eq!(TapsProfile::Regular.as_str(), "regular");
    assert_eq!(TapsProfile::Extended.as_str(), "extended");
    assert_eq!(TapsProfile::OfflineOnly.as_str(), "offline_only");
    assert_eq!(TapsProfile::NotAdmitted.as_str(), "not_admitted");
    assert!(TapsProfile::Regular.is_runtime_candidate());
    assert!(TapsProfile::Extended.is_runtime_candidate());
    assert!(!TapsProfile::OfflineOnly.is_runtime_candidate());
    assert!(!TapsProfile::NotAdmitted.is_runtime_candidate());
}

#[test]
fn schedule_plan_profile_validation_rejects() {
    let p = PlanProfile {
        sample_rate_hz: 44_100,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_RATE);

    let p = PlanProfile {
        block_size: 256,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_BLOCK_SIZE);

    let p = PlanProfile {
        workers: 3,
        ..profile_default()
    };
    assert_eq!(
        build_schedule(&p).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );

    let p = PlanProfile {
        n_role: 17,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_ROLES);

    let p = PlanProfile {
        container_channels: 6,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_CHANNELS);

    let p = PlanProfile {
        m: 0,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_ROLES);

    let p = PlanProfile {
        m: 9,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_ROLES);

    let p = PlanProfile {
        d_samples: 4096,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_TAPS);

    let p = PlanProfile {
        max_taps: 4095,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_TAPS);

    let p = PlanProfile {
        matrix_paths: 129,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_PATH_TABLE);

    let p = PlanProfile {
        matrix_paths: 40,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_PATH_TABLE);

    let p = PlanProfile {
        diag_firs: 8,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_ROLES);

    let p = PlanProfile {
        total_delay_q16: u32::MAX as u64 + 1,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_BAD_LAYOUT);

    let p = PlanProfile {
        d_samples: 8,
        total_delay_q16: (7u64) << 16,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_BAD_LAYOUT);

    let p = PlanProfile {
        l_a: 0,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_TAPS);

    let p = PlanProfile {
        max_taps: 5000,
        ..profile_default()
    };
    assert_eq!(build_schedule(&p).unwrap_err(), RtStatus::ERR_TAPS);
}

#[test]
fn schedule_plan_direct_factorized_full_graph() {
    let profile = profile_default();
    let plan = build_schedule(&profile).expect("default 4.1.2 direct factorized must build");
    assert_eq!(plan.graph_class, GraphClass::Normal);
    assert_eq!(plan.kernel_form, KernelForm::Direct);
    assert_eq!(plan.fusion_effective, FusionForm::Factorized);
    assert!(!plan.fusion_downgraded);
    assert!(!plan.offline_only);
    assert!(!plan.runtime_admitted);
    assert_eq!(plan.taps_profile, TapsProfile::Regular);
    assert_eq!(plan.route_support, 4096);
    assert_eq!(plan.fft_size, 256);
    assert_eq!(plan.partitions, 0);
    assert_eq!(plan.ack_mask, 0xF);
    assert_eq!(plan.ack_mask, ack_mask(4));
    assert_eq!(plan.reduce_lanes, 4);
    assert_eq!(plan.reduce_order, [0, 1, 2, 3]);
    assert_eq!(plan.matrix_tiles, 7);
    assert_eq!(plan.matrix_paths, 25);
    assert_eq!(plan.diag_tiles, 7);
    assert_eq!(plan.fft_transforms, 0);

    // 全图任务表：B1 = 7 pre + 7 A；B2 = 7 列 tile；B3 = 7 reduce + 7 diag + 7 protect + 1 scatter。
    assert_eq!(plan.phase_task_count, [14, 7, 22]);
    assert_eq!(plan.task_count, 43);
    assert!(plan.task_count as usize <= SCHEDULE_MAX_TASKS);
    assert_eq!(plan.phase_task_count.iter().sum::<u16>(), plan.task_count);

    // stage 覆盖：direct factorized 有 G/P/reduce，没有 FFT。
    let w = |k: StageKind| plan.stage_work[k.index()];
    assert_eq!(w(StageKind::Pre), 7 * 128);
    assert_eq!(w(StageKind::InputFirA), 7 * 128);
    assert_eq!(w(StageKind::MatrixG), 25 * 4096 * 128);
    assert_eq!(w(StageKind::Reduce), 7 * 3 * 128);
    assert_eq!(w(StageKind::DiagFirP), 7 * 1024 * 128);
    assert_eq!(w(StageKind::Protect), 7 * 2 * 128);
    assert_eq!(w(StageKind::Scatter), 7 * 128);
    assert_eq!(w(StageKind::InputFft), 0);
    assert_eq!(w(StageKind::FftBinMac), 0);
    assert_eq!(w(StageKind::Ifft), 0);
    assert_eq!(plan.stage_work.len(), SCHEDULE_STAGE_KINDS);

    let expected_work =
        25 * 4096 * 128 + 7 * 128 * 2 + 7 * 3 * 128 + 7 * 1024 * 128 + 7 * 2 * 128 + 7 * 128;
    assert_eq!(plan.work_per_block, expected_work);
    assert_eq!(plan.phase_work_sum(), expected_work);
    assert_eq!(plan.worker_work_sum(), expected_work);

    // 成本口径（CPU-BUDGET 第 2 节）：矩阵 25*4096 = 102400 MAC/sample；2*102400*48000 FLOP/s。
    assert_eq!(plan.matrix_macs_per_sample, 102_400);
    assert_eq!(plan.input_macs_per_sample, 7);
    assert_eq!(plan.output_macs_per_sample, 7 * 1024);
    assert_eq!(
        plan.matrix_macs_per_sample * 2 * profile.sample_rate_hz as u64,
        9_830_400_000
    );
    assert_eq!(plan.flops_per_second, 2 * plan.macs_per_second);
    assert_eq!(
        plan.macs_per_second,
        plan.work_per_block * profile.sample_rate_hz as u64 / profile.block_size as u64
    );

    // deadline：LPT 结果 3,932,672 MAC -> 4 GMAC/s -> 983,168 ns < 1,333,333 ns。
    let deadline = admit_deadline(&plan, &DeadlineBudgets::default());
    assert_eq!(deadline.gate_ns, GATE_48K_128);
    assert_eq!(deadline.total_worst_ns, 983_168);
    assert_eq!(deadline.phase_worst_ns, [128, 917_504, 65_536]);
    assert_eq!(deadline.worker_block_ns[0], 983_168);
    assert_eq!(deadline.worst_worker, 0);
    assert!(deadline.admitted);

    // 4 worker 都有有效工作；负载差不超过 2 倍（6/6/6/7 起点非假均衡）。
    let mut used = 0;
    let mut min_work = u64::MAX;
    let mut max_work = 0u64;
    let mut worker = 0usize;
    while worker < 4 {
        let work = plan.worker_work_total[worker];
        assert!(work > 0);
        used += 1;
        min_work = min_work.min(work);
        max_work = max_work.max(work);
        worker += 1;
    }
    assert_eq!(used, 4);
    assert!(max_work < min_work * 2);

    // 每 task owner/phase 自洽、每 phase task 数一致。
    let mut task = 0usize;
    while task < plan.task_count as usize {
        let entry = plan.tasks[task];
        assert_eq!(entry.phase, entry.kind.phase_of());
        assert!((entry.owner as usize) < 4);
        assert!(entry.work_macs > 0);
        task += 1;
    }

    // 串行比例（T09 口径，Q16）：B1 14 tile / 4 -> 10/14；B2 7 / 4 -> 3/7；B3 22 / 4 -> 18/22。
    assert_eq!(plan.parallel_workers, [4, 4, 4]);
    assert_eq!(plan.serial_fraction_q16, [46_811, 28_086, 53_620]);
}

#[test]
fn schedule_plan_direct_fused_downgrade_and_cost() {
    // 融合可行：L_F = 2048+1024-1 = 3071 <= 4096；25 条 fused 路径。
    let fused = PlanProfile {
        l_g: 2048,
        l_p: 1024,
        fusion: FusionForm::FusedGp,
        ..profile_default()
    };
    let plan = build_schedule(&fused).expect("fused 3071 must build");
    assert_eq!(plan.fusion_effective, FusionForm::FusedGp);
    assert!(!plan.fusion_downgraded);
    assert_eq!(plan.route_support, 3071);
    assert_eq!(fused_len(&fused).unwrap(), 3071);
    assert_eq!(plan.matrix_work_taps, 3071);
    assert_eq!(plan.matrix_macs_per_sample, 25 * 3071);
    assert_eq!(plan.output_macs_per_sample, 0);
    assert_eq!(plan.diag_tiles, 0);
    assert_eq!(plan.stage_work[StageKind::DiagFirP.index()], 0);
    assert_eq!(plan.stage_work[StageKind::MatrixG.index()], 25 * 3071 * 128);
    // CPU-BUDGET 第 5 节：25 条 3071 tap fused 的 direct 成本。
    assert_eq!(
        plan.matrix_macs_per_sample * 2 * fused.sample_rate_hz as u64,
        7_370_400_000
    );

    // 融合超 tap 上限：FusedGp 5119 > 4096 -> 合法降级 factorized（禁止静默截尾）。
    let over = PlanProfile {
        fusion: FusionForm::FusedGp,
        ..profile_default()
    };
    assert_eq!(fused_len(&over).unwrap(), 5119);
    let plan = build_schedule(&over).expect("fused over tap limit must downgrade, not truncate");
    assert_eq!(plan.fusion_effective, FusionForm::Factorized);
    assert!(plan.fusion_downgraded);
    assert_eq!(plan.route_support, 4096);
    assert_eq!(plan.matrix_work_taps, 4096);
    assert_eq!(plan.diag_tiles, 7);
    assert_eq!(plan.stage_work[StageKind::DiagFirP.index()], 7 * 1024 * 128);
    // 25 条 5119 tap 的假设成本（CPU-BUDGET 第 5 节 12.2856 GFLOP/s）仍可复算。
    assert_eq!(2u64 * 25 * 5119 * 48_000, 12_285_600_000);
}

fn profile_hybrid_8192() -> PlanProfile {
    PlanProfile {
        l_g: 8192,
        l_p: 1024,
        max_taps: 8192,
        extended_taps_admitted: true,
        kernel: KernelForm::Hybrid,
        ..profile_default()
    }
}

fn profile_direct_8192() -> PlanProfile {
    PlanProfile {
        l_g: 8192,
        l_p: 1024,
        max_taps: 8192,
        extended_taps_admitted: true,
        ..profile_default()
    }
}

#[test]
fn schedule_plan_extended_8192_admission() {
    let mut profile = profile_direct_8192();
    profile.extended_taps_admitted = false;
    assert_eq!(build_schedule(&profile).unwrap_err(), RtStatus::ERR_TAPS);

    let direct = build_schedule(&profile_direct_8192()).expect("8192 extended direct build");
    assert_eq!(direct.taps_profile, TapsProfile::Extended);
    assert_eq!(direct.route_support, 8192);
    assert_eq!(direct.fft_transforms, 0);
    let deadline = admit_deadline(&direct, &DeadlineBudgets::default());
    // 4.1.2/8192 direct：矩阵 4 个 4,194,304 tile + 3 个 3,145,728 tile -> 最坏 7,340,032。
    assert_eq!(deadline.total_worst_ns, 1_900_672);
    assert!(
        !deadline.admitted,
        "8192 direct must be rejected at 8 GFLOPS sustained"
    );

    let hybrid = build_schedule(&profile_hybrid_8192()).expect("8192 extended hybrid build");
    assert_eq!(hybrid.kernel_form, KernelForm::Hybrid);
    assert_eq!(hybrid.taps_profile, TapsProfile::Extended);
    assert_eq!(hybrid.route_support, 8192);
    assert_eq!(hybrid.partitions, 64);
    assert_eq!(hybrid.fft_transforms, 14);
    // 矩阵走 FFT：25 路径 x 64 分区 x 129 bin x 4 MAC 当量。
    assert_eq!(
        hybrid.stage_work[StageKind::FftBinMac.index()],
        25 * 64 * 129 * 4
    );
    // A/P 仍时域直算（hybrid 卖点：低带长矩阵与中高带对角 FIR 同图分核）。
    assert_eq!(hybrid.stage_work[StageKind::InputFirA.index()], 7 * 1 * 128);
    assert_eq!(
        hybrid.stage_work[StageKind::DiagFirP.index()],
        7 * 1024 * 128
    );
    assert_eq!(
        hybrid.fft_transforms as u64 * fft_transform_macs(hybrid.fft_size as u32),
        14u64 * 2560
    );
    let hybrid_deadline = admit_deadline(&hybrid, &DeadlineBudgets::default());
    assert!(
        hybrid_deadline.admitted,
        "8192 extended must be admissible via explicit FFT plan"
    );
    assert!(hybrid_deadline.total_worst_ns < direct_deadline_reject(&direct));
}

fn direct_deadline_reject(plan: &SchedulePlan) -> u64 {
    admit_deadline(plan, &DeadlineBudgets::default()).total_worst_ns
}

fn profile_hybrid_16384() -> PlanProfile {
    PlanProfile {
        l_g: 16_384,
        l_p: 1024,
        max_taps: 16_384,
        extended_taps_admitted: true,
        kernel: KernelForm::Hybrid,
        ..profile_default()
    }
}

#[test]
fn schedule_plan_16384_hybrid_and_sharedfft() {
    // 16384 必须显式 extended 准入。
    let mut missing = profile_hybrid_16384();
    missing.extended_taps_admitted = false;
    assert_eq!(build_schedule(&missing).unwrap_err(), RtStatus::ERR_TAPS);

    let direct = PlanProfile {
        kernel: KernelForm::Direct,
        fusion: FusionForm::Factorized,
        ..profile_hybrid_16384()
    };
    let plan = build_schedule(&direct).expect("16384 direct factorized build");
    assert_eq!(plan.taps_profile, TapsProfile::Extended);
    let deadline = admit_deadline(&plan, &DeadlineBudgets::default());
    assert!(!deadline.admitted);

    let hybrid = build_schedule(&profile_hybrid_16384()).expect("16384 hybrid build");
    assert_eq!(hybrid.taps_profile, TapsProfile::Extended);
    assert_eq!(hybrid.partitions, 128);
    assert_eq!(
        hybrid.stage_work[StageKind::FftBinMac.index()],
        25 * 128 * 129 * 4
    );
    let deadline = admit_deadline(&hybrid, &DeadlineBudgets::default());
    assert!(
        deadline.admitted,
        "16384 hybrid plan must pass default deadline budget"
    );

    // SharedFft 全融合：L_A+L_G+L_P-2 = 1+16384+1024-2 = 17407 > 16384 -> 未列出/offline 档，拒绝。
    let shared = PlanProfile {
        kernel: KernelForm::SharedFft,
        fusion: FusionForm::FusedGp,
        ..profile_hybrid_16384()
    };
    assert_eq!(route_support(&shared).unwrap(), 17_407);
    assert_eq!(taps_profile_of(&shared).unwrap(), TapsProfile::OfflineOnly);
    assert_eq!(build_schedule(&shared).unwrap_err(), RtStatus::ERR_TAPS);
    assert_eq!(full_path_len(&shared).unwrap(), 17_407);
}

fn profile_direct_offline() -> PlanProfile {
    PlanProfile {
        l_g: 20_000,
        l_p: 1,
        max_taps: 32_768,
        extended_taps_admitted: true,
        diag_firs: 0,
        ..profile_default()
    }
}

#[test]
fn schedule_plan_offline_only_above_16384() {
    let profile = profile_direct_offline();
    assert_eq!(route_support(&profile).unwrap(), 20_000);
    assert_eq!(taps_profile_of(&profile).unwrap(), TapsProfile::OfflineOnly);
    assert_eq!(build_schedule(&profile).unwrap_err(), RtStatus::ERR_TAPS);

    let plan = build_offline_schedule(&profile).expect("offline design plan must build");
    assert!(plan.offline_only);
    assert_eq!(plan.graph_class, GraphClass::OfflineDesign);
    assert!(!plan.runtime_admitted);
    assert_eq!(plan.taps_profile, TapsProfile::OfflineOnly);
    assert!(plan.task_count > 0);

    // 4097..8191 / 8193..16383 是 NotAdmitted：runtime 与 offline 都拒绝。
    let middle = PlanProfile {
        l_g: 10_000,
        l_p: 1,
        max_taps: 16_384,
        extended_taps_admitted: true,
        diag_firs: 0,
        ..profile_default()
    };
    assert_eq!(taps_profile_of(&middle).unwrap(), TapsProfile::NotAdmitted);
    assert_eq!(build_schedule(&middle).unwrap_err(), RtStatus::ERR_TAPS);
    assert_eq!(
        build_offline_schedule(&middle).unwrap_err(),
        RtStatus::ERR_TAPS
    );

    // offline 硬上限。
    let huge = PlanProfile {
        l_g: 300_000,
        l_p: 1,
        max_taps: 300_000,
        extended_taps_admitted: true,
        ..profile_default()
    };
    assert_eq!(
        build_offline_schedule(&huge).unwrap_err(),
        RtStatus::ERR_TAPS
    );
}

fn profile_layout(n_role: u16, container: u16, m: u16, paths: u32) -> PlanProfile {
    PlanProfile {
        n_src: n_role,
        n_role,
        container_channels: container,
        m,
        l_g: 256,
        l_p: 64,
        d_samples: 0,
        matrix_paths: paths,
        diag_firs: n_role as u32,
        max_taps: 4096,
        ..profile_default()
    }
}

#[test]
fn schedule_plan_non_25_paths_scale() {
    // 4.1.2 / 5.1.2 / 5.3.2 / 7.1.4 级布局与 25/36/64/128 路径（不是固定 25 path）。
    let cases = [
        (7u16, 8u16, 5u16, 25u32),
        (8, 10, 6, 36),
        (10, 12, 8, 64),
        (16, 17, 8, 128),
    ];
    for &(n_role, container, m, paths) in cases.iter() {
        let profile = profile_layout(n_role, container, m, paths);
        let plan = build_schedule(&profile)
            .unwrap_or_else(|err| panic!("layout O={n_role} paths={paths} build failed: {err:?}"));
        assert_eq!(plan.matrix_paths, paths);
        assert_eq!(plan.matrix_tiles, n_role);
        assert_eq!(plan.phase_task_count[1], n_role);
        assert_eq!(
            plan.stage_work[StageKind::MatrixG.index()],
            paths as u64 * 256 * 128
        );
        assert_eq!(
            plan.coeff_bytes,
            (n_role as u64 * 1 + paths as u64 * 256 + n_role as u64 * 64) * 4
        );
        let deadline = admit_deadline(&plan, &DeadlineBudgets::default());
        assert!(
            deadline.admitted,
            "small-tap layout O={n_role} paths={paths} must pass"
        );
        let mut used = 0;
        let mut worker = 0usize;
        while worker < plan.workers() as usize {
            if plan.worker_work_total[worker] > 0 {
                used += 1;
            }
            worker += 1;
        }
        assert_eq!(used, plan.workers() as i32);
    }

    // 128 条 4096tap 路径：必须如实拒绝 deadline（不是静默折成 25 条）。
    let heavy = PlanProfile {
        l_g: 4096,
        l_p: 64,
        diag_firs: 16,
        ..profile_layout(16, 17, 8, 128)
    };
    let plan = build_schedule(&heavy).expect("128 paths 4096 build");
    assert_eq!(plan.matrix_paths, 128);
    assert_eq!(plan.matrix_tiles, 16);
    let deadline = admit_deadline(&plan, &DeadlineBudgets::default());
    assert!(!deadline.admitted);
}

#[test]
fn schedule_plan_2core_readmission_gate() {
    let four = build_schedule(&profile_default()).expect("4-core plan");
    assert_eq!(four.ack_mask, 0xF);
    assert!(admit_deadline(&four, &DeadlineBudgets::default()).admitted);

    // 2 核是独立降级 profile：4.1.2/4096 direct 重准入失败（13 条最忙路径模型约 1.97ms）。
    let two = build_schedule(&profile_default().with_workers(2)).expect("2-core plan");
    assert_eq!(two.ack_mask, 0x3);
    assert_eq!(two.ack_mask, ack_mask(2));
    assert_eq!(two.reduce_order, [0, 1, 2, 3]);
    let deadline = admit_deadline(&two, &DeadlineBudgets::default());
    assert_eq!(deadline.total_worst_ns, 1_966_304);
    assert!(
        !deadline.admitted,
        "2-core direct 4096 must not be re-admitted"
    );

    // 2 核 hybrid（矩阵走 FFT）在默认预算下可以重准入。
    let two_hybrid = build_schedule(
        &PlanProfile {
            kernel: KernelForm::Hybrid,
            ..profile_default()
        }
        .with_workers(2),
    )
    .expect("2-core hybrid plan");
    assert_eq!(two_hybrid.ack_mask, 0x3);
    let deadline = admit_deadline(&two_hybrid, &DeadlineBudgets::default());
    assert!(deadline.admitted, "2-core hybrid must be re-admittable");
    assert!(deadline.total_worst_ns < GATE_48K_128);
    assert_eq!(SCHEDULE_WORKERS_FALLBACK, 2);
}

fn profile_shared_4096_b128() -> PlanProfile {
    PlanProfile {
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
        ..profile_default()
    }
}

fn profile_shared_8192_b64() -> PlanProfile {
    PlanProfile {
        block_size: 64,
        n_src: 5,
        n_role: 5,
        container_channels: 6,
        m: 5,
        l_a: 1,
        l_g: 8192,
        l_p: 1,
        d_samples: 0,
        matrix_paths: 25,
        diag_firs: 5,
        max_taps: 8192,
        extended_taps_admitted: true,
        fusion: FusionForm::FusedGp,
        kernel: KernelForm::SharedFft,
        workers: 4,
        total_delay_q16: 0,
        ..profile_default()
    }
}

#[test]
fn schedule_plan_fft_cost_matches_cpu_budget() {
    // CPU-BUDGET 第 3 节：48k/4096/B128 共享 FFT = 328.8 MFLOP/s（矩阵 309.6 + 变换 19.2）。
    let p = profile_shared_4096_b128();
    let plan = build_schedule(&p).expect("shared FFT 4096 build");
    assert_eq!(plan.kernel_form, KernelForm::SharedFft);
    assert_eq!(plan.fft_size, 256);
    assert_eq!(plan.partitions, 32);
    assert_eq!(plan.fft_transforms, 10);
    assert_eq!(
        plan.stage_work[StageKind::FftBinMac.index()],
        25 * 32 * 129 * 4
    );
    assert_eq!(plan.stage_work[StageKind::InputFft.index()], 5 * 2560);
    assert_eq!(plan.stage_work[StageKind::Ifft.index()], 5 * 2560);
    let bin_flops = plan.stage_work[StageKind::FftBinMac.index()] * 2 * 48_000 / 128;
    let xform_flops = (plan.stage_work[StageKind::InputFft.index()]
        + plan.stage_work[StageKind::Ifft.index()])
        * 2
        * 48_000
        / 128;
    assert_eq!(bin_flops, 309_600_000);
    assert_eq!(xform_flops, 19_200_000);
    assert_eq!(bin_flops + xform_flops, 328_800_000);

    // CPU-BUDGET 第 3 节：48k/8192/B64 共享 FFT = 1264.8 MFLOP/s（矩阵 1248.0 + 变换 16.8）。
    let p = profile_shared_8192_b64();
    let plan = build_schedule(&p).expect("shared FFT 8192/B64 build");
    assert_eq!(plan.fft_size, 128);
    assert_eq!(plan.partitions, 128);
    assert_eq!(
        plan.stage_work[StageKind::FftBinMac.index()],
        25 * 128 * 65 * 4
    );
    let bin_flops = plan.stage_work[StageKind::FftBinMac.index()] * 2 * 48_000 / 64;
    let xform_flops = (plan.stage_work[StageKind::InputFft.index()]
        + plan.stage_work[StageKind::Ifft.index()])
        * 2
        * 48_000
        / 64;
    assert_eq!(bin_flops, 1_248_000_000);
    assert_eq!(xform_flops, 16_800_000);
    assert_eq!(bin_flops + xform_flops, 1_264_800_000);
}

#[test]
fn schedule_plan_deadline_admission_sensitivity() {
    let plan = build_schedule(&profile_default()).expect("plan");
    let base = admit_deadline(&plan, &DeadlineBudgets::with_core_rate(4_000_000_000));
    assert_eq!(base.gate_ns, GATE_48K_128);
    assert_eq!(base.total_worst_ns, 983_168);
    assert!(base.admitted);

    let half = admit_deadline(&plan, &DeadlineBudgets::with_core_rate(2_000_000_000));
    assert_eq!(half.total_worst_ns, 1_966_336);
    assert!(!half.admitted);

    let quarter = admit_deadline(&plan, &DeadlineBudgets::with_core_rate(1_000_000_000));
    assert_eq!(quarter.total_worst_ns, 3_932_672);
    assert!(!quarter.admitted);

    // 64 帧独立窗：gate 666,666 ns；同图工作量减半后仍准入。
    let p64 = PlanProfile {
        block_size: 64,
        ..profile_default()
    };
    let plan64 = build_schedule(&p64).expect("B64 plan");
    let deadline = admit_deadline(&plan64, &DeadlineBudgets::default());
    assert_eq!(deadline.gate_ns, GATE_48K_64);
    assert_eq!(deadline.total_worst_ns, 491_584);
    assert!(deadline.admitted);

    // 更严自定义门（T_block/3）：983,168 > 888,889 -> 拒绝。
    let strict = admit_deadline(
        &plan,
        &DeadlineBudgets {
            core_macs_per_second: 4_000_000_000,
            gate_num: 1,
            gate_den: 3,
        },
    );
    assert_eq!(strict.gate_ns, 888_888);
    assert!(!strict.admitted);

    // 非法预算 -> 不准入（不 panic）。
    let invalid = admit_deadline(
        &plan,
        &DeadlineBudgets {
            core_macs_per_second: 0,
            gate_num: 1,
            gate_den: 2,
        },
    );
    assert!(!invalid.admitted);
    let invalid = admit_deadline(
        &plan,
        &DeadlineBudgets {
            core_macs_per_second: 4_000_000_000,
            gate_num: 1,
            gate_den: 0,
        },
    );
    assert!(!invalid.admitted);
}

fn hot_swap_old_new() -> (SchedulePlan, SchedulePlan) {
    let profile = PlanProfile {
        crossfade: true,
        ..profile_default()
    };
    let old = build_schedule(&profile).expect("old plan");
    let new = build_schedule(&profile).expect("new plan");
    (old, new)
}

fn hot_swap_spec() -> HotSwapSpec {
    HotSwapSpec {
        old_plan_version: 1,
        new_plan_version: 2,
        crossfade_frames: 128,
        available_cpu_ns: 2_666_667,
        available_ram_bytes: 64 * 1024 * 1024,
        core_macs_per_second: 0,
    }
}

#[test]
fn schedule_plan_hot_swap_crossfade_admission() {
    let (old, new) = hot_swap_old_new();
    let spec = hot_swap_spec();
    let verdict = evaluate_hot_swap(&old, &new, &spec);
    assert_eq!(verdict.disposition, HotSwapDisposition::Crossfade);
    assert!(verdict.admitted);
    assert!(verdict.d_aligned);
    assert_eq!(verdict.old_total_delay_q16, 512 << 16);
    assert_eq!(verdict.new_total_delay_q16, 512 << 16);
    assert_eq!(verdict.crossfade_frames, 128);
    assert_eq!(verdict.old_plan_version, 1);
    assert_eq!(verdict.new_plan_version, 2);
    // 峰值 CPU = max(一个 block 窗口 2,666,667, 双图 1,966,336) = 2,666,667 ns。
    assert_eq!(verdict.peak_cpu_ns, 2_666_667);
    assert_eq!(verdict.peak_ram_bytes, 2 * old.plan_bytes);
    assert_eq!(verdict.status, RtStatus::OK);
    let admitted = admit_hot_swap(&old, &new, &spec).expect("crossfade admitted");
    assert_eq!(admitted.disposition, HotSwapDisposition::Crossfade);

    // 双图 CPU 预算不足 -> 硬切+静音（不是静默插值）。
    let tight = HotSwapSpec {
        available_cpu_ns: 2_666_666,
        ..spec
    };
    let verdict = evaluate_hot_swap(&old, &new, &tight);
    assert_eq!(verdict.disposition, HotSwapDisposition::HardSwitchMute);
    assert!(!verdict.admitted);
    assert_eq!(verdict.status, ERR_CROSSFADE_BUDGET);
    assert_eq!(
        admit_hot_swap(&old, &new, &tight).unwrap_err(),
        ERR_CROSSFADE_BUDGET
    );

    // 双图 RAM 预算不足。
    let tight_ram = HotSwapSpec {
        available_ram_bytes: old.plan_bytes,
        ..spec
    };
    let verdict = evaluate_hot_swap(&old, &new, &tight_ram);
    assert_eq!(verdict.disposition, HotSwapDisposition::HardSwitchMute);
    assert_eq!(verdict.status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn schedule_plan_hot_swap_rejections() {
    let (_old, new) = hot_swap_old_new();
    let spec = hot_swap_spec();

    // D 未对齐：total_delay_q16 不同 -> 硬切+静音，RT_E_CROSSFADE_ALIGN。
    let d_mismatch = build_schedule(&PlanProfile {
        crossfade: true,
        total_delay_q16: 600 << 16,
        ..profile_default()
    })
    .expect("d-mismatch plan");
    let verdict = evaluate_hot_swap(&new, &d_mismatch, &spec);
    assert_eq!(verdict.disposition, HotSwapDisposition::HardSwitchMute);
    assert!(!verdict.d_aligned);
    assert_eq!(verdict.status, RT_E_CROSSFADE_ALIGN);

    // crossfade 帧数必须 == B 且 <=128。
    let frames_bad = HotSwapSpec {
        crossfade_frames: 64,
        ..spec
    };
    assert_eq!(
        evaluate_hot_swap(&new, &new, &frames_bad).status,
        RT_E_CROSSFADE_ALIGN
    );
    let frames_big = HotSwapSpec {
        crossfade_frames: 256,
        ..spec
    };
    assert_eq!(
        evaluate_hot_swap(&new, &new, &frames_big).status,
        RT_E_CROSSFADE_ALIGN
    );

    // 跨 block 变化：必须新 epoch（不能 crossfade）。
    let b64 = build_schedule(&PlanProfile {
        block_size: 64,
        crossfade: true,
        ..profile_default()
    })
    .expect("B64 plan");
    let verdict = evaluate_hot_swap(&new, &b64, &spec);
    assert_eq!(verdict.disposition, HotSwapDisposition::NewEpoch);
    assert_eq!(verdict.status, RT_E_CROSSFADE_ALIGN);

    // 新 plan 未声明 crossfade -> NotRequested（只允许硬切），admit_hot_swap 返回 unsupported。
    let not_requested = build_schedule(&profile_default()).expect("no-crossfade plan");
    let verdict = evaluate_hot_swap(&new, &not_requested, &spec);
    assert_eq!(verdict.disposition, HotSwapDisposition::NotRequested);
    assert!(!verdict.admitted);
    assert_eq!(verdict.status, RtStatus::OK);
    assert_eq!(
        admit_hot_swap(&new, &not_requested, &spec).unwrap_err(),
        RtStatus::ERR_UNSUPPORTED
    );
}

#[test]
fn schedule_plan_fallback_admission_and_safety_mute() {
    let normal = build_schedule(&profile_default()).expect("normal plan");
    let fallback = build_fallback_schedule(&normal, 4).expect("fallback plan");
    // fallback = G=I（仅 delta_D 基线），保留 A/P 与 P_post 保护。
    assert_eq!(fallback.graph_class, GraphClass::Fallback);
    assert_eq!(fallback.kernel_form, KernelForm::Direct);
    assert_eq!(fallback.fusion_effective, FusionForm::Factorized);
    assert_eq!(fallback.route_support, 1024);
    assert_eq!(fallback.taps_profile, TapsProfile::Regular);
    assert_eq!(fallback.matrix_paths, 7);
    assert_eq!(fallback.matrix_work_taps, 1);
    assert_eq!(fallback.diag_tiles, 7);
    assert!(fallback.plan_bytes < normal.plan_bytes);
    assert_eq!(fallback.ack_mask, 0xF);

    let budgets = FallbackBudgets {
        deadline: DeadlineBudgets::default(),
        available_ram_bytes: 4 * 1024 * 1024,
        preallocated: true,
        workers: 4,
    };
    let admission = evaluate_fallback(&normal, &budgets);
    assert!(admission.admitted);
    assert!(!admission.safety_mute_required);
    assert_eq!(admission.reason, FallbackReason::Admitted);
    assert!(admission.deadline.admitted);
    assert!(admission.ram_ok);
    assert!(admission.plan.is_some());
    assert_eq!(
        admission.plan_bytes,
        admission.plan.as_ref().unwrap().plan_bytes
    );

    // 未预分配 -> 只允许安全 mute（core-v1 第 5.5 节）。
    let not_preallocated = FallbackBudgets {
        preallocated: false,
        ..budgets
    };
    let admission = evaluate_fallback(&normal, &not_preallocated);
    assert!(!admission.admitted);
    assert!(admission.safety_mute_required);
    assert_eq!(admission.reason, FallbackReason::NotPreallocated);

    // RAM 不足 -> 安全 mute。
    let tight = FallbackBudgets {
        available_ram_bytes: 64,
        ..budgets
    };
    let admission = evaluate_fallback(&normal, &tight);
    assert!(admission.safety_mute_required);
    assert_eq!(admission.reason, FallbackReason::RamOverBudget);

    // 2 核 fallback 是独立 plan（ACK_MASK=0x3），小图仍可准入。
    let two = build_fallback_schedule(&normal, 2).expect("2-core fallback");
    assert_eq!(two.ack_mask, 0x3);
    let admission = evaluate_fallback(
        &normal,
        &FallbackBudgets {
            workers: 2,
            ..budgets
        },
    );
    assert!(admission.admitted);
    assert_eq!(admission.plan.as_ref().unwrap().ack_mask, 0x3);

    // fallback 图无法表达（L_P=12288 不落在规范档）-> Unbuildable + safety mute。
    let unbuildable_normal = build_schedule(&PlanProfile {
        l_g: 4097,
        l_p: 12_288,
        max_taps: 16_384,
        extended_taps_admitted: true,
        fusion: FusionForm::FusedGp,
        ..profile_default()
    })
    .expect("fused 16384 normal plan");
    assert_eq!(route_support(&unbuildable_normal.profile).unwrap(), 16_384);
    let admission = evaluate_fallback(&unbuildable_normal, &budgets);
    assert!(!admission.admitted);
    assert!(admission.safety_mute_required);
    assert!(admission.plan.is_none());
    assert_eq!(admission.reason, FallbackReason::Unbuildable);
}

#[test]
fn schedule_plan_runtime_admission_end_to_end() {
    let profile = PlanProfile {
        crossfade: true,
        ..profile_default()
    };
    let budgets = RuntimeBudgets::baseline(&profile);
    let admission = admit_runtime(&profile, &budgets).expect("runtime admission");
    assert!(admission.admitted);
    assert_eq!(admission.status, RtStatus::OK);
    assert!(admission.normal.runtime_admitted);
    assert!(admission.deadline.admitted);
    assert!(admission.coeff_ok);
    assert!(admission.ram_ok);
    assert_eq!(
        admission.hot_swap.disposition,
        HotSwapDisposition::Crossfade
    );
    assert!(admission.hot_swap.admitted);
    let fallback = admission.fallback.plan.as_ref().expect("fallback plan");
    assert!(admission.fallback.admitted);
    assert!(fallback.plan_bytes > 0);
    assert_eq!(admission.normal.ack_mask, 0xF);

    // 系数预算不足 -> MEMORY，且 plan 不标记 runtime_admitted。
    let tight_coeff = RuntimeBudgets {
        available_coeff_bytes: 1,
        ..budgets
    };
    let admission = admit_runtime(&profile, &tight_coeff).expect("runtime admission");
    assert!(!admission.admitted);
    assert_eq!(admission.status, RtStatus::ERR_ADMISSION_MEMORY);
    assert!(!admission.normal.runtime_admitted);

    // 图侧 RAM 不足 -> MEMORY。
    let tight_ram = RuntimeBudgets {
        available_ram_bytes: 1,
        ..budgets
    };
    let admission = admit_runtime(&profile, &tight_ram).expect("runtime admission");
    assert!(!admission.admitted);
    assert_eq!(admission.status, RtStatus::ERR_ADMISSION_MEMORY);

    // 算力预算不足 -> SCHED。
    let slow = RuntimeBudgets {
        deadline: DeadlineBudgets::with_core_rate(1_000_000_000),
        ..budgets
    };
    let admission = admit_runtime(&profile, &slow).expect("runtime admission");
    assert!(!admission.admitted);
    assert_eq!(admission.status, RtStatus::ERR_ADMISSION_SCHED);

    // 无 crossfade 声明 -> NotRequested（不是错误，只允许硬切）。
    let admission = admit_runtime(
        &profile_default(),
        &RuntimeBudgets::baseline(&profile_default()),
    )
    .expect("runtime admission");
    assert_eq!(
        admission.hot_swap.disposition,
        HotSwapDisposition::NotRequested
    );

    // 非法 deadline 预算 -> Err（配置非法，不是预算不足）。
    let bad = RuntimeBudgets {
        deadline: DeadlineBudgets {
            core_macs_per_second: 0,
            gate_num: 1,
            gate_den: 2,
        },
        ..RuntimeBudgets::baseline(&profile)
    };
    assert_eq!(
        admit_runtime(&profile, &bad).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );
}

#[test]
fn schedule_plan_determinism_and_summary() {
    let profile = profile_default();
    let a = build_schedule(&profile).expect("plan a");
    let b = build_schedule(&profile).expect("plan b");
    assert_eq!(a, b, "same profile must produce identical static plan");
    assert_eq!(profile, profile_default(), "build must not mutate profile");

    let lines = a.summary_lines();
    assert_eq!(lines.len(), 7);
    let text = lines.join("\n");
    assert!(text.contains("class=normal kernel=direct fusion=factorized"));
    assert!(text.contains("taps: class=regular support=4096"));
    assert!(text.contains("ack_mask=0xF"));
    assert!(text.contains("tasks=43"));
    assert!(text.contains("matrix=102400"));
    println!("=== T56 schedule_plan evidence sample ===");
    for line in lines.iter() {
        println!("{line}");
    }

    // 不同 kernel 的摘要可区分。
    let hybrid = build_schedule(&PlanProfile {
        kernel: KernelForm::Hybrid,
        ..profile_default()
    })
    .expect("hybrid plan");
    assert!(hybrid
        .summary_lines()
        .join("\n")
        .contains("kernel=hybrid fusion=factorized"));
    let shared = build_schedule(&profile_shared_4096_b128()).expect("shared plan");
    assert!(shared
        .summary_lines()
        .join("\n")
        .contains("kernel=shared_fft fusion=fused_gp"));
}

#[test]
fn schedule_plan_stage_coverage_all_kernels() {
    let direct = build_schedule(&profile_default()).expect("direct plan");
    let direct_expected = [
        (StageKind::Pre, true),
        (StageKind::InputFirA, true),
        (StageKind::InputFft, false),
        (StageKind::MatrixG, true),
        (StageKind::FftBinMac, false),
        (StageKind::Reduce, true),
        (StageKind::DiagFirP, true),
        (StageKind::Ifft, false),
        (StageKind::Protect, true),
        (StageKind::Scatter, true),
    ];
    for &(kind, expected) in direct_expected.iter() {
        assert_eq!(
            direct.stage_work[kind.index()] > 0,
            expected,
            "direct stage {}",
            kind.as_str()
        );
    }

    let shared = build_schedule(&profile_shared_4096_b128()).expect("shared plan");
    let shared_expected = [
        (StageKind::Pre, true),
        (StageKind::InputFirA, false),
        (StageKind::InputFft, true),
        (StageKind::MatrixG, false),
        (StageKind::FftBinMac, true),
        (StageKind::Reduce, true),
        (StageKind::DiagFirP, false),
        (StageKind::Ifft, true),
        (StageKind::Protect, true),
        (StageKind::Scatter, true),
    ];
    for &(kind, expected) in shared_expected.iter() {
        assert_eq!(
            shared.stage_work[kind.index()] > 0,
            expected,
            "shared FFT stage {}",
            kind.as_str()
        );
    }
    assert_eq!(shared.fft_transforms, 10);

    let hybrid = build_schedule(&profile_hybrid_8192()).expect("hybrid plan");
    let hybrid_expected = [
        (StageKind::Pre, true),
        (StageKind::InputFirA, true),
        (StageKind::InputFft, true),
        (StageKind::MatrixG, false),
        (StageKind::FftBinMac, true),
        (StageKind::Reduce, true),
        (StageKind::DiagFirP, true),
        (StageKind::Ifft, true),
        (StageKind::Protect, true),
        (StageKind::Scatter, true),
    ];
    for &(kind, expected) in hybrid_expected.iter() {
        assert_eq!(
            hybrid.stage_work[kind.index()] > 0,
            expected,
            "hybrid stage {}",
            kind.as_str()
        );
    }
    assert_eq!(hybrid.fft_transforms, 14);

    // 所有 stage 下标唯一且 phase 归属正确。
    let mut seen = [false; SCHEDULE_STAGE_KINDS];
    for kind in StageKind::ALL.iter() {
        let slot = kind.index();
        assert!(!seen[slot]);
        seen[slot] = true;
        assert_eq!(kind.phase_of(), kind.phase_of());
        assert!(kind.phase_of() < SCHEDULE_PHASES as u8);
    }
    assert!(seen.iter().all(|value| *value));
}

#[test]
fn schedule_plan_t09_dag_parity() {
    // T09 direct 静态 DAG：B1/B2/B3 各 n_role 个 tile（B2 每输入列一个）。
    let mut topo = DirectTopology::identity();
    topo.n_src = 7;
    topo.n_role = 7;
    topo.container_channels = 8;
    topo.d_samples = 0;
    topo.l_a = 1;
    topo.l_g = 2048;
    topo.l_p = 1024;
    topo.m = 5;
    let mut src_of_role = [DIRECT_NO_SOURCE; 22];
    let mut role = 0usize;
    while role < 7 {
        src_of_role[role] = role as u16;
        role += 1;
    }
    topo.src_of_role = src_of_role;
    let mut participants = [DIRECT_NO_SOURCE; 8];
    participants[0] = 0;
    participants[1] = 1;
    participants[2] = 2;
    participants[3] = 3;
    participants[4] = 6;
    topo.participants = participants;
    let mut scatter = [0u16; 22];
    scatter[0] = 0;
    scatter[1] = 1;
    scatter[2] = 2;
    scatter[3] = 3;
    scatter[4] = 4;
    scatter[5] = 5;
    scatter[6] = 7;
    topo.scatter = scatter;
    let wd = WorkerDagPlan::build(&topo, 128, 4, 1).expect("T09 WorkerDagPlan build");
    assert_eq!(wd.phase_task_count(0), 7);
    assert_eq!(wd.phase_task_count(1), 7);
    assert_eq!(wd.phase_task_count(2), 7);
    assert_eq!(wd.total_tasks(), 21);

    // T56 全图 DAG 与 T09 的 path tile 粒度对齐，同时增加 FFT/reduce/P/protect/scatter 段。
    let plan = build_schedule(&PlanProfile {
        l_g: 2048,
        l_p: 1024,
        ..profile_default()
    })
    .expect("T56 plan for T09 parity");
    assert_eq!(plan.matrix_tiles, wd.phase_task_count(1) as u16);
    assert_eq!(plan.phase_task_count[0], 14);
    assert_eq!(plan.phase_task_count[1], wd.phase_task_count(1) as u16);
    assert_eq!(
        plan.worker_task_count[2].iter().sum::<u16>(),
        22,
        "B3 must include reduce + diag P + protect + scatter"
    );
    assert!(plan.task_count > wd.total_tasks());
    assert_eq!(plan.ack_mask, ack_mask(4));
}

#[test]
fn schedule_plan_memory_model_formulas() {
    let plan = build_schedule(&profile_default()).expect("direct plan");
    // 系数：A dense + 25 条 4096 + 7 条 1024 f32。
    assert_eq!(plan.coeff_bytes, (7 * 1 + 25 * 4096 + 7 * 1024) * 4);
    // 表：header/path/diag/role，各自 64B 对齐。
    let expected_tables = align64(128)
        + align64(25 * core::mem::size_of::<TspiPathV1>() as u64)
        + align64(7 * core::mem::size_of::<TspiDiagFirV1>() as u64)
        + align64(7 * core::mem::size_of::<TspiRoleV1>() as u64);
    assert_eq!(plan.table_bytes, expected_tables);
    // 历史：A 历史 0 + 7*(4095+1023) samples x f32。
    assert_eq!(plan.history_bytes, 7 * (4095 + 1023) * 4);
    // scratch：4 worker x B x O x f32（direct 无 FFT scratch）。
    assert_eq!(plan.scratch_bytes, 4 * 128 * 7 * 4);
    assert_eq!(
        plan.plan_bytes,
        align64(plan.coeff_bytes)
            + align64(plan.table_bytes)
            + align64(plan.history_bytes)
            + align64(plan.scratch_bytes)
    );
    assert_eq!(plan.dual_graph_bytes, 2 * plan.plan_bytes);

    // FFT 形态（T08 PartitionedLayout 口径）：谱系数 + x_prev/x_hist/work/slot 状态。
    let shared = build_schedule(&profile_shared_8192_b64()).expect("shared plan");
    let k = shared.fft_size as u64;
    let q = shared.partitions as u64;
    assert_eq!(k, 128);
    assert_eq!(q, 128);
    assert_eq!(shared.coeff_bytes, (k + 25 * q * k * 2) * 4);
    let state_samples = 5 * 64 + 5 * q * k * 2 + 4 * k + 1;
    assert_eq!(shared.history_bytes, state_samples * 4);
    let expected_scratch = 4 * 64 * 5 * 4 + 4 * 4 * k * 4;
    assert_eq!(shared.scratch_bytes, expected_scratch);
}

#[test]
fn schedule_plan_capacity_guards_and_kernel_selection() {
    // 常量与公式绑定。
    assert_eq!(SCHEDULE_PHASES, 3);
    assert_eq!(SCHEDULE_WORKERS_MAX, 4);
    assert_eq!(SCHEDULE_WORKERS_FALLBACK, 2);
    assert_eq!(fft_size_for(&profile_default()), 256);
    assert_eq!(fft_transform_flops(256), 5 * 256 * 8 / 2);
    assert_eq!(fft_transform_macs(256), 2_560);
    assert_eq!(route_support(&profile_default()).unwrap(), 4096);
    assert_eq!(fused_len(&profile_default()).unwrap(), 5119);
    assert_eq!(full_path_len(&profile_default()).unwrap(), 5119);
    assert_eq!(
        taps_profile_of(&profile_default()).unwrap(),
        TapsProfile::Regular
    );

    // 推荐 kernel：常规 4096 -> Direct；8192/16384 factorized -> Hybrid；
    // hybrid 支撑也不可准入时才建议 SharedFft。
    assert_eq!(recommended_kernel(&profile_default()), KernelForm::Direct);
    assert_eq!(
        recommended_kernel(&profile_direct_8192()),
        KernelForm::Hybrid
    );
    assert_eq!(
        recommended_kernel(&profile_hybrid_16384()),
        KernelForm::Hybrid
    );
    let awkward = PlanProfile {
        l_p: 10_000,
        max_taps: 16_384,
        extended_taps_admitted: true,
        ..profile_default()
    };
    assert_eq!(recommended_kernel(&awkward), KernelForm::SharedFft);

    // 最大布局（N=20/O=16/C=22/M=8/paths=128/diag=16）任务数仍在上界内。
    let max_layout = PlanProfile {
        n_src: 20,
        n_role: 16,
        container_channels: 22,
        m: 8,
        l_g: 64,
        l_p: 32,
        matrix_paths: 128,
        diag_firs: 16,
        ..profile_default()
    };
    let plan = build_schedule(&max_layout).expect("max layout build");
    assert_eq!(plan.task_count, 101);
    assert!(plan.task_count as usize <= SCHEDULE_MAX_TASKS);
    assert_eq!(plan.phase_task_count, [36, 16, 49]);
    let deadline = admit_deadline(&plan, &DeadlineBudgets::default());
    assert!(deadline.admitted);

    // 非法配置均返回冻结错误码，不 panic。
    let invalid = PlanProfile {
        n_role: 0,
        ..profile_default()
    };
    assert_eq!(build_schedule(&invalid).unwrap_err(), RtStatus::ERR_ROLES);
    let invalid = PlanProfile {
        container_channels: 23,
        ..profile_default()
    };
    assert_eq!(
        build_schedule(&invalid).unwrap_err(),
        RtStatus::ERR_CHANNELS
    );
    let invalid = PlanProfile {
        workers: 0,
        ..profile_default()
    };
    assert_eq!(
        build_schedule(&invalid).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );
    let invalid = PlanProfile {
        max_taps: 0,
        ..profile_default()
    };
    assert_eq!(build_schedule(&invalid).unwrap_err(), RtStatus::ERR_TAPS);
    let invalid = PlanProfile {
        l_p: 0,
        ..profile_default()
    };
    assert_eq!(build_schedule(&invalid).unwrap_err(), RtStatus::ERR_TAPS);
    // 96k 采样率合法，但 gate 按自身 T_block 缩放（96k/128 = 666,666 ns）。
    let k96 = build_schedule(&PlanProfile {
        sample_rate_hz: 96_000,
        ..profile_default()
    })
    .expect("96k plan");
    let deadline = admit_deadline(&k96, &DeadlineBudgets::default());
    assert_eq!(deadline.gate_ns, 666_666);
    assert!(
        !deadline.admitted,
        "same app work at 96k must exceed the halved gate"
    );
}

#[test]
fn schedule_plan_serial_fraction_and_balance() {
    // 2 角色小图：B2 只有 2 tile（4 worker 下如实 2 并行，不 spin 凑忙）。
    let small = PlanProfile {
        n_src: 2,
        n_role: 2,
        container_channels: 3,
        m: 2,
        l_g: 64,
        l_p: 32,
        matrix_paths: 2,
        diag_firs: 2,
        ..profile_default()
    };
    let plan = build_schedule(&small).expect("small plan");
    assert_eq!(plan.phase_task_count, [4, 2, 7]);
    assert_eq!(plan.parallel_workers, [4, 2, 4]);
    assert_eq!(plan.serial_fraction_q16[0], 0);
    assert_eq!(plan.serial_fraction_q16[1], 0);
    // B3 7 tile / 4 worker -> (7-4)*65536/7。
    assert_eq!(plan.serial_fraction_q16[2], 28_086);
    // worker 2/3 只在 B1 有任务；B2 只有 worker 0/1。
    assert!(plan.worker_task_count[0][2] > 0);
    assert!(plan.worker_task_count[0][3] > 0);
    assert_eq!(plan.worker_task_count[1][2], 0);
    assert_eq!(plan.worker_task_count[1][3], 0);
    assert_eq!(
        plan.worker_task_count
            .iter()
            .map(|p| p.iter().sum::<u16>())
            .sum::<u16>(),
        plan.task_count
    );

    // 常规图：4 worker 均有有效任务且没有空转凑 100%（每 worker 的负载由固定 tile 决定）。
    let plan = build_schedule(&profile_default()).expect("default plan");
    let mut worker = 0usize;
    while worker < 4 {
        assert!(
            plan.worker_task_count[0][worker]
                + plan.worker_task_count[1][worker]
                + plan.worker_task_count[2][worker]
                > 0
        );
        worker += 1;
    }
    // 矩阵阶段工作量与路径数线性一致（tile 分块细节由 owner/负载自洽覆盖）。
    assert_eq!(
        plan.stage_work[StageKind::MatrixG.index()],
        plan.matrix_paths as u64 * 4096 * 128
    );
}
