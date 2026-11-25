// T21（fault）离线模型测试：有界直方图 / 另核 watchdog / fallback 预分配+准入 /
// 安全 mute + quarantine 不可释放 / 非 RT 导出。
//
// 本文件经 src/fault.rs 末尾的 cfg(test) mod fault_tests { include!(...) } 进入单测构建
// （cargo 不自动发现 tests/<子目录>；公共注册属 integrator，勿写入
// tests/public_registration.rs）。运行：
//   cargo test -p tspi-rt-host fault
// 全部测试名 t21_ 前缀；离线：不碰设备/不启线程/不做板端动作。

use super::*;
use core::mem::needs_drop;

use crate::admission::{AdmissionEnv, AdmissionReport, MemoryPlan};
use tspi_dsp_core::rt::MAX_OUTPUT_CHANNELS;
use tspi_dsp_core::workers::{ack_mask, TspiRtControlV1};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

const GOOD_RATE: u32 = 48_000;
const GOOD_BLOCK: u16 = 128;

fn hist_config() -> HistogramConfig {
    match HistogramConfig::uniform(10) {
        Ok(config) => config,
        Err(_) => panic!("uniform(10) must succeed"),
    }
}

fn env_4w_48k_128() -> AdmissionEnv {
    let mut env = AdmissionEnv::new();
    env.sample_rate_hz = GOOD_RATE;
    env.block_size = GOOD_BLOCK;
    env.rt_workers = RT_WORKERS;
    env.expected_workers = RT_WORKERS;
    env.rt_privilege_ok = true;
    env.clock_source_ok = true;
    env.cpu_set_ok = true;
    env.mlock_limit_bytes = 1 << 30;
    env
}

fn env_2w() -> AdmissionEnv {
    let mut env = env_4w_48k_128();
    env.rt_workers = RT_WORKERS_FALLBACK;
    env.expected_workers = RT_WORKERS_FALLBACK;
    env
}

fn memory_plan(safety_silence_bytes: u64) -> MemoryPlan {
    MemoryPlan {
        capture_payload_bytes: 20 * 128 * 4,
        playback_payload_bytes: 22 * 128 * 4,
        worker_partial_bytes: 4 * 128 * 22 * 4,
        quarantine_bytes: 8 * 128 * 22 * 4,
        safety_silence_bytes,
        desc_bytes: 8 * 64,
        arena_bytes: 40_960 + 45_056 + 11_264 + 90_112 + 11_264 + 512,
        graph_scratch_bytes: 16_384,
        graph_current_bytes: 1_048_576,
        graph_pending_bytes: 1_048_576,
        stacks_bytes: 8 * 524_288,
        control_plane_bytes: 70_528 + 65_536,
        total_bytes: 4_194_304,
        path_count: 25,
        diag_count: 7,
    }
}

fn fallback_spec() -> FallbackGraphSpec {
    FallbackGraphSpec {
        plan_version: 9,
        graph_id: [0x5A; GRAPH_ID_BYTES],
        output_channels: 22,
        identity_g: true,
        p_post_present: true,
        matrix_routes: 0,
        worst_block_ns: 1_000_000,
    }
}

fn admitted_fallback() -> FallbackAdmission {
    let env = env_4w_48k_128();
    let memory = memory_plan(128 * 22 * 4);
    match admit_fallback(&fallback_spec(), &env, &memory) {
        Ok(admission) => admission,
        Err(status) => panic!("fallback admission must succeed, got {:?}", status),
    }
}

fn supervisor_with_safety_output() -> FaultSupervisor {
    FaultSupervisor::with_fallback(hist_config(), admitted_fallback())
}

fn report_for(workers: u8) -> AdmissionReport {
    AdmissionReport {
        plan_version: 7,
        arena_bytes: 4_096,
        mlocked_bytes: 1 << 20,
        stack_bytes: 1 << 20,
        workers,
        graph_id: [0xAB; GRAPH_ID_BYTES],
        expected_workers: workers,
        ack_mask: ack_mask(workers),
        actual_cpu: [0; 4],
        sched_priority: [70; 4],
        irq_dependency_mask: 0,
    }
}

fn watchdog_config() -> WatchdogConfig {
    WatchdogConfig::new(6, [0, 1, 2, 3], RT_WORKERS)
}

fn watchdog_with_small_thresholds() -> Watchdog {
    let mut config = watchdog_config();
    config.stale_samples = 3;
    config.worker_lost_samples = 2;
    config.retire_ack_samples = 2;
    match Watchdog::with_config(config) {
        Ok(watchdog) => watchdog,
        Err(status) => panic!("watchdog config must be valid, got {:?}", status),
    }
}

// ---------------------------------------------------------------------------
// A. 布局/静态断言/源码纪律
// ---------------------------------------------------------------------------

#[test]
fn t21_constants_and_fixed_layout_match_core_v1() {
    assert_eq!(HIST_BUCKETS, 32);
    assert_eq!(QUARANTINE_SLOTS, 8);
    assert_eq!(HIST_METRIC_COUNT, 6);
    assert_eq!(HistMetric::COUNT, 6);
    assert_eq!(HISTOGRAM_BYTES, 3072);
    assert_eq!(core::mem::size_of::<HistogramTable>(), 3072);
    assert_eq!(core::mem::align_of::<HistogramTable>(), 64);
    assert_eq!(core::mem::size_of::<EventLanes>(), 66_048);
    assert_eq!(EventLanes::new().dropped_total(), 0);
    assert_eq!(QuarantinePool::capacity(), 8);
    assert_eq!(RT_WORKERS, 4);
    assert_eq!(RT_WORKERS_FALLBACK, 2);
    assert_eq!(WATCHDOG_PERIOD_MS, 10);
    assert_eq!(WATCHDOG_STALE_BLOCKS, 4);
}

#[test]
fn t21_public_types_have_no_drop() {
    assert!(!needs_drop::<EventRingLane>());
    assert!(!needs_drop::<EventLanes>());
    assert!(!needs_drop::<SafetyAction>());
    assert!(!needs_drop::<SafetyMode>());
    assert!(!needs_drop::<WatchdogAction>());
    assert!(!needs_drop::<Watchdog>());
    assert!(!needs_drop::<WatchdogSample>());
    assert!(!needs_drop::<HistogramConfig>());
    assert!(!needs_drop::<HistogramTable>());
    assert!(!needs_drop::<HistogramSnapshot>());
    assert!(!needs_drop::<FaultCounters>());
    assert!(!needs_drop::<CountersSnapshot>());
    assert!(!needs_drop::<QuarantinePool>());
    assert!(!needs_drop::<QuarantineSnapshot>());
    assert!(!needs_drop::<FallbackAdmission>());
    assert!(!needs_drop::<ReAdmissionTicket>());
    assert!(!needs_drop::<FaultSupervisor>());
}

#[test]
fn t21_module_source_has_no_heap_lock_syscall_or_panic_tokens() {
    // 只扫描可执行代码行（去注释）；const 静态断言块不在 RT 运行期，跳过。
    let source = include_str!("../../src/fault.rs");
    let mut in_const_assert = false;
    let mut code_lines: Vec<&str> = Vec::new();
    for raw_line in source.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("const _: () = {") {
            in_const_assert = true;
            continue;
        }
        if in_const_assert {
            if trimmed == "};" {
                in_const_assert = false;
            }
            continue;
        }
        let comment_start = raw_line.find("//");
        let code = match comment_start {
            Some(index) => &raw_line[..index],
            None => raw_line,
        };
        if !code.trim().is_empty() {
            code_lines.push(code);
        }
    }
    let forbidden = [
        "Vec<",
        "String",
        "Box<",
        "Arc<",
        "Rc<",
        "Mutex",
        "RwLock",
        "OnceLock",
        "Condvar",
        "thread::spawn",
        "std::fs",
        "std::net",
        "std::process",
        "Instant::now",
        "SystemTime",
        "clock_gettime",
        "println!",
        "eprintln!",
        "format!(",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "catch_unwind",
        "unsafe ",
    ];
    for (index, line) in code_lines.iter().enumerate() {
        for needle in forbidden {
            assert!(
                !line.contains(needle),
                "fault.rs line {} contains forbidden token {:?}: {}",
                index + 1,
                needle,
                line
            );
        }
    }
}

// ---------------------------------------------------------------------------
// B. watchdog 另核/配置
// ---------------------------------------------------------------------------

