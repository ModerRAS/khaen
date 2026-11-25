// T45（scheduler_fake）离线模型测试：实测拓扑解析 / 逐 worker DSP_CPUS 亲和 / FIFO 排序 /
// IRQ 依赖 / 准入参数化计划 / 2 worker 独立 profile / 失核整块拒收 / 不写内核。
//
// 注册方式：crates/rt-host/src/scheduler.rs 末尾 #[cfg(test)] mod scheduler_fake_tests
// { include!("../tests/T45/scheduler_fake.rs"); }（cargo 不自动发现 tests/<子目录>；
// 公共注册属 integrator，勿写入 tests/public_registration.rs）。
// 运行：cargo test -p tspi-rt-host scheduler_fake
//
// 夹具说明：SYNTHETIC(合成) 板卡快照，与 WBS §7 / S0-02/S0-03/S0-05 被动命令输出同构：
// 8 CPU = DSP_CPUS 0..3（isolcpus=0-3 只作为“计划”记录，未执行）+ 控制/网络/PTP/IRQ 在 4..7。
// 真实板端采集 NOT_RUN（见 evidence/T45/MANIFEST.md §8），本文件不打开任何设备/不写内核。

use super::*;

// ---------------------------------------------------------------------------
// 夹具：实测拓扑（合成快照，格式同 S0-03/S0-05 输出）
// ---------------------------------------------------------------------------

const TICK_FREQ_HZ: u32 = 24_000_000;
const SPIN_MAX: u32 = 1_000_000;
const CUTOFF_DRAIN: u32 = 4096;

fn core_map_unique_8() -> [i8; MAX_CPU] {
    let mut cores = [CPU_UNKNOWN_CORE; MAX_CPU];
    let mut cpu = 0usize;
    while cpu < 8 {
        cores[cpu] = cpu as i8;
        cpu += 1;
    }
    cores
}

/// 8 CPU 快照：online/possible=0xFF、isolated=0x0F、allowed=0xFF、core 唯一。
fn topology_8() -> TopologySnapshot {
    let mut topo = TopologySnapshot::new()
        .with_online_mask(0xFF)
        .with_possible_mask(0xFF)
        .with_isolated_mask(0x0F)
        .with_nohz_full_mask(0x0F)
        .with_allowed_mask(0xFF)
        .with_core_map(core_map_unique_8());
    // 实测 IRQ 亲和（/proc/irq/N/smp_affinity_list）：USB capture→CPU6、PTP→CPU7、ALSA→CPU6。
    topo.set_irq_cpu(0, 6).unwrap();
    topo.set_irq_cpu(2, 7).unwrap();
    topo.set_irq_cpu(4, 6).unwrap();
    topo
}

fn request_4worker() -> SchedRequest {
    SchedRequest::new()
        .with_dsp_cpu(0, 0)
        .with_dsp_cpu(1, 1)
        .with_dsp_cpu(2, 2)
        .with_dsp_cpu(3, 3)
        .with_irq_dependency(0, IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER)
        .with_tick(TICK_FREQ_HZ, 2)
        .with_calibration(SPIN_MAX, CUTOFF_DRAIN)
        .with_plan_version(7)
}

fn request_2worker() -> SchedRequest {
    request_4worker()
        .with_workers(2)
        .with_dsp_cpu(2, CPU_UNUSED)
        .with_dsp_cpu(3, CPU_UNUSED)
        .with_irq_dependency(2, IRQ_DEPENDENCY_NONE)
        .with_irq_dependency(3, IRQ_DEPENDENCY_NONE)
}

fn plan_4worker() -> SchedPlan {
    plan(&request_4worker(), &topology_8()).unwrap()
}

fn plan_2worker() -> SchedPlan {
    plan(&request_2worker(), &topology_8()).unwrap()
}

/// 与 S0-03 的 lscpu -e=CPU,CORE,SOCKET,ONLINE,MAXMHZ,MINMHZ 同构（合成值）。
const LSCPU_8: &str = "CPU CORE SOCKET ONLINE MAXMHZ    MINMHZ
0   0    0      yes    2016.0000 800.0000
1   1    0      yes    2016.0000 800.0000
2   2    0      yes    2016.0000 800.0000
3   3    0      yes    2016.0000 800.0000
4   4    0      yes    1416.0000 800.0000
5   5    0      yes    1416.0000 800.0000
6   6    0      yes    1416.0000 800.0000
7   7    0      yes    1416.0000 800.0000";

const CMDLINE_8: &str =
    "console=ttyS0,115200 root=/dev/mmcblk0p2 rw isolcpus=0-3 nohz_full=0-3 rcu_nocbs=0-3";

// ---------------------------------------------------------------------------
// A. 布局 / ABI / 常量
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_layout_affinity_entry_offsets() {
    // core-v1 §11.13 TspiWorkerAffinityV1：32 B / align 8（align 例外）。
    assert_eq!(size_of::<TspiWorkerAffinityV1>(), 32);
    assert_eq!(align_of::<TspiWorkerAffinityV1>(), 8);
    assert_eq!(core::mem::offset_of!(TspiWorkerAffinityV1, worker_id), 0);
    assert_eq!(
        core::mem::offset_of!(TspiWorkerAffinityV1, sched_priority),
        1
    );
    assert_eq!(core::mem::offset_of!(TspiWorkerAffinityV1, flags), 2);
    assert_eq!(core::mem::offset_of!(TspiWorkerAffinityV1, cpu_index), 4);
    assert_eq!(core::mem::offset_of!(TspiWorkerAffinityV1, reserved0), 6);
    assert_eq!(
        core::mem::offset_of!(TspiWorkerAffinityV1, irq_dependency_mask),
        8
    );
    assert_eq!(core::mem::offset_of!(TspiWorkerAffinityV1, reserved1), 16);
    assert_eq!(core::mem::offset_of!(TspiWorkerAffinityV1, reserved2), 24);
}

#[test]
fn scheduler_fake_layout_no_drop_and_capacity() {
    assert!(!needs_drop::<TspiWorkerAffinityV1>());
    assert!(!needs_drop::<TopologySnapshot>());
    assert!(!needs_drop::<LscpuSnapshot>());
    assert!(!needs_drop::<SchedRequest>());
    assert!(!needs_drop::<SchedPlan>());
    assert!(!needs_drop::<SchedGate>());
    assert!(!needs_drop::<SchedRejectReason>());
    assert!(!needs_drop::<SchedAdmissionReport>());
    assert_eq!(AFFINITY_TABLE_MAX_BYTES, 128);
    assert_eq!(WORKER_AFFINITY_COUNT, 4);
    assert_eq!(DSP_CPU_MAX, RT_WORKERS as usize); // 4（T09 对齐）
}