#[test]
fn t21_watchdog_rejects_observation_core_equal_to_dsp_core() {
    for core in 0u16..4 {
        let config = WatchdogConfig::new(core, [0, 1, 2, 3], RT_WORKERS);
        assert_eq!(
            Watchdog::with_config(config).is_err(),
            true,
            "core {} must be rejected",
            core
        );
        match Watchdog::with_config(config) {
            Err(status) => assert_eq!(status.code(), RtStatus::ERR_ADMISSION_SCHED.code()),
            Ok(_) => panic!("same-core watchdog must be rejected"),
        }
    }
}

#[test]
fn t21_watchdog_accepts_separate_observation_core() {
    let watchdog = match Watchdog::with_config(watchdog_config()) {
        Ok(watchdog) => watchdog,
        Err(status) => panic!("separate core must be accepted, got {:?}", status),
    };
    assert_eq!(watchdog.config().observation_core, 6);
    assert_eq!(watchdog.config().dsp_cores, [0, 1, 2, 3]);
    assert!(watchdog.affinity_verified());
    assert_eq!(watchdog.config().sample_period_ms, WATCHDOG_PERIOD_MS);
    assert_eq!(watchdog.config().stale_samples, WATCHDOG_STALE_BLOCKS);
}

#[test]
fn t21_watchdog_rejects_invalid_dsp_core_count_and_thresholds() {
    let mut three = WatchdogConfig::new(6, [0, 1, 2, 3], 3);
    assert_eq!(
        Watchdog::with_config(three).is_err(),
        true,
        "dsp_core_count 3 is not a v1 profile"
    );
    three.dsp_core_count = RT_WORKERS;
    three.sample_period_ms = 0;
    match Watchdog::with_config(three) {
        Err(status) => assert_eq!(status, RtStatus::ERR_BAD_LAYOUT),
        Ok(_) => panic!("zero period must be rejected"),
    }
    let mut zero_stale = watchdog_config();
    zero_stale.stale_samples = 0;
    assert_eq!(Watchdog::with_config(zero_stale).is_err(), true);
}

#[test]
fn t21_watchdog_two_worker_profile() {
    let config = WatchdogConfig::new(2, [0, 1, 0, 0], RT_WORKERS_FALLBACK);
    let watchdog = match Watchdog::with_config(config) {
        Ok(watchdog) => watchdog,
        Err(status) => panic!("2-worker profile must be accepted, got {:?}", status),
    };
    assert_eq!(watchdog.config().dsp_core_count, 2);
    // 观测核 2 与 worker0/1 不同 → 另核成立。
    assert!(watchdog.affinity_verified());
    let same = WatchdogConfig::new(1, [0, 1, 0, 0], RT_WORKERS_FALLBACK);
    assert_eq!(Watchdog::with_config(same).is_err(), true);
}

// ---------------------------------------------------------------------------
// C. watchdog 心跳/stale/只置位/不自动恢复
// ---------------------------------------------------------------------------

#[test]
fn t21_watchdog_stale_boundary_sets_safety_request() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    // 首次采样：建立 last_heartbeat=0。
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    // 阈值 3：前 2 次 stale 采样仍未到阈值。
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.stale_samples(), 2);
    // 第 3 次 stale 采样 → 请求 mute（RT_E_WATCHDOG=406）。
    let action = watchdog.observe(&ctl);
    assert_eq!(action, WatchdogAction::RequestMute(RtStatus::RT_E_WATCHDOG));
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 1);
    assert!(watchdog.latched());
    assert_eq!(watchdog.latched_reason(), RtStatus::RT_E_WATCHDOG);
}

#[test]
fn t21_watchdog_heartbeat_advance_resets_stale_counter() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.stale_samples(), 1);
    ctl.heartbeat_seq.fetch_add(1, Ordering::Release);
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.stale_samples(), 0);
}

#[test]
fn t21_watchdog_epoch_change_resets_stale_without_mute() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.stale_samples(), 1);
    ctl.block_start_epoch.store(1, Ordering::Release);
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
    assert_eq!(watchdog.stale_samples(), 0);
    assert!(!watchdog.latched());
}

#[test]
fn t21_watchdog_latch_is_not_auto_cleared_by_heartbeat() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    for _ in 0..4 {
        let _ = watchdog.observe(&ctl);
    }
    assert!(watchdog.latched());
    // heartbeat 恢复也不得自动清除（§5.1 硬规则：不得自动恢复 Running）。
    for _ in 0..8 {
        ctl.heartbeat_seq.fetch_add(1, Ordering::Release);
        let action = watchdog.observe(&ctl);
        assert_eq!(action, WatchdogAction::RequestMute(RtStatus::RT_E_WATCHDOG));
    }
    assert!(watchdog.latched());
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 1);
}

#[test]
fn t21_watchdog_reset_requires_readmission_ticket() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    for _ in 0..4 {
        let _ = watchdog.observe(&ctl);
    }
    assert!(watchdog.latched());
    let env = env_4w_48k_128();
    let report = report_for(RT_WORKERS);
    let ticket = match ReAdmissionTicket::new(&report, &env) {
        Ok(ticket) => ticket,
        Err(status) => panic!("ticket must be valid, got {:?}", status),
    };
    assert_eq!(watchdog.reset_after_readmission(&ticket), RtStatus::OK);
    assert!(!watchdog.latched());
    assert_eq!(watchdog.stale_samples(), 0);
    // 复位后按新样本重新计数。
    assert_eq!(watchdog.observe(&ctl), WatchdogAction::None);
}

#[test]
fn t21_watchdog_sample_validation_rejects_bad_expected_workers() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let bad = WatchdogSample {
        expected_workers: 3,
        ..WatchdogSample::ok(1, 0, 3, 0, 0)
    };
    let action = watchdog.observe_sample(&bad, &ctl);
    assert_eq!(action, WatchdogAction::Fault(RtStatus::RT_E_ABI_RUNTIME));
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 2);
}

#[test]
fn t21_watchdog_sample_validation_rejects_stray_mask_bits() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    // expected=2 但 ack 位图含 bit2 → ABI fault。
    let bad = WatchdogSample::ok(1, 0, RT_WORKERS_FALLBACK, 0x7, 0x3);
    assert_eq!(
        watchdog.observe_sample(&bad, &ctl),
        WatchdogAction::Fault(RtStatus::RT_E_ABI_RUNTIME)
    );
}

#[test]
fn t21_watchdog_worker_lost_requests_fault_after_threshold() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let sample = WatchdogSample {
        worker_alive_mask: 0b1011, // worker 2 失联
        ..WatchdogSample::ok(1, 0, RT_WORKERS, 0xF, 0xF)
    };
    assert_eq!(watchdog.observe_sample(&sample, &ctl), WatchdogAction::None);
    assert_eq!(
        watchdog.observe_sample(&sample, &ctl),
        WatchdogAction::Fault(RtStatus::RT_E_WORKER_LOST)
    );
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 2);
    assert_eq!(watchdog.latched_reason(), RtStatus::RT_E_WORKER_LOST);
}

#[test]
fn t21_watchdog_worker_alive_resets_lost_counter() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let lost = WatchdogSample {
        worker_alive_mask: 0b1110,
        ..WatchdogSample::ok(1, 0, RT_WORKERS, 0xF, 0xF)
    };
    assert_eq!(watchdog.observe_sample(&lost, &ctl), WatchdogAction::None);
    let alive = WatchdogSample::ok(2, 0, RT_WORKERS, 0xF, 0xF);
    assert_eq!(watchdog.observe_sample(&alive, &ctl), WatchdogAction::None);
    assert_eq!(watchdog.observe_sample(&lost, &ctl), WatchdogAction::None);
    assert!(!watchdog.latched());
}

#[test]
fn t21_watchdog_lost_core_mask_counts_as_worker_lost() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let sample = WatchdogSample {
        lost_core_mask: 0b0100,
        ..WatchdogSample::ok(1, 0, RT_WORKERS, 0xF, 0xF)
    };
    assert_eq!(watchdog.observe_sample(&sample, &ctl), WatchdogAction::None);
    assert_eq!(
        watchdog.observe_sample(&sample, &ctl),
        WatchdogAction::Fault(RtStatus::RT_E_WORKER_LOST)
    );
}

#[test]
fn t21_watchdog_swap_ack_stall_requests_fault() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let stalled = WatchdogSample::ok(1, 0, RT_WORKERS, 0b0111, 0xF);
    assert_eq!(
        watchdog.observe_sample(&stalled, &ctl),
        WatchdogAction::None
    );
    assert_eq!(
        watchdog.observe_sample(&stalled, &ctl),
        WatchdogAction::Fault(RtStatus::RT_E_WORKER_LOST)
    );
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 2);
}

#[test]
fn t21_watchdog_full_swap_ack_resets_stall_counter() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let stalled = WatchdogSample::ok(1, 0, RT_WORKERS, 0b0111, 0xF);
    assert_eq!(
        watchdog.observe_sample(&stalled, &ctl),
        WatchdogAction::None
    );
    let complete = WatchdogSample::ok(2, 0, RT_WORKERS, 0xF, 0xF);
    assert_eq!(
        watchdog.observe_sample(&complete, &ctl),
        WatchdogAction::None
    );
    assert_eq!(
        watchdog.observe_sample(&stalled, &ctl),
        WatchdogAction::None
    );
    assert!(!watchdog.latched());
}

#[test]
fn t21_watchdog_device_lost_requests_mute() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let sample = WatchdogSample {
        device_lost: true,
        ..WatchdogSample::ok(1, 0, RT_WORKERS, 0xF, 0xF)
    };
    assert_eq!(
        watchdog.observe_sample(&sample, &ctl),
        WatchdogAction::RequestMute(RtStatus::RT_E_DEVICE_LOST)
    );
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 1);
    assert_eq!(watchdog.latched_reason(), RtStatus::RT_E_DEVICE_LOST);
}

#[test]
fn t21_watchdog_only_escalates_mute_to_fault() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    for _ in 0..4 {
        let _ = watchdog.observe(&ctl);
    }
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 1);
    let lost = WatchdogSample {
        worker_alive_mask: 0b1110,
        ..WatchdogSample::ok(1, 0, RT_WORKERS, 0xF, 0xF)
    };
    // 已 mute：第一个失联采样保持 mute（不重复置位），第二个升级 fault。
    assert_eq!(
        watchdog.observe_sample(&lost, &ctl),
        WatchdogAction::RequestMute(RtStatus::RT_E_WATCHDOG)
    );
    assert_eq!(
        watchdog.observe_sample(&lost, &ctl),
        WatchdogAction::Fault(RtStatus::RT_E_WORKER_LOST)
    );
    assert_eq!(ctl.safety_request.load(Ordering::Acquire), 2);
    // fault 后更弱动作不改变 latch。
    assert_eq!(
        watchdog.observe_device_lost(&ctl),
        WatchdogAction::Fault(RtStatus::RT_E_WORKER_LOST)
    );
}

#[test]
fn t21_watchdog_two_worker_ack_mask_not_hardcoded_all() {
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    // expected=2，all-ack=0x3（不是 0xF）；0x3 完成 → 无动作。
    let complete = WatchdogSample::ok(1, 0, RT_WORKERS_FALLBACK, 0x3, 0x3);
    assert_eq!(
        watchdog.observe_sample(&complete, &ctl),
        WatchdogAction::None
    );
    // 0x1 未 all-ack → 阈值后 fault。
    let stalled = WatchdogSample::ok(2, 0, RT_WORKERS_FALLBACK, 0x1, 0x3);
    assert_eq!(
        watchdog.observe_sample(&stalled, &ctl),
        WatchdogAction::None
    );
    assert_eq!(
        watchdog.observe_sample(&stalled, &ctl),
        WatchdogAction::Fault(RtStatus::RT_E_WORKER_LOST)
    );
}

// ---------------------------------------------------------------------------
// D. 有界直方图
// ---------------------------------------------------------------------------

#[test]
fn t21_histogram_uniform_bucket_boundaries() {
    let config = hist_config();
    assert_eq!(config.bounds(HistMetric::BlockTicks).len(), 31);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 0), 0);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 9), 0);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 10), 1);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 19), 1);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 300), 30);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 309), 30);
    // 第 32 桶 = 溢出桶：>= 31*step。
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, 310), 31);
    assert_eq!(config.bucket_of(HistMetric::BlockTicks, u64::MAX), 31);
}

#[test]
fn t21_histogram_records_are_isolated_per_metric_and_worker() {
    let config = hist_config();
    let table = HistogramTable::new();
    assert_eq!(
        table.record(&config, HistMetric::BlockTicks, 0, 5),
        RtStatus::OK
    );
    assert_eq!(
        table.record(&config, HistMetric::BlockTicks, 0, 5),
        RtStatus::OK
    );
    assert_eq!(
        table.record(&config, HistMetric::BlockTicks, 3, 15),
        RtStatus::OK
    );
    assert_eq!(
        table.record(&config, HistMetric::SpinTicks, 0, 5),
        RtStatus::OK
    );
    assert_eq!(table.count(HistMetric::BlockTicks, 0, 0), 2);
    assert_eq!(table.count(HistMetric::BlockTicks, 0, 1), 0);
    assert_eq!(table.count(HistMetric::BlockTicks, 3, 1), 1);
    assert_eq!(table.count(HistMetric::SpinTicks, 0, 0), 1);
    assert_eq!(table.count(HistMetric::PhaseGapTicks, 0, 0), 0);
}

#[test]
fn t21_histogram_overflow_bucket_collects_tail() {
    let config = hist_config();
    let table = HistogramTable::new();
    for value in [310u64, 311, 1_000, u64::MAX] {
        assert_eq!(
            table.record(&config, HistMetric::BlockTicks, 1, value),
            RtStatus::OK
        );
    }
    assert_eq!(table.count(HistMetric::BlockTicks, 1, 31), 4);
    assert_eq!(table.count(HistMetric::BlockTicks, 1, 30), 0);
}

#[test]
fn t21_histogram_snapshot_totals_match_records() {
    let config = hist_config();
    let table = HistogramTable::new();
    let mut expected = 0u64;
    let mut value = 0u64;
    while value < 200 {
        assert_eq!(
            table.record(&config, HistMetric::BlockTicks, 2, value),
            RtStatus::OK
        );
        expected += 1;
        value += 7;
    }
    let snapshot = table.snapshot(&config);
    assert_eq!(snapshot.total(HistMetric::BlockTicks, 2), expected);
    assert_eq!(snapshot.total(HistMetric::SpinTicks, 2), 0);
    assert_eq!(snapshot.bounds, *config.bounds_all());
}