#[test]
fn scheduler_fake_constants_initial_values() {
    assert_eq!(DSP_PRIORITY_INITIAL, 70);
    assert_eq!(IO_PRIORITY_INITIAL, 80);
    assert_eq!(IRQ_PRIORITY_INITIAL, 90);
    assert_eq!(FIFO_PRIORITY_MIN, 1);
    assert_eq!(FIFO_PRIORITY_MAX, 99);
    assert_eq!(TICK_READ_BUDGET_MAX, 3); // core-v1 §3.4（D-05，不消费 core::rt 的 2）
    assert_eq!(IRQ_BIT_KNOWN_MASK, 0x1F);
    assert_eq!(IRQ_BIT_USB_CAPTURE, 1 << 0);
    assert_eq!(IRQ_BIT_USB_PLAYBACK, 1 << 1);
    assert_eq!(IRQ_BIT_PTP_TIMER, 1 << 2);
    assert_eq!(IRQ_BIT_ETH_AOIP, 1 << 3);
    assert_eq!(IRQ_BIT_ALSA_PERIOD, 1 << 4);
    assert_eq!(IRQ_CLASS_COUNT, 8);
    assert!(!KERNEL_WRITES_PERFORMED);
}

#[test]
fn scheduler_fake_affinity_entry_new_and_predicates() {
    let entry = TspiWorkerAffinityV1::new(2, 70, 0xFFFF, 5, IRQ_BIT_USB_CAPTURE);
    assert_eq!(entry.worker_id, 2);
    assert_eq!(entry.sched_priority, 70);
    assert_eq!(entry.flags, AFFINITY_FLAG_KNOWN_MASK); // 未知位被掩掉
    assert_eq!(entry.cpu_index, 5);
    assert_eq!(entry.irq_dependency_mask, IRQ_BIT_USB_CAPTURE);
    assert_eq!(entry.reserved0, 0);
    assert_eq!(entry.reserved1, 0);
    assert_eq!(entry.reserved2, 0);
    assert!(entry.is_rt());
    assert!(entry.is_isolated());
    assert!(entry.validate().is_ok());

    let plain = TspiWorkerAffinityV1::new(0, 70, AFFINITY_FLAG_RT, 0, 0);
    assert!(plain.is_rt());
    assert!(!plain.is_isolated());
    assert!(plain.validate().is_ok());
}