#[test]
fn t21_histogram_config_validation() {
    assert_eq!(HistogramConfig::uniform(0), Err(RtStatus::ERR_BAD_LAYOUT));
    assert_eq!(
        HistogramConfig::uniform(u64::MAX / 2),
        Err(RtStatus::ERR_CAPACITY)
    );
    let mut non_increasing = [[10u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT];
    assert_eq!(
        HistogramConfig::from_bounds(non_increasing),
        Err(RtStatus::ERR_BAD_LAYOUT)
    );
    non_increasing[0][1] = 5;
    assert_eq!(
        HistogramConfig::from_bounds(non_increasing),
        Err(RtStatus::ERR_BAD_LAYOUT)
    );
    let mut zeros = [[0u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT];
    assert_eq!(
        HistogramConfig::from_bounds(zeros),
        Err(RtStatus::ERR_BAD_LAYOUT)
    );
    zeros[0][0] = 1;
    assert!(HistogramConfig::from_bounds(zeros).is_err());
}

#[test]
fn t21_histogram_record_rejects_invalid_worker() {
    let config = hist_config();
    let table = HistogramTable::new();
    assert_eq!(
        table.record(&config, HistMetric::BlockTicks, 4, 1),
        RtStatus::RT_E_INVARIANT
    );
    assert_eq!(table.count(HistMetric::BlockTicks, 4, 0), 0);
}

#[test]
fn t21_histogram_export_contains_units_bounds_and_ns() {
    let config = hist_config();
    let table = HistogramTable::new();
    assert_eq!(
        table.record(&config, HistMetric::BlockTicks, 0, 9),
        RtStatus::OK
    );
    assert_eq!(
        table.record(&config, HistMetric::BlockTicks, 0, 310),
        RtStatus::OK
    );
    assert_eq!(
        table.record(&config, HistMetric::QueueDepth, 1, 3),
        RtStatus::OK
    );
    let snapshot = table.snapshot(&config);
    let mut text = String::new();
    snapshot
        .write_text(1_000_000, &mut text)
        .expect("hist export must not fail");
    assert!(text.contains("hist.metric=block_ticks unit=ticks buckets=32"));
    assert!(text.contains("hist.metric=queue_depth unit=slots buckets=32"));
    assert!(text.contains("hist.bounds.block_ticks=[10,20,"));
    assert!(text.contains("range_ns="));
    assert!(text.contains("bucket=31"));
}

#[test]
fn t21_ticks_to_ns_conversion() {
    assert_eq!(ticks_to_ns(1_000_000, 1_000_000), 1_000_000_000);
    assert_eq!(ticks_to_ns(1, 0), 0);
    assert_eq!(ticks_to_ns(0, 3_000_000), 0);
    assert_eq!(ticks_to_ns(2_000_000, 1_000_000), 2_000_000_000);
}

// ---------------------------------------------------------------------------
// E. 计数
// ---------------------------------------------------------------------------

#[test]
fn t21_counters_record_and_snapshot() {
    let counters = FaultCounters::new();
    counters.record_block_done();
    counters.record_block_done();
    counters.record_late_block();
    counters.record_cutoff_block();
    counters.record_deadline_gate_miss();
    counters.record_hard_late();
    counters.record_input_overrun();
    counters.record_output_underrun();
    counters.record_nonfinite_in(3);
    counters.record_nonfinite_out(1);
    counters.record_clipped(5);
    counters.record_quarantine_enter();
    counters.set_quarantine_counts(1, 1);
    counters.record_swap();
    counters.record_retire();
    counters.record_duplicate_transition();
    counters.record_fallback_enter();
    counters.record_safety_mute_enter();
    counters.record_safety_mute_exit();
    counters.record_fault_enter();
    counters.record_ack_slot_exhausted();
    counters.set_quarantine_exhausted(2);
    counters.record_worker_lost();
    counters.record_watchdog_mute();
    let snapshot = counters.snapshot();
    assert_eq!(snapshot.blocks_done, 2);
    assert_eq!(snapshot.late_blocks, 1);
    assert_eq!(snapshot.cutoff_blocks, 1);
    assert_eq!(snapshot.deadline_gate_miss, 1);
    assert_eq!(snapshot.hard_late, 1);
    assert_eq!(snapshot.input_overruns, 1);
    assert_eq!(snapshot.output_underruns, 1);
    assert_eq!(snapshot.nonfinite_in, 3);
    assert_eq!(snapshot.nonfinite_out, 1);
    assert_eq!(snapshot.clipped_samples, 5);
    assert_eq!(snapshot.quarantine_entries, 1);
    assert_eq!(snapshot.quarantine_retained, 1);
    assert_eq!(snapshot.swap_count, 1);
    assert_eq!(snapshot.retire_count, 1);
    assert_eq!(snapshot.duplicate_transition_requests, 1);
    assert_eq!(snapshot.fallback_entries, 1);
    assert_eq!(snapshot.safety_mute_entries, 1);
    assert_eq!(snapshot.safety_mute_exits, 1);
    assert_eq!(snapshot.fault_entries, 1);
    assert_eq!(snapshot.ack_slot_exhausted, 1);
    assert_eq!(snapshot.quarantine_exhausted, 2);
    assert_eq!(snapshot.worker_lost, 1);
    assert_eq!(snapshot.watchdog_mutes, 1);
    // 快照是 Copy POD。
    let copy = snapshot;
    assert_eq!(copy, snapshot);
}

#[test]
fn t21_counters_snapshot_text_contains_all_names() {
    let counters = FaultCounters::new();
    counters.record_block_done();
    let mut text = String::new();
    counters
        .snapshot()
        .write_text(&mut text)
        .expect("counters export must not fail");
    assert!(text.contains("counters.blocks_done=1"));
    assert!(text.contains("counters.quarantine_retained=0"));
    assert!(text.contains("counters.watchdog_mutes=0"));
}

// ---------------------------------------------------------------------------
// F. quarantine 池（all-ack/释放/失联/失核）
// ---------------------------------------------------------------------------

fn handle_of(
    pool: &QuarantinePool,
    epoch: u64,
    version: PlanVersion,
    block: u64,
    mask: u32,
) -> QuarantineHandle {
    match pool.enter(epoch, version, block, mask) {
        Ok(handle) => handle,
        Err(status) => panic!("quarantine enter must succeed, got {:?}", status),
    }
}

#[test]
fn t21_quarantine_eight_slots_ninth_refused_and_counted() {
    let pool = QuarantinePool::new();
    let mut handles = [QuarantineHandle { index: 0 }; QUARANTINE_SLOTS];
    let mut index = 0usize;
    while index < QUARANTINE_SLOTS {
        handles[index] = handle_of(&pool, 1, 1, index as u64, 0xF);
        index += 1;
    }
    assert_eq!(pool.retained(), 8);
    assert_eq!(pool.in_use(), 8);
    assert_eq!(
        pool.enter(1, 1, 99, 0xF),
        Err(RtStatus::RT_E_QUARANTINE_FULL)
    );
    assert_eq!(pool.exhausted(), 1);
    // 第九次进入不覆盖任何旧槽。
    assert_eq!(pool.retained(), 8);
    assert!(pool.is_occupied(handles[0]));
    assert_eq!(pool.pending_ack_mask(handles[0]), 0xF);
}

#[test]
fn t21_quarantine_release_before_all_ack_refused() {
    let pool = QuarantinePool::new();
    let handle = handle_of(&pool, 1, 3, 7, 0xF);
    assert_eq!(pool.release(handle), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(pool.retained(), 1);
    assert_eq!(pool.released(), 0);
    assert!(pool.is_occupied(handle));
}

#[test]
fn t21_quarantine_all_ack_then_release_ok_and_slot_reusable() {
    let pool = QuarantinePool::new();
    let handle = handle_of(&pool, 2, 5, 11, 0xF);
    let mut worker = 0u8;
    while worker < RT_WORKERS {
        assert_eq!(pool.note_ack(handle, worker, 2, 5, 11), RtStatus::OK);
        worker += 1;
    }
    assert_eq!(pool.pending_ack_mask(handle), 0);
    assert_eq!(pool.release(handle), RtStatus::OK);
    assert_eq!(pool.retained(), 0);
    assert_eq!(pool.released(), 1);
    assert!(!pool.is_occupied(handle));
    // 释放后槽可复用。
    let reused = handle_of(&pool, 3, 6, 12, 0x1);
    assert!(pool.is_occupied(reused));
    assert_eq!(pool.retained(), 1);
}

#[test]
fn t21_quarantine_label_mismatch_does_not_corrupt_slot() {
    let pool = QuarantinePool::new();
    let handle = handle_of(&pool, 4, 2, 8, 0xF);
    assert_eq!(
        pool.note_ack(handle, 0, 5, 2, 8),
        RtStatus::RT_E_EPOCH_MISMATCH
    );
    assert_eq!(
        pool.note_ack(handle, 0, 4, 3, 8),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert_eq!(pool.note_ack(handle, 0, 4, 2, 9), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(pool.pending_ack_mask(handle), 0xF);
    // 正确标签仍能 ack。
    assert_eq!(pool.note_ack(handle, 0, 4, 2, 8), RtStatus::OK);
    assert_eq!(pool.pending_ack_mask(handle), 0b1110);
}

#[test]
fn t21_quarantine_duplicate_or_unexpected_ack_refused() {
    let pool = QuarantinePool::new();
    // 可能迟到集合只有 worker0/1。
    let handle = handle_of(&pool, 1, 1, 1, 0b0011);
    assert_eq!(pool.note_ack(handle, 2, 1, 1, 1), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(pool.note_ack(handle, 0, 1, 1, 1), RtStatus::OK);
    assert_eq!(pool.note_ack(handle, 0, 1, 1, 1), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(pool.pending_ack_mask(handle), 0b0010);
}

#[test]
fn t21_quarantine_invalid_pending_mask_refused() {
    let pool = QuarantinePool::new();
    assert_eq!(pool.enter(1, 1, 1, 0), Err(RtStatus::RT_E_ABI_RUNTIME));
    assert_eq!(pool.enter(1, 1, 1, 0x10), Err(RtStatus::RT_E_ABI_RUNTIME));
    assert_eq!(pool.retained(), 0);
}

#[test]
fn t21_quarantine_worker_lost_pins_release_forever() {
    let pool = QuarantinePool::new();
    let handle = handle_of(&pool, 1, 1, 1, 0xF);
    assert_eq!(pool.pin_worker_lost(2), RtStatus::OK);
    assert!(pool.release_forbidden());
    // 即使所有 worker 都 ack（含已失联 worker 的迟到记录），也不得释放。
    let mut worker = 0u8;
    while worker < RT_WORKERS {
        let _ = pool.note_ack(handle, worker, 1, 1, 1);
        worker += 1;
    }
    assert_eq!(pool.release(handle), RtStatus::RT_E_WORKER_LOST);
    assert_eq!(pool.release(handle), RtStatus::RT_E_WORKER_LOST);
    assert_eq!(pool.retained(), 1);
    assert_eq!(pool.released(), 0);
    assert!(pool.is_occupied(handle));
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.lost_workers, 0b0100);
    assert_eq!(snapshot.release_forbidden, 1);
    assert_eq!(snapshot.forbid_reason, RtStatus::RT_E_WORKER_LOST.code());
}

#[test]
fn t21_quarantine_core_lost_pins_release() {
    let pool = QuarantinePool::new();
    let handle = handle_of(&pool, 1, 1, 1, 0xF);
    assert_eq!(pool.pin_core_lost(), RtStatus::OK);
    let mut worker = 0u8;
    while worker < RT_WORKERS {
        let _ = pool.note_ack(handle, worker, 1, 1, 1);
        worker += 1;
    }
    assert_eq!(pool.release(handle), RtStatus::RT_E_WORKER_LOST);
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.core_lost, 1);
    assert_eq!(snapshot.retained, 1);
}

#[test]
fn t21_quarantine_snapshot_reflects_state() {
    let pool = QuarantinePool::new();
    let handle = handle_of(&pool, 9, 4, 33, 0b1100);
    assert_eq!(pool.note_ack(handle, 2, 9, 4, 33), RtStatus::OK);
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.entries, 1);
    assert_eq!(snapshot.retained, 1);
    assert!(snapshot.slots[handle.index()].occupied);
    assert_eq!(snapshot.slots[handle.index()].epoch_id, 9);
    assert_eq!(snapshot.slots[handle.index()].plan_version, 4);
    assert_eq!(snapshot.slots[handle.index()].block_seq, 33);
    assert_eq!(snapshot.slots[handle.index()].pending_ack_mask, 0b1000);
}

#[test]
fn t21_quarantine_forbid_release_is_idempotent_first_reason_wins() {
    let pool = QuarantinePool::new();
    assert_eq!(pool.forbid_release(RT_E_ACK_SLOT_EXHAUSTED), RtStatus::OK);
    assert_eq!(
        pool.forbid_release(RtStatus::RT_E_WORKER_LOST),
        RtStatus::OK
    );
    assert_eq!(
        pool.snapshot().forbid_reason,
        RT_E_ACK_SLOT_EXHAUSTED.code()
    );
}

// ---------------------------------------------------------------------------
// G. fallback 预分配 + 准入
// ---------------------------------------------------------------------------

#[test]
fn t21_fallback_admit_ok_and_headroom() {
    let env = env_4w_48k_128();
    let memory = memory_plan(128 * 22 * 4);
    let mut spec = fallback_spec();
    spec.worst_block_ns = 1_000_000;
    let admission = match admit_fallback(&spec, &env, &memory) {
        Ok(admission) => admission,
        Err(status) => panic!("valid fallback must be admitted, got {:?}", status),
    };
    assert!(admission.is_ready());
    assert_eq!(admission.plan_version, 9);
    assert_eq!(admission.graph_id, [0x5A; GRAPH_ID_BYTES]);
    assert_eq!(admission.graph_bytes, memory.graph_pending_bytes);
    assert_eq!(admission.safety_silence_bytes, memory.safety_silence_bytes);
    assert_eq!(
        admission.budget_ns,
        2_666_667 - IO_TAIL_ALLOWANCE_NS_DEFAULT
    );
    assert_eq!(admission.headroom_ns(), admission.budget_ns - 1_000_000);
    assert!(admission.identity_g && admission.p_post_present);
}

#[test]
fn t21_fallback_admit_requires_two_preallocated_graphs() {
    let env = env_4w_48k_128();
    let mut memory = memory_plan(128 * 22 * 4);
    memory.graph_pending_bytes = 0;
    assert_eq!(
        admit_fallback(&fallback_spec(), &env, &memory),
        Err(RtStatus::ERR_CAPACITY)
    );
    memory.graph_pending_bytes = memory.graph_current_bytes - 1;
    assert_eq!(
        admit_fallback(&fallback_spec(), &env, &memory),
        Err(RtStatus::ERR_CAPACITY)
    );
    memory.graph_pending_bytes = memory.graph_current_bytes;
    memory.graph_current_bytes = 0;
    assert_eq!(
        admit_fallback(&fallback_spec(), &env, &memory),
        Err(RtStatus::ERR_CAPACITY)
    );
}

#[test]
fn t21_fallback_admit_requires_preallocated_silence_buffer() {
    let env = env_4w_48k_128();
    let required = 128 * 22 * 4;
    let mut memory = memory_plan(required - 1);
    assert_eq!(
        admit_fallback(&fallback_spec(), &env, &memory),
        Err(RtStatus::ERR_CAPACITY)
    );
    memory.safety_silence_bytes = required;
    assert!(admit_fallback(&fallback_spec(), &env, &memory).is_ok());
}

#[test]
fn t21_fallback_admit_requires_identity_g_with_p_post() {
    let env = env_4w_48k_128();
    let memory = memory_plan(128 * 22 * 4);
    let mut spec = fallback_spec();
    spec.identity_g = false;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_UNSUPPORTED)
    );
    spec.identity_g = true;
    spec.p_post_present = false;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_UNSUPPORTED)
    );
}

#[test]
fn t21_fallback_admit_rejects_matrix_routes() {
    let env = env_4w_48k_128();
    let memory = memory_plan(128 * 22 * 4);
    let mut spec = fallback_spec();
    spec.matrix_routes = 1;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_PATH_TABLE)
    );
}

#[test]
fn t21_fallback_admit_rejects_zero_hash_version_and_bad_channels() {
    let env = env_4w_48k_128();
    let memory = memory_plan(128 * 22 * 4);
    let mut spec = fallback_spec();
    spec.graph_id = [0; GRAPH_ID_BYTES];
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_HASH)
    );
    spec = fallback_spec();
    spec.plan_version = 0;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_BAD_LAYOUT)
    );
    spec = fallback_spec();
    spec.output_channels = 0;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_CHANNELS)
    );
    spec.output_channels = (MAX_OUTPUT_CHANNELS + 1) as u16;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_CHANNELS)
    );
}

#[test]
fn t21_fallback_admit_requires_measured_worst_deadline() {
    let env = env_4w_48k_128();
    let memory = memory_plan(128 * 22 * 4);
    let mut spec = fallback_spec();
    spec.worst_block_ns = 0;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_CAPACITY)
    );
    let budget = fallback_deadline_budget_ns(&env).expect("budget must be valid");
    spec.worst_block_ns = budget;
    assert!(admit_fallback(&spec, &env, &memory).is_ok());
    spec.worst_block_ns = budget + 1;
    assert_eq!(
        admit_fallback(&spec, &env, &memory),
        Err(RtStatus::ERR_CAPACITY)
    );
}

#[test]
fn t21_fallback_deadline_budget_per_rate_and_block() {
    let mut env = env_4w_48k_128();
    assert_eq!(
        fallback_deadline_budget_ns(&env),
        Ok(2_666_667 - IO_TAIL_ALLOWANCE_NS_DEFAULT)
    );
    env.block_size = 64;
    assert_eq!(
        fallback_deadline_budget_ns(&env),
        Ok(1_333_334 - IO_TAIL_ALLOWANCE_NS_DEFAULT)
    );
    env.sample_rate_hz = 96_000;
    env.block_size = 128;
    assert_eq!(
        fallback_deadline_budget_ns(&env),
        Ok(1_333_334 - IO_TAIL_ALLOWANCE_NS_DEFAULT)
    );
    env.sample_rate_hz = 44_100;
    assert_eq!(fallback_deadline_budget_ns(&env), Err(RtStatus::ERR_RATE));
    env.sample_rate_hz = 48_000;
    env.block_size = 256;
    assert_eq!(
        fallback_deadline_budget_ns(&env),
        Err(RtStatus::ERR_BLOCK_SIZE)
    );
}

// ---------------------------------------------------------------------------
// H. 安全状态机
// ---------------------------------------------------------------------------

#[test]
fn t21_safety_state_starts_normal() {
    let state = SafetyState::new();
    assert_eq!(state.mode(), SafetyMode::Normal);
    assert_eq!(state.action(), SafetyAction::Normal);
    assert!(state.block_start_allowed());
    assert_eq!(state.reason(), None);
    assert_eq!(state.transition_key(), None);
    assert_eq!(state.revision(), 0);
}