#[test]
fn scheduler_fake_affinity_entry_validate_negative_matrix() {
    assert_eq!(
        TspiWorkerAffinityV1::new(0, 0, AFFINITY_FLAG_RT, 0, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        TspiWorkerAffinityV1::new(0, 100, AFFINITY_FLAG_RT, 0, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        TspiWorkerAffinityV1::new(0, 70, 0x8000, 0, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        TspiWorkerAffinityV1::new(0, 70, 0, 0, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED) // flags bit0=rt 必须置位
    );
    assert_eq!(
        TspiWorkerAffinityV1::new(0, 70, AFFINITY_FLAG_RT, CPU_UNUSED, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    let mut reserved = TspiWorkerAffinityV1::new(0, 70, AFFINITY_FLAG_RT, 0, 0);
    reserved.reserved1 = 1;
    assert_eq!(reserved.validate(), Err(RtStatus::ERR_BAD_LAYOUT));
    let unknown_irq = TspiWorkerAffinityV1::new(0, 70, AFFINITY_FLAG_RT, 0, 1 << 40);
    assert_eq!(unknown_irq.validate(), Err(RtStatus::ERR_ADMISSION_SCHED));
}

#[test]
fn scheduler_fake_affinity_entry_to_bytes_layout() {
    let entry = TspiWorkerAffinityV1::new(3, 70, AFFINITY_FLAG_RT, 0x0102, IRQ_BIT_PTP_TIMER);
    let bytes = entry.to_bytes();
    assert_eq!(bytes.len(), 32);
    assert_eq!(bytes[0], 3);
    assert_eq!(bytes[1], 70);
    assert_eq!(bytes[2], 1); // flags LE
    assert_eq!(bytes[3], 0);
    assert_eq!(bytes[4], 0x02); // cpu LE
    assert_eq!(bytes[5], 0x01);
    assert_eq!(&bytes[6..8], &[0, 0]);
    assert_eq!(&bytes[8..16], &(IRQ_BIT_PTP_TIMER as u64).to_le_bytes());
    assert_eq!(&bytes[16..32], &[0u8; 16]);
}

#[test]
fn scheduler_fake_affinity_entry_zero_encodes_all_zero() {
    assert_eq!(TspiWorkerAffinityV1::ZERO.to_bytes(), [0u8; 32]);
    assert_eq!(
        TspiWorkerAffinityV1::ZERO,
        TspiWorkerAffinityV1::new(0, 0, 0, 0, 0)
    );
}

// ---------------------------------------------------------------------------
// B. 实测文本解析（/sys CPU 列表 / cmdline / lscpu -e / IRQ 亲和）
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_parse_cpu_list_basic() {
    assert_eq!(parse_cpu_list("0-3"), Ok(0x0F));
    assert_eq!(parse_cpu_list(" 0-3 "), Ok(0x0F));
    assert_eq!(parse_cpu_list("0-3,6,7"), Ok(0xCF));
    assert_eq!(parse_cpu_list("4-5"), Ok(0x30));
    assert_eq!(parse_cpu_list("0"), Ok(1));
    assert_eq!(parse_cpu_list("0-0"), Ok(1));
    assert_eq!(parse_cpu_list(""), Ok(0));
    assert_eq!(parse_cpu_list("   "), Ok(0));
}

#[test]
fn scheduler_fake_parse_cpu_list_rejects_malformed() {
    for bad in [
        "0-", "-1", "3-1", "1,,2", "1,", ",1", "a", "0-a", "64", "0-64", "1-2-3", "1;2",
    ] {
        assert_eq!(
            parse_cpu_list(bad),
            Err(RtStatus::ERR_ADMISSION_SCHED),
            "bad input must be rejected: {}",
            bad
        );
    }
}

#[test]
fn scheduler_fake_parse_cmdline_isolcpus_variants() {
    assert_eq!(parse_cmdline_isolcpus(CMDLINE_8), Ok(0x0F));
    assert_eq!(parse_cmdline_isolcpus("isolcpus=2-3 quiet"), Ok(0x0C));
    assert_eq!(
        parse_cmdline_isolcpus("console=ttyS0 isolcpus=domain,2-3 nohz_full=4"),
        Ok(0x0C)
    );
    assert_eq!(parse_cmdline_isolcpus("console=ttyS0"), Ok(0));
    assert_eq!(
        parse_cmdline_isolcpus("isolcpus=0,1 isolcpus=2"),
        Ok(0x07) // 多次出现取并集
    );
    assert_eq!(
        parse_cmdline_isolcpus("isolcpus=2-"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_parse_cmdline_nohz_full_and_all_sentinel() {
    assert_eq!(parse_cmdline_nohz_full(CMDLINE_8), Ok(0x0F));
    assert_eq!(parse_cmdline_nohz_full("nohz_full=4-5"), Ok(0x30));
    assert_eq!(parse_cmdline_nohz_full("nohz_full=all"), Ok(0)); // 非列表值忽略
    assert_eq!(parse_cmdline_nohz_full(""), Ok(0));
    assert_eq!(
        parse_cmdline_nohz_full("nohz_full=4-5-6"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_parse_lscpu_e_typical_snapshot() {
    let table = parse_lscpu_e(LSCPU_8).unwrap();
    assert_eq!(table.cpu_count, 8);
    assert_eq!(table.online_mask, 0xFF);
    assert_eq!(table.possible_mask, 0xFF);
    assert_eq!(table.core_of_cpu[0], 0);
    assert_eq!(table.core_of_cpu[3], 3);
    assert_eq!(table.core_of_cpu[7], 7);
}

#[test]
fn scheduler_fake_parse_lscpu_e_tolerates_blank_tail_and_offline() {
    // MAXMHZ/MINMHZ 空（切分后列数变少）不影响前四列；ONLINE=no 的行只出现在 possible。
    let text = "CPU CORE SOCKET ONLINE MAXMHZ MINMHZ
0   0    0      yes
1   1    0      no
2   2    0      1
3   3    0      0";
    let table = parse_lscpu_e(text).unwrap();
    assert_eq!(table.cpu_count, 4);
    assert_eq!(table.online_mask, 0b0101);
    assert_eq!(table.possible_mask, 0b1111);
}

#[test]
fn scheduler_fake_parse_lscpu_e_sparse_cpu_ids() {
    let text = "CPU CORE ONLINE
0 0 yes
2 2 yes
7 7 yes";
    let table = parse_lscpu_e(text).unwrap();
    assert_eq!(table.possible_mask, (1 << 0) | (1 << 2) | (1 << 7));
    assert_eq!(table.online_mask, table.possible_mask);
    assert_eq!(table.core_of_cpu[7], 7);
    assert_eq!(table.core_of_cpu[1], CPU_UNKNOWN_CORE);
}

#[test]
fn scheduler_fake_parse_lscpu_e_rejects_malformed() {
    // 缺 ONLINE 列。
    assert_eq!(
        parse_lscpu_e("CPU CORE SOCKET\n0 0 0"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 重复 CPU。
    assert_eq!(
        parse_lscpu_e("CPU CORE ONLINE\n0 0 yes\n0 0 yes"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 非法 ONLINE 值。
    assert_eq!(
        parse_lscpu_e("CPU CORE ONLINE\n0 0 maybe"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 空文本。
    assert_eq!(parse_lscpu_e(""), Err(RtStatus::ERR_ADMISSION_SCHED));
    // 数据行列不足。
    assert_eq!(
        parse_lscpu_e("CPU CORE SOCKET ONLINE\n0 0"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_parse_irq_affinity_list() {
    assert_eq!(parse_irq_affinity_list("6"), Ok(1 << 6));
    assert_eq!(parse_irq_affinity_list("6-7"), Ok(0xC0));
    assert_eq!(first_cpu_in_mask(0xC0), Some(6));
    assert_eq!(first_cpu_in_mask(0), None);
    assert_eq!(
        parse_irq_affinity_list("not-a-list"),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_topology_from_measured_end_to_end() {
    let topo = TopologySnapshot::from_measured(LSCPU_8, "0-7", "0-7", "0-3", CMDLINE_8).unwrap();
    assert_eq!(topo.online_mask, 0xFF);
    assert_eq!(topo.possible_mask, 0xFF);
    assert_eq!(topo.allowed_mask, 0xFF);
    assert_eq!(topo.isolated_mask, 0x0F);
    assert_eq!(topo.nohz_full_mask, 0x0F);
    assert_eq!(topo.core_of_cpu[5], 5);
    assert!(topo.validate().is_ok());
    assert!(topo.irq_cpu_for_bit(IRQ_BIT_USB_CAPTURE).is_none()); // IRQ 需另注入，不猜
}

#[test]
fn scheduler_fake_topology_from_measured_refuses_missing_allowed() {
    let err = TopologySnapshot::from_measured(LSCPU_8, "0-7", "", "0-3", CMDLINE_8);
    assert_eq!(err, Err(RtStatus::ERR_ADMISSION_SCHED));
}

#[test]
fn scheduler_fake_topology_from_measured_rejects_inconsistent_sets() {
    // lscpu 只报 4 CPU（possible=0x0F）而 online 报 0-7 → online 不在 possible 内，拒绝。
    let lscpu_4 = "CPU CORE ONLINE
0 0 yes
1 1 yes
2 2 yes
3 3 yes";
    assert_eq!(
        TopologySnapshot::from_measured(lscpu_4, "0-7", "0-7", "0", ""),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // allowed 落在离线 CPU（allowed 含 CPU 8，online 只到 7）→ 拒绝。
    assert_eq!(
        TopologySnapshot::from_measured(LSCPU_8, "0-7", "0-8", "0-3", ""),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

// ---------------------------------------------------------------------------
// C. TopologySnapshot 查询/注入
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_topology_masks_and_cores() {
    let topo = topology_8();
    assert!(topo.cpu_online(7));
    assert!(!topo.cpu_online(8));
    assert!(!topo.cpu_online(CPU_UNUSED));
    assert!(topo.cpu_isolated(3));
    assert!(!topo.cpu_isolated(4));
    assert!(topo.cpu_allowed(0));
    assert_eq!(topo.cpu_core(2), 2);
    assert_eq!(topo.validate(), Ok(()));
    assert_eq!(TopologySnapshot::default(), TopologySnapshot::new());
}

#[test]
fn scheduler_fake_topology_irq_injection_and_evidence() {
    let mut topo = topology_8();
    assert_eq!(topo.irq_cpu_for_bit(IRQ_BIT_USB_CAPTURE), Some(6));
    assert_eq!(topo.irq_cpu_for_bit(IRQ_BIT_PTP_TIMER), Some(7));
    assert_eq!(topo.irq_cpu_for_bit(IRQ_BIT_USB_PLAYBACK), None);
    assert!(topo.has_irq_evidence(IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER));
    assert!(!topo.has_irq_evidence(IRQ_BIT_USB_PLAYBACK));
    assert!(!topo.has_irq_evidence(1 << 40)); // 未知位 = 无证据
    assert_eq!(
        topo.set_irq_cpu(IRQ_CLASS_COUNT, 0),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        topo.set_irq_cpu(1, CPU_UNUSED),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        topo.set_irq_affinity_mask(1, 0),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    topo.set_irq_affinity_mask(1, 0b11 << 4).unwrap();
    assert_eq!(topo.irq_cpu_for_bit(IRQ_BIT_USB_PLAYBACK), Some(4));
}

#[test]
fn scheduler_fake_topology_validate_negative_matrix() {
    let mut missing_online = topology_8();
    missing_online.online_mask = 0;
    assert_eq!(
        missing_online.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut missing_allowed = topology_8();
    missing_allowed.allowed_mask = 0;
    assert_eq!(
        missing_allowed.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut online_not_possible = topology_8();
    online_not_possible.possible_mask = 0x0F;
    assert_eq!(
        online_not_possible.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut isolated_offline = topology_8();
    isolated_offline.isolated_mask = 0xFF;
    isolated_offline.online_mask = 0x0F;
    assert_eq!(
        isolated_offline.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut unknown_irq_bits = topology_8();
    unknown_irq_bits.irq_present_mask = 1 << 40;
    assert_eq!(
        unknown_irq_bits.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

// ---------------------------------------------------------------------------
// D. SchedRequest（profile/优先级/标定/CPU 槽/IRQ 位）
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_request_defaults_and_t09_alignment() {
    let req = SchedRequest::new();
    assert_eq!(req.workers, RT_WORKERS);
    assert_eq!(req.dsp_priority, DSP_PRIORITY_INITIAL);
    assert_eq!(req.io_priority, IO_PRIORITY_INITIAL);
    assert_eq!(req.irq_priority, IRQ_PRIORITY_INITIAL);
    assert_eq!(req.dsp_cpus, [CPU_UNUSED; DSP_CPU_MAX]);
    assert_eq!(req.irq_dependency, [IRQ_DEPENDENCY_NONE; DSP_CPU_MAX]);
    assert_eq!(req.tick_freq_hz, 0); // 未标定
    assert_eq!(req.tick_read_budget, 1);
    // T09 默认值对齐（2 worker 独立 profile 也用同一 bounded spin/cutoff 族）。
    assert_eq!(
        req.spin_max_iters,
        tspi_dsp_core::workers::WORKER_DEFAULT_SPIN_MAX_ITERS
    );
    assert_eq!(
        req.cutoff_drain_iters,
        tspi_dsp_core::workers::WORKER_DEFAULT_CUTOFF_DRAIN_ITERS
    );
    assert_eq!(SchedRequest::default(), req);
}

#[test]
fn scheduler_fake_request_validate_ok_for_both_profiles() {
    assert_eq!(request_4worker().validate(), Ok(()));
    assert_eq!(request_2worker().validate(), Ok(()));
}

#[test]
fn scheduler_fake_request_rejects_bad_profile() {
    for workers in [0u8, 1, 3, 5, 255] {
        assert_eq!(
            request_4worker().with_workers(workers).validate(),
            Err(RtStatus::ERR_ADMISSION_SCHED),
            "workers={}",
            workers
        );
    }
}

#[test]
fn scheduler_fake_request_rejects_priority_order_violations() {
    // dsp 必须严格低于 io（I/O 供数）；io 不得高于 irq（IRQ 最高）。
    assert_eq!(
        request_4worker().with_priorities(80, 70, 90).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_priorities(80, 80, 90).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_priorities(70, 95, 90).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_priorities(0, 80, 90).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_priorities(70, 80, 100).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 实测校正允许任意具体值，但保持 dsp < io <= irq。
    assert_eq!(
        request_4worker().with_priorities(65, 75, 85).validate(),
        Ok(())
    );
    assert_eq!(
        request_4worker().with_priorities(70, 80, 80).validate(),
        Ok(())
    );
}

#[test]
fn scheduler_fake_request_rejects_uncalibrated_params() {
    assert_eq!(
        request_4worker().with_tick(0, 2).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_tick(TICK_FREQ_HZ, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_tick(TICK_FREQ_HZ, 4).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED) // > TICK_READ_BUDGET_MAX=3
    );
    assert_eq!(
        request_4worker()
            .with_calibration(0, CUTOFF_DRAIN)
            .validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        request_4worker().with_calibration(SPIN_MAX, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_request_rejects_bad_cpu_slots_and_irq_bits() {
    // 未配置 DSP CPU。
    assert_eq!(
        SchedRequest::new().with_tick(TICK_FREQ_HZ, 2).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 越界 CPU。
    assert_eq!(
        request_4worker().with_dsp_cpu(0, 64).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 重复 CPU。
    assert_eq!(
        request_4worker().with_dsp_cpu(1, 0).validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 未使用槽必须为空。
    let mut req = request_2worker();
    req.dsp_cpus[2] = 3;
    assert_eq!(req.validate(), Err(RtStatus::ERR_ADMISSION_SCHED));
    // 未使用槽的 IRQ 必须为空。
    let mut req = request_2worker();
    req.irq_dependency[3] = IRQ_BIT_USB_CAPTURE;
    assert_eq!(req.validate(), Err(RtStatus::ERR_ADMISSION_SCHED));
    // 未知 IRQ 位（绕过 builder 掩码直接写字段）。
    let mut req = request_4worker();
    req.irq_dependency[0] = 1 << 40;
    assert_eq!(req.validate(), Err(RtStatus::ERR_ADMISSION_SCHED));
}

// ---------------------------------------------------------------------------
// E. plan()：4 worker / 2 worker 独立 profile 正向
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_plan_4worker_happy_path() {
    let topo = topology_8();
    let built = plan_4worker();
    assert_eq!(built.workers, 4);
    assert_eq!(built.expected_workers(), 4);
    assert_eq!(built.ack_mask(), 0xF);
    assert_eq!(built.affinity_count(), 4);
    assert_eq!(built.affinity_table_len(), 128);
    assert!(!built.is_two_worker_profile());
    assert_eq!(built.plan_version, 7);
    assert_eq!(built.dsp_priority, 70);
    assert_eq!(built.io_priority, 80);
    assert_eq!(built.irq_priority, 90);
    assert_eq!(built.tick_freq_hz, TICK_FREQ_HZ);
    assert_eq!(built.tick_read_budget, 2);
    assert_eq!(built.spin_max_iters, SPIN_MAX);
    assert_eq!(built.cutoff_drain_iters, CUTOFF_DRAIN);
    for k in 0..4usize {
        let entry = built.affinity_entry(k).unwrap();
        assert_eq!(entry.worker_id, k as u8);
        assert_eq!(entry.cpu_index, k as u16);
        assert_eq!(entry.sched_priority, 70);
        assert_eq!(entry.flags, AFFINITY_FLAG_RT | AFFINITY_FLAG_ISOLATED_CPU);
        assert!(entry.is_rt());
        assert!(entry.is_isolated());
        assert_eq!(
            entry.irq_dependency_mask,
            if k == 0 {
                IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER
            } else {
                0
            }
        );
    }
    assert_eq!(built.affinity_entry(4), None);
    assert_eq!(built.worker_cpu(0), Some(0));
    assert_eq!(built.worker_cpu(4), None);
    assert_eq!(
        built.worker_irq_mask(0),
        Some(IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER)
    );
    assert_eq!(built.worker_irq_mask(3), Some(0));
    assert_eq!(
        built.union_irq_dependency(),
        IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER
    );
    assert!(built.cpu_in_plan(3));
    assert!(!built.cpu_in_plan(4));
    assert_eq!(built.validate(), Ok(()));
    assert_eq!(built.validate_against(&topo), Ok(()));
    assert!(built.cpu_set_ok(&topo));
    assert_eq!(built.report().kernel_writes_performed, false);
}

#[test]
fn scheduler_fake_plan_flags_isolated_only_when_measured() {
    let mut topo = topology_8();
    topo.isolated_mask = 0; // 未执行 isolcpus：仍可准入，但不得谎标 isolated
    assert_eq!(topo.validate(), Ok(()));
    let built = plan(&request_4worker(), &topo).unwrap();
    for k in 0..4usize {
        let entry = built.affinity_entry(k).unwrap();
        assert!(entry.is_rt());
        assert!(!entry.is_isolated());
        assert_eq!(entry.flags, AFFINITY_FLAG_RT);
    }
    assert_eq!(built.validate_against(&topo), Ok(()));
}

#[test]
fn scheduler_fake_plan_2worker_independent_profile() {
    let topo = topology_8();
    let four = plan_4worker();
    let two = plan_2worker();
    assert_eq!(two.workers, 2);
    assert_eq!(two.expected_workers(), 2);
    assert_eq!(two.ack_mask(), 0x3);
    assert_eq!(two.affinity_count(), 2);
    assert_eq!(two.affinity_table_len(), 64);
    assert!(two.is_two_worker_profile());
    assert_eq!(two.worker_cpu(0), Some(0));
    assert_eq!(two.worker_cpu(1), Some(1));
    assert_eq!(two.worker_cpu(2), None);
    assert_eq!(two.affinity[2], TspiWorkerAffinityV1::ZERO);
    assert_eq!(two.affinity[3], TspiWorkerAffinityV1::ZERO);
    assert_eq!(two.validate(), Ok(()));
    assert_eq!(two.validate_against(&topo), Ok(()));
    // 2/4 是独立 profile：hash 必须不同（graph_id 会把 affinity 表算进去）。
    assert_ne!(two.affinity_hash, four.affinity_hash);
    assert_eq!(two.plan_version, four.plan_version);
}

#[test]
fn scheduler_fake_plan_report_fields_match_plan() {
    let topo = topology_8();
    let built = plan_4worker();
    let report = built.report();
    assert_eq!(report.plan_version, 7);
    assert_eq!(report.workers, 4);
    assert_eq!(report.expected_workers, 4);
    assert_eq!(report.ack_mask, 0xF);
    assert_eq!(report.actual_cpu, [0, 1, 2, 3]);
    assert_eq!(report.sched_priority, [70, 70, 70, 70]);
    assert_eq!(
        report.irq_dependency_mask,
        IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER
    );
    assert_eq!(report.tick_freq_hz, TICK_FREQ_HZ);
    assert_eq!(report.spin_max_iters, SPIN_MAX);
    assert_eq!(report.cutoff_drain_iters, CUTOFF_DRAIN);
    assert_eq!(report.affinity_hash, built.affinity_hash);
    assert!(!report.two_worker_profile);
    assert!(!report.crossfade_accepted); // 归 T20；T45 不得冒充
    assert!(!report.kernel_writes_performed);
    assert!(built.validate_against(&topo).is_ok());

    let report_two = plan_2worker().report();
    assert_eq!(report_two.workers, 2);
    assert_eq!(report_two.expected_workers, 2);
    assert_eq!(report_two.ack_mask, 0x3);
    assert_eq!(report_two.actual_cpu, [0, 1, CPU_UNUSED, CPU_UNUSED]);
    assert_eq!(report_two.sched_priority, [70, 70, 0, 0]);
    assert!(report_two.two_worker_profile);
}

#[test]
fn scheduler_fake_plan_pack_affinity_table_layout() {
    let built = plan_4worker();
    let mut out = [0xAAu8; 128];
    let written = built.pack_affinity_table(&mut out).unwrap();
    assert_eq!(written, 128);
    // 第 0 条：worker_id=0、priority=70、flags=0x3、cpu=0 LE、irq mask=0b101 LE。
    assert_eq!(out[0], 0);
    assert_eq!(out[1], 70);
    assert_eq!(&out[2..4], &[0x03, 0x00]);
    assert_eq!(&out[4..6], &[0x00, 0x00]);
    assert_eq!(&out[8..16], &(0b101u64).to_le_bytes());
    // 第 1 条: offset 32，worker_id=1、cpu=1、无 IRQ。
    assert_eq!(out[32], 1);
    assert_eq!(&out[36..38], &[0x01, 0x00]);
    assert_eq!(&out[40..48], &[0u8; 8]);
    // 第 3 条 offset 96：worker_id=3、cpu=3。
    assert_eq!(out[96], 3);
    assert_eq!(&out[100..102], &[0x03, 0x00]);

    // 2 worker 只写 64 B，其后保持原样。
    let mut small = [0xAAu8; 128];
    assert_eq!(plan_2worker().pack_affinity_table(&mut small), Ok(64));
    assert_eq!(&small[64..], &[0xAAu8; 64]);

    // 输出不足 → ERR_CAPACITY。
    let mut short = [0u8; 63];
    assert_eq!(
        built.pack_affinity_table(&mut short),
        Err(RtStatus::ERR_CAPACITY)
    );
}

#[test]
fn scheduler_fake_plan_matches_plan_header_fields() {
    let built = plan_4worker();
    assert!(built.matches_plan_header(4, 4, built.affinity_hash));
    assert!(!built.matches_plan_header(2, 2, built.affinity_hash));
    assert!(!built.matches_plan_header(4, 2, built.affinity_hash));
    let mut wrong = built.affinity_hash;
    wrong[0] ^= 0xFF;
    assert!(!built.matches_plan_header(4, 4, wrong));
}

#[test]
fn scheduler_fake_plan_affinity_helper_matches_plan() {
    let topo = topology_8();
    let table = plan_affinity(&request_4worker(), &topo).unwrap();
    let built = plan_4worker();
    assert_eq!(table, built.affinity);
    let table_two = plan_affinity(&request_2worker(), &topo).unwrap();
    assert_eq!(table_two[2], TspiWorkerAffinityV1::ZERO);
    assert_eq!(table_two[3], TspiWorkerAffinityV1::ZERO);
}

// ---------------------------------------------------------------------------
// F. plan()：负向（实测拓扑/参数不足必须拒收，不 warn 降级）
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_plan_rejects_missing_allowed_evidence() {
    let mut topo = topology_8();
    topo.allowed_mask = 0;
    assert_eq!(
        plan(&request_4worker(), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_plan_rejects_offline_or_not_allowed_cpu() {
    let mut offline = topology_8();
    offline.online_mask = 0xF7; // CPU3 离线
    offline.isolated_mask = 0x07;
    offline.allowed_mask = 0xF7;
    assert_eq!(
        plan(&request_4worker(), &offline),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut not_allowed = topology_8();
    not_allowed.allowed_mask = 0x07; // CPU3 不在允许集合
    assert_eq!(
        plan(&request_4worker(), &not_allowed),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_plan_rejects_unconfigured_or_duplicate_cpu() {
    let topo = topology_8();
    let unconfigured = SchedRequest::new()
        .with_tick(TICK_FREQ_HZ, 2)
        .with_calibration(SPIN_MAX, CUTOFF_DRAIN);
    assert_eq!(
        plan(&unconfigured, &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    let duplicate = request_4worker().with_dsp_cpu(1, 0);
    assert_eq!(plan(&duplicate, &topo), Err(RtStatus::ERR_ADMISSION_SCHED));
}

#[test]
fn scheduler_fake_plan_rejects_smt_sibling_sharing() {
    let mut cores = core_map_unique_8();
    cores[1] = cores[0]; // CPU0/CPU1 同一物理核（SMT 兄弟核）
    let topo = topology_8().with_core_map(cores);
    assert_eq!(
        plan(&request_4worker(), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 换成不相邻的两核共享（CPU0/CPU2）同样拒绝。
    let mut cores = core_map_unique_8();
    cores[2] = cores[0];
    let topo = topology_8().with_core_map(cores);
    assert_eq!(
        plan(&request_4worker(), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_plan_rejects_irq_dependency_problems() {
    let topo = topology_8();

    // 依赖位无实测证据（未注入 USB_PLAYBACK 亲和）。
    let no_evidence = request_4worker().with_irq_dependency(1, IRQ_BIT_USB_PLAYBACK);
    assert_eq!(
        plan(&no_evidence, &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    // 依赖 IRQ 落在 DSP CPU 上（CPU1）：FIFO 忙等可能饿死供数 IRQ → 拒绝。
    let mut irq_on_dsp = topology_8();
    irq_on_dsp.set_irq_cpu(1, 1).unwrap();
    let with_playback_irq = request_4worker().with_irq_dependency(1, IRQ_BIT_USB_PLAYBACK);
    assert_eq!(
        plan(&with_playback_irq, &irq_on_dsp),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    // 依赖 IRQ 的 CPU 离线。
    let mut irq_offline = topology_8();
    irq_offline.set_irq_cpu(1, 7).unwrap();
    irq_offline.online_mask = 0x7F;
    irq_offline.allowed_mask = 0x7F;
    assert_eq!(
        plan(&with_playback_irq, &irq_offline),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    // 依赖 IRQ 的 CPU 不在允许集合。
    let mut irq_not_allowed = topology_8();
    irq_not_allowed.set_irq_cpu(1, 7).unwrap();
    irq_not_allowed.allowed_mask = 0x7F;
    assert_eq!(
        plan(&with_playback_irq, &irq_not_allowed),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    // 未知 IRQ 位（绕过 builder 掩码直接写字段）。
    let mut unknown_bit = request_4worker();
    unknown_bit.irq_dependency[0] = 1 << 40;
    assert_eq!(
        plan(&unknown_bit, &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_plan_rejects_bad_request_before_topology() {
    let topo = topology_8();
    assert_eq!(
        plan(&request_4worker().with_tick(0, 2), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        plan(&request_4worker().with_workers(3), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    assert_eq!(
        plan(&request_4worker().with_priorities(90, 80, 95), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

// ---------------------------------------------------------------------------
// G. 失核整块拒收 + 2 worker 独立 profile 重新准入
// ---------------------------------------------------------------------------

/// 失核 CPU2 后的实测快照（online/allowed/isolated 同步收缩，possible 保持）。
fn topology_after_losing_cpu2() -> TopologySnapshot {
    let mut topo = topology_8();
    topo.online_mask = 0xFB;
    topo.allowed_mask = 0xFB;
    topo.isolated_mask = 0x0B;
    topo
}

#[test]
fn scheduler_fake_gate_admits_healthy_plan() {
    let topo = topology_8();
    let built = plan_4worker();
    let gate = built.gate_block(&topo);
    assert_eq!(gate, SchedGate::Admit);
    assert!(gate.is_admit());
    assert_eq!(gate.reject_reason(), None);
    assert_eq!(gate.status(), RtStatus::OK);
    assert_eq!(built.lost_worker_mask(&topo), 0);
}

#[test]
fn scheduler_fake_gate_lost_worker_rejects_whole_block() {
    let topo = topology_after_losing_cpu2();
    let built = plan_4worker();
    assert_eq!(built.lost_worker_mask(&topo), 0b0100);
    let gate = built.gate_block(&topo);
    assert_eq!(
        gate,
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::LostWorkerCpu
        }
    );
    assert!(!gate.is_admit());
    assert_eq!(gate.status(), RtStatus::ERR_ADMISSION_SCHED);
    assert_eq!(
        gate.reject_reason().unwrap().status(),
        RtStatus::ERR_ADMISSION_SCHED
    );
    // 旧 4 worker 计划仍然是 4 worker（没有任何“剩 3 个继续”的半图入口）。
    assert_eq!(built.workers, 4);
    assert_eq!(built.ack_mask(), 0xF);
    assert_eq!(
        built.validate_against(&topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_gate_lost_wakeup_irq_rejects_whole_block() {
    let mut topo = topology_8();
    topo.irq_present_mask = 0; // IRQ 亲和证据丢失（例如 /proc/irq 不可读）
    let built = plan_4worker();
    assert_eq!(
        built.gate_block(&topo),
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::LostWakeupIrq
        }
    );
}

#[test]
fn scheduler_fake_gate_irq_on_dsp_cpu_rejects_whole_block() {
    let mut topo = topology_8();
    topo.set_irq_cpu(0, 1).unwrap(); // USB capture IRQ 被移到 DSP CPU1
    let built = plan_4worker();
    assert_eq!(
        built.gate_block(&topo),
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::IrqOnDspCpu
        }
    );
}

#[test]
fn scheduler_fake_gate_topology_missing_and_plan_invalid() {
    let mut no_allowed = topology_8();
    no_allowed.allowed_mask = 0;
    let built = plan_4worker();
    assert_eq!(
        built.gate_block(&no_allowed),
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::TopologyMissing
        }
    );

    let mut tampered = plan_4worker();
    tampered.affinity_hash[0] ^= 0xFF;
    assert_eq!(
        tampered.gate_block(&topology_8()),
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::PlanInvalid
        }
    );
}

#[test]
fn scheduler_fake_gate_stale_topology_rejects_whole_block() {
    let mut topo = topology_8();
    topo.isolated_mask = 0x0D; // CPU1 隔离状态变化（计划标记 stale）
    assert_eq!(
        plan_4worker().gate_block(&topo),
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::StaleTopology
        }
    );
}

#[test]
fn scheduler_fake_gate_two_worker_needs_both_cpus() {
    let mut topo = topology_8();
    topo.online_mask = 0xFD; // CPU1 离线
    topo.allowed_mask = 0xFD;
    topo.isolated_mask = 0x0D;
    let two = plan_2worker();
    assert_eq!(two.lost_worker_mask(&topo), 0b10);
    assert_eq!(
        two.gate_block(&topo),
        SchedGate::RejectWholeBlock {
            reason: SchedRejectReason::LostWorkerCpu
        }
    );
}

#[test]
fn scheduler_fake_two_worker_re_admission_after_one_core_lost() {
    let topo = topology_after_losing_cpu2();
    let four = plan_4worker();
    let req = request_2worker();
    let two = four.two_worker_re_admission(&req, &topo).unwrap();
    assert_eq!(two.workers, 2);
    assert_eq!(two.expected_workers(), 2);
    assert_eq!(two.ack_mask(), 0x3);
    assert!(two.is_two_worker_profile());
    assert_eq!(two.worker_cpu(0), Some(0));
    assert_eq!(two.worker_cpu(1), Some(1));
    assert_eq!(two.affinity[2], TspiWorkerAffinityV1::ZERO);
    assert_ne!(two.affinity_hash, four.affinity_hash);
    assert_eq!(two.validate_against(&topo), Ok(()));
    assert_eq!(two.gate_block(&topo), SchedGate::Admit);
    assert_eq!(four.plan_version, two.plan_version); // 新计划版本由 T26/epoch 决定
}

#[test]
fn scheduler_fake_two_worker_re_admission_uses_healthy_subset() {
    // 失掉 CPU1/CPU2：healthy = [0, 3]（保持原 worker 顺序，不重排）。
    let mut topo = topology_8();
    topo.online_mask = 0xF9;
    topo.allowed_mask = 0xF9;
    topo.isolated_mask = 0x09;
    let four = plan_4worker();
    let two = four
        .two_worker_re_admission(&request_2worker(), &topo)
        .unwrap();
    assert_eq!(two.worker_cpu(0), Some(0));
    assert_eq!(two.worker_cpu(1), Some(3));
    assert_eq!(two.validate_against(&topo), Ok(()));
}

#[test]
fn scheduler_fake_two_worker_re_admission_refuses_single_healthy_cpu() {
    // 只余 CPU0 健康：2 worker 独立 profile 不可行 → Err，调用方只能 last-good/mute。
    let mut topo = topology_8();
    topo.online_mask = 0xF1;
    topo.allowed_mask = 0xF1;
    topo.isolated_mask = 0x01;
    let four = plan_4worker();
    assert_eq!(
        four.two_worker_re_admission(&request_2worker(), &topo),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_two_worker_re_admission_requires_loss_and_four_profile() {
    let topo = topology_8();
    let four = plan_4worker();
    let req = request_2worker();
    // 无失核：不是降级路径。
    assert_eq!(
        four.two_worker_re_admission(&req, &topo),
        Err(RtStatus::ERR_SWITCH_BUSY)
    );
    // 已经是 2 worker 计划：不允许再次“降级”。
    let two = plan_2worker();
    let mut lost = topology_after_losing_cpu2();
    lost.online_mask = 0xF9;
    lost.allowed_mask = 0xF9;
    lost.isolated_mask = 0x09;
    assert_eq!(
        two.two_worker_re_admission(&req, &lost),
        Err(RtStatus::ERR_SWITCH_BUSY)
    );
}

#[test]
fn scheduler_fake_two_worker_re_admission_keeps_first_two_dependencies() {
    let topo = topology_after_losing_cpu2();
    let four = plan_4worker();
    let mut req = request_2worker();
    req.irq_dependency[0] = IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER;
    req.irq_dependency[1] = IRQ_DEPENDENCY_NONE;
    let two = four.two_worker_re_admission(&req, &topo).unwrap();
    assert_eq!(
        two.worker_irq_mask(0),
        Some(IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER)
    );
    assert_eq!(two.worker_irq_mask(1), Some(0));
    assert_eq!(
        two.union_irq_dependency(),
        IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER
    );
}

// ---------------------------------------------------------------------------
// H. 不自动写内核（WBS T45 DoD）+ 源码级无 syscall/FFI 断言
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_no_kernel_writes_performed() {
    let topo = topology_8();
    let built = plan_4worker();
    assert!(!KERNEL_WRITES_PERFORMED);
    assert_eq!(apply_plan_to_kernel(&built), Err(RtStatus::ERR_UNSUPPORTED));
    assert_eq!(
        apply_plan_to_kernel(&built).unwrap_err().code(),
        122 // ERR_UNSUPPORTED
    );
    assert!(!built.report().kernel_writes_performed);
    assert!(built.validate_against(&topo).is_ok());
}

#[test]
fn scheduler_fake_source_has_no_kernel_write_or_ffi_calls() {
    // 直接扫描本模块源码：T45 只产出计划，禁止任何内核设置/FFI/syscall 入口落地。
    let source = include_str!("../../src/scheduler.rs");
    assert!(source.contains("KERNEL_WRITES_PERFORMED"));
    let forbidden = [
        "sched_setaffinity",
        "sched_setscheduler",
        "pthread_setaffinity",
        "unsafe {",
        "unsafe{",
        "extern \"C\"",
        "std::fs",
        "std::process",
        "libc::",
        "asm!",
    ];
    for token in forbidden {
        assert!(
            !source.contains(token),
            "scheduler.rs 不得包含内核写入/FFI token: {}",
            token
        );
    }
}

// ---------------------------------------------------------------------------
// I. 与 T18 admission / T09 workers 接口对齐 + 实测快照端到端
// ---------------------------------------------------------------------------

#[test]
fn scheduler_fake_t09_ack_mask_and_profile_alignment() {
    use tspi_dsp_core::workers::ack_mask as t09_ack_mask;
    let four = plan_4worker();
    let two = plan_2worker();
    assert_eq!(four.ack_mask(), t09_ack_mask(four.expected_workers()));
    assert_eq!(four.ack_mask(), t09_ack_mask(RT_WORKERS));
    assert_eq!(four.ack_mask(), 0xF);
    assert_eq!(two.ack_mask(), t09_ack_mask(two.expected_workers()));
    assert_eq!(two.ack_mask(), t09_ack_mask(RT_WORKERS_FALLBACK));
    assert_eq!(two.ack_mask(), 0x3);
    assert!(two.is_two_worker_profile() == (two.workers == RT_WORKERS_FALLBACK));
    assert!(four.is_two_worker_profile() == (four.workers == RT_WORKERS_FALLBACK));
    // §3.4：affinity 表条目 <= WORKER_AFFINITY_COUNT。
    assert!(four.affinity_count() as usize <= tspi_dsp_core::rt::WORKER_BITS as usize + 3);
    assert!(two.affinity_count() as usize <= WORKER_AFFINITY_COUNT);
}

#[test]
fn scheduler_fake_t18_cpu_set_gate_wiring() {
    use crate::admission::{validate as admission_validate, AdmissionEnv};
    use tspi_dsp_core::abi::{TspiPlanHeaderV1, ABI_VERSION, TSPI_PLAN_MAGIC};
    use tspi_dsp_core::rt::PlanView;

    let topo = topology_8();
    let planned = plan_4worker();
    let header = TspiPlanHeaderV1 {
        magic: TSPI_PLAN_MAGIC,
        abi_version: ABI_VERSION,
        header_size: 128,
        plan_version: 1,
        sample_rate: 48_000,
        block_size: 128,
        n_in: 2,
        n_out: 2,
        low_band_count: 0,
        path_count: 0,
        diag_fir_count: 0,
        max_taps: 64,
        workers: RT_WORKERS,
        flags: 0,
        precision: 0,
        reserved0: 0,
        graph_id: [0u8; 32],
        total_coeff_bytes: 0,
        path_table_offset: 0,
        diag_table_offset: 0,
        in_stride: 16,
        out_stride: 16,
        role_table_offset: 0,
        role_table_count: 0,
        reserved1: 0,
        affinity_table_offset: 0,
        affinity_count: 0,
        reserved2: 0,
        ctl_meta_offset: 0,
        ctl_meta_len: 0,
        reserved3: [0u32; 3],
        header_crc32c: 0,
    };
    let view = PlanView {
        header: &header,
        paths: &[],
        diags: &[],
        roles: &[],
        coeffs: &[],
    };
    let mut env = AdmissionEnv::new();
    env.rt_privilege_ok = true;
    env.clock_source_ok = true;
    env.cpu_set_ok = planned.cpu_set_ok(&topo);
    assert_eq!(admission_validate(&view, &env), Ok(()));
    // T45 判定为 false（失核）时 T18 必须拒启（115），不得 warn 后自称 RT。
    let mut lost = topology_after_losing_cpu2();
    lost.core_of_cpu = core_map_unique_8();
    env.cpu_set_ok = planned.cpu_set_ok(&lost);
    assert!(!env.cpu_set_ok);
    assert_eq!(
        admission_validate(&view, &env),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_measured_snapshot_to_plan_end_to_end() {
    // 实测入口（纯文本，无设备读取）：S0-02 cmdline + S0-03 lscpu/online + allowed + isolated，
    // S0-05 IRQ 亲和（smp_affinity_list）→ 参数化计划 → gate Admit。
    let mut topo =
        TopologySnapshot::from_measured(LSCPU_8, "0-7", "0-7", "0-3", CMDLINE_8).unwrap();
    let capture = parse_irq_affinity_list("6").unwrap();
    let ptp = parse_irq_affinity_list("7").unwrap();
    assert_eq!(capture, 1 << 6);
    assert_eq!(ptp, 1 << 7);
    topo.set_irq_affinity_mask(0, capture).unwrap(); // USB capture
    topo.set_irq_affinity_mask(2, ptp).unwrap(); // PTP/timer
    assert_eq!(topo.irq_cpu_for_bit(IRQ_BIT_USB_CAPTURE), Some(6));
    assert_eq!(topo.irq_cpu_for_bit(IRQ_BIT_PTP_TIMER), Some(7));

    let built = plan(&request_4worker(), &topo).unwrap();
    assert_eq!(built.validate_against(&topo), Ok(()));
    assert_eq!(built.gate_block(&topo), SchedGate::Admit);
    assert_eq!(built.report().actual_cpu, [0, 1, 2, 3]);
    assert_eq!(built.report().sched_priority, [70, 70, 70, 70]);
    assert_eq!(
        built.report().irq_dependency_mask,
        IRQ_BIT_USB_CAPTURE | IRQ_BIT_PTP_TIMER
    );
    assert_eq!(built.report().ack_mask, 0xF);
    assert_eq!(built.report().tick_freq_hz, TICK_FREQ_HZ);
}

#[test]
fn scheduler_fake_affinity_hash_changes_with_inputs() {
    let topo = topology_8();
    let base = plan(&request_4worker(), &topo).unwrap();
    // 同输入确定。
    assert_eq!(base, plan(&request_4worker(), &topo).unwrap());
    // plan_version 不参与 hash（§11.10 graph_id 不含 plan_version）。
    let versioned = plan(&request_4worker().with_plan_version(99), &topo).unwrap();
    assert_eq!(versioned.affinity_hash, base.affinity_hash);
    assert_ne!(versioned, base);
    // CPU 分配变化 → hash 变化。
    let swapped = request_4worker().with_dsp_cpu(0, 1).with_dsp_cpu(1, 0);
    assert_ne!(
        plan(&swapped, &topo).unwrap().affinity_hash,
        base.affinity_hash
    );
    // FIFO 优先级（实测校正）变化 → hash 变化。
    let priority = request_4worker().with_priorities(65, 80, 90);
    assert_ne!(
        plan(&priority, &topo).unwrap().affinity_hash,
        base.affinity_hash
    );
    // IRQ 依赖变化 → hash 变化。
    let dep = request_4worker().with_irq_dependency(0, IRQ_BIT_USB_CAPTURE);
    assert_ne!(plan(&dep, &topo).unwrap().affinity_hash, base.affinity_hash);
    // profile 变化 → hash 变化（2/4 独立计划）。
    assert_ne!(plan_2worker().affinity_hash, base.affinity_hash);
}

#[test]
fn scheduler_fake_validate_detects_tampered_plan() {
    let mut tampered_hash = plan_4worker();
    tampered_hash.affinity_hash = [0u8; 8];
    assert_eq!(tampered_hash.validate(), Err(RtStatus::ERR_HASH));

    let mut unused_nonzero = plan_2worker();
    unused_nonzero.affinity[3] = TspiWorkerAffinityV1::new(3, 70, AFFINITY_FLAG_RT, 3, 0);
    assert_eq!(
        unused_nonzero.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut wrong_worker_id = plan_4worker();
    wrong_worker_id.affinity[2].worker_id = 9;
    assert_eq!(
        wrong_worker_id.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut priority_mismatch = plan_4worker();
    priority_mismatch.affinity[1].sched_priority = 60;
    assert_eq!(
        priority_mismatch.validate(),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut dup_cpu = plan_4worker();
    dup_cpu.affinity[1].cpu_index = dup_cpu.affinity[0].cpu_index;
    dup_cpu.affinity_hash = compute_affinity_hash(dup_cpu.workers, &dup_cpu.affinity);
    assert_eq!(dup_cpu.validate(), Err(RtStatus::ERR_ADMISSION_SCHED));
}

#[test]
fn scheduler_fake_validate_against_detects_topology_staleness() {
    let built = plan_4worker();

    let mut cpu_offline = topology_8();
    cpu_offline.online_mask = 0xFB;
    cpu_offline.allowed_mask = 0xFB;
    cpu_offline.isolated_mask = 0x0B;
    assert_eq!(
        built.validate_against(&cpu_offline),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut allowed_shrunk = topology_8();
    allowed_shrunk.allowed_mask = 0x07;
    assert_eq!(
        built.validate_against(&allowed_shrunk),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut iso_changed = topology_8();
    iso_changed.isolated_mask = 0x0D;
    assert_eq!(
        built.validate_against(&iso_changed),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut smt = topology_8();
    let mut cores = core_map_unique_8();
    cores[2] = cores[0];
    smt.core_of_cpu = cores;
    assert_eq!(
        built.validate_against(&smt),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut irq_removed = topology_8();
    irq_removed.irq_present_mask = 0;
    assert_eq!(
        built.validate_against(&irq_removed),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );

    let mut irq_moved_to_dsp = topology_8();
    irq_moved_to_dsp.set_irq_cpu(0, 2).unwrap();
    assert_eq!(
        built.validate_against(&irq_moved_to_dsp),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn scheduler_fake_affinity_hash_golden_4worker() {
    // 冻结确定性证据（D-04 算法回归；改动算法/表项布局必须显式更新并说明）。
    let hash = plan_4worker().affinity_hash;
    // 2026-11-17 首次运行冻结：算法/夹具不变则必须逐位一致。
    let expected: [u8; 8] = [21, 38, 236, 30, 57, 140, 174, 19];
    assert_eq!(hash, expected, "golden affinity_hash = {:?}", hash);
}

#[test]
fn scheduler_fake_affinity_hash_golden_2worker() {
    let hash = plan_2worker().affinity_hash;
    // 2026-11-17 首次运行冻结：2 worker 独立 profile 的 hash 必须与 4 worker 不同。
    let expected: [u8; 8] = [255, 223, 222, 86, 154, 222, 208, 57];
    assert_eq!(hash, expected, "golden affinity_hash(2w) = {:?}", hash);
}

// === END ===