#[test]
fn t21_safety_worker_requester_is_rejected() {
    let mut state = SafetyState::new();
    let mut events = EventLanes::new();
    let counters = FaultCounters::new();
    let context = SafetyContext {
        fallback_ready: false,
        silence_ready: true,
    };
    let status = state.request(
        SafetyRequest::SilenceMute {
            reason: RtStatus::RT_E_DEVICE_LOST,
            requester: GuardRequester::Worker,
        },
        1,
        1,
        context,
        &mut events,
        &counters,
    );
    assert_eq!(status, RtStatus::RT_E_INVARIANT);
    assert_eq!(state.mode(), SafetyMode::Normal);
    assert_eq!(state.revision(), 0);
    assert!(state.block_start_allowed());
    assert_eq!(events.lane(LANE_CONTROL).expect("lane").readable(), 0);
}

#[test]
fn t21_safety_mute_without_silence_buffer_goes_faulted() {
    // admission 未给安全静音：mute 不可达 → Faulted（§5.5 硬规则）。
    let mut supervisor = FaultSupervisor::new(hist_config());
    let status =
        supervisor.enter_silence_mute(RT_E_ACK_SLOT_EXHAUSTED, GuardRequester::Master, 1, 1);
    assert_eq!(status, RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::Faulted);
    assert_eq!(supervisor.safety.reason(), Some(RT_E_ACK_SLOT_EXHAUSTED));
    assert!(!supervisor.block_start_allowed());
}

#[test]
fn t21_safety_fallback_unavailable_goes_307_mute_or_fault() {
    // 有静音但无 fallback → 进入 mute 且原因 307。
    let mut supervisor = FaultSupervisor::new(hist_config());
    supervisor.set_silence_ready(true);
    assert_eq!(
        supervisor.enter_fallback(RtStatus::RT_E_LATE_STREAK, GuardRequester::Master, 1, 1),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.mode(), SafetyMode::SilenceMute);
    assert_eq!(
        supervisor.safety.reason(),
        Some(RtStatus::RT_E_FALLBACK_UNAVAILABLE)
    );
    // 无静音也无 fallback → Faulted，原因 307。
    let mut bare = FaultSupervisor::new(hist_config());
    assert_eq!(
        bare.enter_fallback(RtStatus::RT_E_LATE_STREAK, GuardRequester::Master, 1, 1),
        RtStatus::OK
    );
    assert_eq!(bare.safety.mode(), SafetyMode::Faulted);
    assert_eq!(
        bare.safety.reason(),
        Some(RtStatus::RT_E_FALLBACK_UNAVAILABLE)
    );
}

#[test]
fn t21_safety_admitted_fallback_enters_protection_fallback() {
    let mut supervisor = supervisor_with_safety_output();
    assert_eq!(
        supervisor.enter_fallback(RtStatus::RT_E_LATE_STREAK, GuardRequester::Master, 3, 4),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.mode(), SafetyMode::ProtectionFallback);
    assert_eq!(supervisor.safe_action(), SafetyAction::ProtectionFallback);
    assert!(supervisor.block_start_allowed());
    assert_eq!(supervisor.safety.fallback_entries(), 1);
    assert_eq!(supervisor.counters.snapshot().fallback_entries, 1);
}

#[test]
fn t21_safety_transition_key_is_deduplicated() {
    let mut supervisor = supervisor_with_safety_output();
    assert_eq!(
        supervisor.enter_silence_mute(RtStatus::RT_E_WATCHDOG, GuardRequester::Watchdog, 5, 2),
        RtStatus::OK
    );
    let readable_after_first = supervisor
        .events
        .lane(LANE_CONTROL)
        .expect("lane")
        .readable();
    assert_eq!(supervisor.safety.revision(), 1);
    // 同 key 再请求 → 去重：无事件、无 revision，只计 duplicate。
    assert_eq!(
        supervisor.enter_silence_mute(RtStatus::RT_E_WATCHDOG, GuardRequester::Watchdog, 5, 2),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.revision(), 1);
    assert_eq!(supervisor.safety.duplicated_requests(), 1);
    assert_eq!(
        supervisor.counters.snapshot().duplicate_transition_requests,
        1
    );
    assert_eq!(
        supervisor
            .events
            .lane(LANE_CONTROL)
            .expect("lane")
            .readable(),
        readable_after_first
    );
    // 不同 epoch 的同目标请求 → 新 key，更新原因但不重复进入事件。
    assert_eq!(
        supervisor.enter_silence_mute(RtStatus::RT_E_WATCHDOG, GuardRequester::Watchdog, 6, 2),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.revision(), 2);
    assert_eq!(
        supervisor
            .events
            .lane(LANE_CONTROL)
            .expect("lane")
            .readable(),
        readable_after_first
    );
}

#[test]
fn t21_safety_escalates_and_ignores_downgrade() {
    let mut supervisor = supervisor_with_safety_output();
    assert_eq!(
        supervisor.enter_silence_mute(RtStatus::RT_E_WATCHDOG, GuardRequester::Watchdog, 1, 1),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.mode(), SafetyMode::SilenceMute);
    assert_eq!(
        supervisor.enter_fault(RtStatus::RT_E_WORKER_LOST, GuardRequester::Control, 1, 1),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.mode(), SafetyMode::Faulted);
    // Faulted 后 mute/fallback 请求被忽略（不降级）。
    assert_eq!(
        supervisor.enter_silence_mute(RtStatus::RT_E_WATCHDOG, GuardRequester::Watchdog, 2, 1),
        RtStatus::OK
    );
    assert_eq!(supervisor.safety.mode(), SafetyMode::Faulted);
    assert_eq!(supervisor.safety.ignored_requests(), 1);
    assert!(!supervisor.block_start_allowed());
    assert_eq!(supervisor.safe_action(), SafetyAction::SilenceMute);
}

#[test]
fn t21_safety_recovery_requires_ticket_and_writes_exit_event() {
    let mut supervisor = supervisor_with_safety_output();
    assert_eq!(
        supervisor.enter_silence_mute(RtStatus::RT_E_WATCHDOG, GuardRequester::Watchdog, 1, 1),
        RtStatus::OK
    );
    assert_eq!(supervisor.counters.snapshot().safety_mute_entries, 1);
    let env = env_4w_48k_128();
    let report = report_for(RT_WORKERS);
    let ticket = match ReAdmissionTicket::new(&report, &env) {
        Ok(ticket) => ticket,
        Err(status) => panic!("ticket must be valid, got {:?}", status),
    };
    assert_eq!(supervisor.recover(&ticket, 2), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::Normal);
    assert!(supervisor.block_start_allowed());
    assert_eq!(supervisor.counters.snapshot().safety_mute_exits, 1);
    assert_eq!(supervisor.safety.mute_exits(), 1);
    // 再次 recovery（已 Normal）→ 状态违例，不做任何事。
    assert_eq!(supervisor.recover(&ticket, 3), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(supervisor.safety.mode(), SafetyMode::Normal);
    // 退出事件在 control lane。
    let mut events = [TspiEventV1::ZERO; 8];
    let count = supervisor.drain_events(&mut events);
    let mut found_exit = false;
    let mut index = 0usize;
    while index < count {
        if events[index].kind == EV_SAFETY_MUTE_EXIT {
            found_exit = true;
            assert_eq!(events[index].lane, LANE_CONTROL);
            assert_eq!(events[index].value_b, ticket.plan_version());
        }
        index += 1;
    }
    assert!(found_exit);
}

#[test]
fn t21_readmission_ticket_validation() {
    let env = env_4w_48k_128();
    let good = report_for(RT_WORKERS);
    assert!(ReAdmissionTicket::new(&good, &env).is_ok());
    let ticket = match ReAdmissionTicket::new(&good, &env) {
        Ok(ticket) => ticket,
        Err(status) => panic!("valid ticket, got {:?}", status),
    };
    assert_eq!(ticket.plan_version(), 7);
    assert_eq!(ticket.workers(), RT_WORKERS);
    assert_eq!(ticket.ack_mask(), 0xF);
    assert_eq!(ticket.mlocked_bytes(), 1 << 20);

    // worker 数不一致 → 119/ERR_PLAN_STALE。
    let mut mismatch = report_for(RT_WORKERS);
    mismatch.workers = RT_WORKERS_FALLBACK;
    assert_eq!(
        ReAdmissionTicket::new(&mismatch, &env),
        Err(RtStatus::ERR_PLAN_STALE)
    );
    // ack_mask 硬写 0xF 错配 expected=2 → 115。
    let env2 = env_2w();
    let mut bad_mask = report_for(RT_WORKERS_FALLBACK);
    bad_mask.ack_mask = 0xF;
    assert_eq!(
        ReAdmissionTicket::new(&bad_mask, &env2),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
    // 无 arena/mlock → 114。
    let mut no_memory = report_for(RT_WORKERS);
    no_memory.arena_bytes = 0;
    assert_eq!(
        ReAdmissionTicket::new(&no_memory, &env),
        Err(RtStatus::ERR_ADMISSION_MEMORY)
    );
    // env expected_workers 与 rt_workers 不一致 → 119。
    let mut bad_env = env_4w_48k_128();
    bad_env.expected_workers = RT_WORKERS_FALLBACK;
    assert_eq!(
        ReAdmissionTicket::new(&good, &bad_env),
        Err(RtStatus::ERR_PLAN_STALE)
    );
}

// ---------------------------------------------------------------------------
// I. supervisor 集成：watchdog→mute、失联/失核、ack 槽耗尽、quarantine full
// ---------------------------------------------------------------------------

#[test]
fn t21_supervisor_service_watchdog_mutes_and_keeps_quarantine() {
    let mut supervisor = supervisor_with_safety_output();
    let block = handle_of(&supervisor.quarantine, 1, 2, 3, 0xF);
    assert_eq!(supervisor.quarantine.retained(), 1);
    let ctl = TspiRtControlV1::new();
    let mut watchdog = watchdog_with_small_thresholds();
    let mut action = WatchdogAction::None;
    let mut index = 0u32;
    while index < 4 {
        action = watchdog.observe(&ctl);
        index += 1;
    }
    assert_eq!(action, WatchdogAction::RequestMute(RtStatus::RT_E_WATCHDOG));
    assert_eq!(supervisor.service_watchdog(action, 1, 2), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::SilenceMute);
    assert!(!supervisor.block_start_allowed());
    // quarantine 原样保留，且释放被禁止（watchdog/安全态下不得回收）。
    assert!(supervisor.quarantine.is_occupied(block));
    assert_eq!(supervisor.quarantine.retained(), 1);
    assert_eq!(supervisor.counters.snapshot().watchdog_mutes, 1);
    // watchdog lane 的 EV_WATCHDOG_MUTE 与 control lane 的 EV_SAFETY_MUTE_ENTER。
    let watchdog_lane = supervisor.events.lane(LANE_WATCHDOG).expect("lane");
    let event = watchdog_lane.try_pop().expect("watchdog event");
    assert_eq!(event.kind, EV_WATCHDOG_MUTE);
    assert_eq!(event.severity, SEVERITY_FATAL);
    let control_lane = supervisor.events.lane(LANE_CONTROL).expect("lane");
    let event = control_lane.try_pop().expect("mute event");
    assert_eq!(event.kind, EV_SAFETY_MUTE_ENTER);
    assert_eq!(event.value_a, RtStatus::RT_E_WATCHDOG.code());
}

#[test]
fn t21_supervisor_worker_lost_pins_quarantine_and_faults() {
    let mut supervisor = supervisor_with_safety_output();
    let block = handle_of(&supervisor.quarantine, 1, 2, 3, 0xF);
    assert_eq!(supervisor.on_worker_lost(2, 1, 2), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::Faulted);
    assert_eq!(supervisor.safety.reason(), Some(RtStatus::RT_E_WORKER_LOST));
    assert!(!supervisor.block_start_allowed());
    assert!(supervisor.quarantine.release_forbidden());
    // all-ack 也不可能让 quarantine 被释放（失联 → 永久保留）。
    let mut worker = 0u8;
    while worker < RT_WORKERS {
        let _ = supervisor.note_late_ack(block, worker, 1, 2, 3);
        worker += 1;
    }
    assert_eq!(
        supervisor.release_quarantine(block, 1),
        RtStatus::RT_E_WORKER_LOST
    );
    assert_eq!(supervisor.quarantine.retained(), 1);
    assert!(supervisor.quarantine.is_occupied(block));
    assert_eq!(supervisor.counters.snapshot().worker_lost, 1);
}

#[test]
fn t21_supervisor_core_lost_faults_and_pins() {
    let dsp_cores = [0u16, 1, 2, 3];
    let mut supervisor = supervisor_with_safety_output();
    assert_eq!(supervisor.on_core_lost(1, &dsp_cores, 1, 1), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::Faulted);
    assert_eq!(supervisor.safety.reason(), Some(RtStatus::RT_E_WORKER_LOST));
    assert!(supervisor.quarantine.release_forbidden());
    // 非 DSP 核（control/IO）丢失 → RT_E_MASTER_LOST。
    let mut other = supervisor_with_safety_output();
    assert_eq!(other.on_core_lost(9, &dsp_cores, 1, 1), RtStatus::OK);
    assert_eq!(other.safety.reason(), Some(RtStatus::RT_E_MASTER_LOST));
}

#[test]
fn t21_supervisor_ack_slot_exhausted_mutes_and_forbids_release() {
    let mut supervisor = supervisor_with_safety_output();
    let block = handle_of(&supervisor.quarantine, 1, 1, 1, 0xF);
    assert_eq!(supervisor.on_ack_slot_exhausted(4, 1, 1), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::SilenceMute);
    assert_eq!(supervisor.safety.reason(), Some(RT_E_ACK_SLOT_EXHAUSTED));
    assert!(!supervisor.block_start_allowed());
    assert!(supervisor.quarantine.release_forbidden());
    assert_eq!(supervisor.quarantine.retained(), 1);
    assert!(supervisor.quarantine.is_occupied(block));
    assert_eq!(supervisor.counters.snapshot().ack_slot_exhausted, 1);
    let mut all = [TspiEventV1::ZERO; 8];
    let count = supervisor.drain_events(&mut all);
    let mut found = false;
    let mut index = 0usize;
    while index < count {
        if all[index].kind == EV_ACK_SLOT_EXHAUSTED {
            found = true;
            assert_eq!(all[index].value_a, 4);
        }
        index += 1;
    }
    assert!(found);
}

#[test]
fn t21_supervisor_quarantine_full_mutes_and_counts() {
    let mut supervisor = supervisor_with_safety_output();
    let mut index = 0usize;
    while index < QUARANTINE_SLOTS {
        let block = match supervisor.quarantine_block(1, 1, index as u64, 0xF) {
            Ok(handle) => handle,
            Err(status) => panic!("slot {} must enter, got {:?}", index, status),
        };
        assert!(supervisor.quarantine.is_occupied(block));
        index += 1;
    }
    assert_eq!(supervisor.quarantine.retained(), 8);
    let result = supervisor.quarantine_block(1, 1, 99, 0xF);
    assert_eq!(result, Err(RtStatus::RT_E_QUARANTINE_FULL));
    assert_eq!(supervisor.safety.mode(), SafetyMode::SilenceMute);
    assert_eq!(
        supervisor.safety.reason(),
        Some(RtStatus::RT_E_QUARANTINE_FULL)
    );
    assert!(supervisor.quarantine.release_forbidden());
    assert_eq!(supervisor.quarantine.retained(), 8);
    assert_eq!(supervisor.counters.snapshot().quarantine_exhausted, 1);
    assert_eq!(supervisor.counters.snapshot().quarantine_retained, 8);
}

#[test]
fn t21_supervisor_late_streak_enters_admitted_fallback() {
    let mut supervisor = supervisor_with_safety_output();
    assert_eq!(supervisor.on_late_streak(1, 1), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::ProtectionFallback);
    assert!(supervisor.block_start_allowed());
    assert_eq!(supervisor.safe_action(), SafetyAction::ProtectionFallback);
}

#[test]
fn t21_supervisor_service_control_request_from_ctl() {
    let mut supervisor = supervisor_with_safety_output();
    let mut ctl = TspiRtControlV1::new();
    ctl.safety_request.store(1, Ordering::Release);
    ctl.safety_reason = RtStatus::RT_E_DEVICE_LOST.0;
    assert_eq!(supervisor.service_control_request(&ctl, 1, 1), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::SilenceMute);
    assert_eq!(supervisor.safety.reason(), Some(RtStatus::RT_E_DEVICE_LOST));
    // 升级为 fault：request=2 覆盖并进入 Faulted。
    ctl.safety_request.store(2, Ordering::Release);
    ctl.safety_reason = RtStatus::RT_E_WORKER_LOST.0;
    assert_eq!(supervisor.service_control_request(&ctl, 1, 1), RtStatus::OK);
    assert_eq!(supervisor.safety.mode(), SafetyMode::Faulted);
    assert_eq!(supervisor.safety.reason(), Some(RtStatus::RT_E_WORKER_LOST));
}

#[test]
fn t21_supervisor_note_swap_and_retire_counters() {
    let supervisor = supervisor_with_safety_output();
    supervisor.note_swap();
    supervisor.note_retire();
    let snapshot = supervisor.counters.snapshot();
    assert_eq!(snapshot.swap_count, 1);
    assert_eq!(snapshot.retire_count, 1);
}

#[test]
fn t21_supervisor_record_paths_fill_hist_and_counters() {
    let supervisor = supervisor_with_safety_output();
    supervisor.on_block_done(0, 9);
    supervisor.on_block_done(1, 19);
    supervisor.on_block_done(2, 310);
    supervisor.on_deadline_miss();
    supervisor.on_late_block();
    supervisor.on_cutoff_block();
    supervisor.on_hard_late();
    supervisor.on_input_overrun();
    supervisor.on_output_underrun();
    supervisor.on_nonfinite_in(2);
    supervisor.on_nonfinite_out(1);
    supervisor.on_clipped(4);
    assert_eq!(supervisor.record_spin_ticks(0, 5), RtStatus::OK);
    assert_eq!(supervisor.record_io_read_ticks(0, 15), RtStatus::OK);
    assert_eq!(supervisor.record_io_write_ticks(1, 25), RtStatus::OK);
    assert_eq!(supervisor.record_queue_depth(1, 2), RtStatus::OK);
    assert_eq!(supervisor.record_phase_gap_ticks(2, 35), RtStatus::OK);
    let counters = supervisor.counters.snapshot();
    assert_eq!(counters.blocks_done, 3);
    assert_eq!(counters.deadline_gate_miss, 1);
    assert_eq!(counters.late_blocks, 1);
    assert_eq!(counters.cutoff_blocks, 1);
    assert_eq!(counters.hard_late, 1);
    assert_eq!(counters.input_overruns, 1);
    assert_eq!(counters.output_underruns, 1);
    assert_eq!(counters.nonfinite_in, 2);
    assert_eq!(counters.nonfinite_out, 1);
    assert_eq!(counters.clipped_samples, 4);
    let snapshot = supervisor.histogram.snapshot(supervisor.histogram_config());
    assert_eq!(snapshot.total(HistMetric::BlockTicks, 0), 1);
    assert_eq!(snapshot.total(HistMetric::BlockTicks, 2), 1);
    assert_eq!(snapshot.total(HistMetric::SpinTicks, 0), 1);
    assert_eq!(snapshot.total(HistMetric::IoWriteTicks, 1), 1);
    assert_eq!(snapshot.total(HistMetric::QueueDepth, 1), 1);
    assert_eq!(snapshot.total(HistMetric::PhaseGapTicks, 2), 1);
}

// ---------------------------------------------------------------------------
// J. 事件环 / 非 RT 导出
// ---------------------------------------------------------------------------

#[test]
fn t21_event_lane_fifo_seq_lane_and_drop() {
    let mut lane = EventRingLane::new(LANE_CAPTURE);
    let mut event = TspiEventV1::ZERO;
    event.kind = EV_WORKER_LOST;
    event.severity = SEVERITY_FATAL;
    let mut index = 0u64;
    while index < EVENT_LANE_SLOTS as u64 {
        assert!(lane.push(&event));
        index += 1;
    }
    assert_eq!(lane.next_seq(), EVENT_LANE_SLOTS as u64);
    assert!(!lane.push(&event));
    assert_eq!(lane.dropped(), 1);
    // 读出顺序 = 写入顺序，且 seq/lane 由写者盖章。
    let mut read = 0u64;
    while read < EVENT_LANE_SLOTS as u64 {
        let record = lane.try_pop().expect("event");
        assert_eq!(record.seq, read);
        assert_eq!(record.lane, LANE_CAPTURE);
        assert_eq!(record.kind, EV_WORKER_LOST);
        read += 1;
    }
    assert!(lane.try_pop().is_none());
    // 读完可继续写。
    assert!(lane.push(&event));
    assert_eq!(lane.next_seq(), EVENT_LANE_SLOTS as u64 + 1);
}

#[test]
fn t21_event_lanes_isolate_and_count_dropped() {
    let mut events = EventLanes::new();
    let mut event = TspiEventV1::ZERO;
    event.lane = LANE_WATCHDOG;
    event.kind = EV_WATCHDOG_MUTE;
    assert!(events.push(&event));
    event.lane = LANE_CONTROL;
    event.kind = EV_SAFETY_MUTE_ENTER;
    assert!(events.push(&event));
    assert_eq!(events.dropped_total(), 0);
    let watchdog_lane = events.lane(LANE_WATCHDOG).expect("lane");
    let record = watchdog_lane.try_pop().expect("event");
    assert_eq!(record.kind, EV_WATCHDOG_MUTE);
    assert_eq!(record.lane, LANE_WATCHDOG);
    let control_lane = events.lane(LANE_CONTROL).expect("lane");
    let record = control_lane.try_pop().expect("event");
    assert_eq!(record.kind, EV_SAFETY_MUTE_ENTER);
    // lane 编号非法 → 不写、返回 false。
    event.lane = 99;
    assert!(!events.push(&event));
}

#[test]
fn t21_export_contains_required_metadata_units_and_safety() {
    let mut supervisor = supervisor_with_safety_output();
    supervisor.on_block_done(0, 123);
    let block = handle_of(&supervisor.quarantine, 1, 7, 42, 0xF);
    assert_eq!(supervisor.on_worker_lost(1, 1, 7), RtStatus::OK);
    assert!(supervisor.quarantine.is_occupied(block));
    let meta = ExportMeta {
        graph_id: [0x12; GRAPH_ID_BYTES],
        plan_version: 7,
        epoch_id: 3,
        window_start_seq: 10,
        window_end_seq: 20,
        tick_freq_hz: 1_000_000,
        sample_rate_hz: GOOD_RATE,
        block_size: GOOD_BLOCK,
        watchdog_core: 6,
        watchdog_stale_samples: 4,
        watchdog_latched: true,
    };
    let mut text = String::new();
    supervisor
        .write_export(&meta, &mut text)
        .expect("export must not fail");
    assert!(text.starts_with("t21-fault-export/v1\n"));
    assert!(text.contains("graph_id=12121212"));
    assert!(text.contains("plan_version=7"));
    assert!(text.contains("epoch_id=3"));
    assert!(text.contains("sample_window.start_seq=10 end_seq=20"));
    assert!(text.contains("sample_rate_hz=48000"));
    assert!(text.contains("block_size=128"));
    assert!(text.contains("tick_freq_hz=1000000"));
    assert!(text.contains("hist.metric=block_ticks unit=ticks buckets=32"));
    assert!(text.contains("hist.bounds.block_ticks=[10,20,"));
    assert!(text.contains("range_ns="));
    assert!(text.contains("counters.blocks_done=1"));
    assert!(text.contains("quarantine.retained=1"));
    assert!(text.contains("quarantine.release_forbidden=1"));
    assert!(text.contains("safety.mode=Faulted"));
    assert!(text.contains("safety.reason=401"));
    assert!(text.contains("safety.block_start_allowed=0"));
    assert!(text.contains("safety.transition_key="));
    assert!(text.contains("fallback.ready=1"));
    assert!(text.contains("fallback.identity_g=1"));
    assert!(text.contains("watchdog.core=6"));
    assert!(text.contains("events.dropped_total=0"));
}

#[test]
fn t21_supervisor_drain_events_returns_quarantine_and_mute_events() {
    let mut supervisor = supervisor_with_safety_output();
    let _ = match supervisor.quarantine_block(4, 5, 6, 0xF) {
        Ok(handle) => handle,
        Err(status) => panic!("quarantine block must enter, got {:?}", status),
    };
    assert_eq!(supervisor.on_late_streak(4, 5), RtStatus::OK);
    let mut events = [TspiEventV1::ZERO; 16];
    let count = supervisor.drain_events(&mut events);
    assert!(count >= 2);
    let mut saw_quarantine = false;
    let mut saw_fallback = false;
    let mut index = 0usize;
    while index < count {
        if events[index].kind == EV_QUARANTINE_ENTER {
            saw_quarantine = true;
            assert_eq!(events[index].lane, LANE_WORKER0);
            assert_eq!(events[index].block_seq, 6);
            assert_eq!(events[index].value_b, 5);
        }
        if events[index].kind == EV_FALLBACK_ENTER {
            saw_fallback = true;
            assert_eq!(events[index].lane, LANE_CONTROL);
            assert_eq!(events[index].value_a, RtStatus::RT_E_LATE_STREAK.code());
        }
        index += 1;
    }
    assert!(saw_quarantine);
    assert!(saw_fallback);
}
