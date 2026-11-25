// T18（admission）离线模型测试：MCL 标志/RLIMIT/预创建栈/双图/控制面预触/mock 失败拒启。
//
// 本文件经 src/admission.rs 末尾的 cfg(test) mod admission_tests { include!(...) } 进入
// 单测构建（cargo 不自动发现 tests/<子目录>；公共注册属 integrator，勿写入
// tests/public_registration.rs）。运行：
//   cargo test -p khaen-rt-host admission
// 全部测试名 t18_ 前缀；离线：只用 MockMemory，不调用真实 mlockall/getrlimit，不碰设备。

use super::*;
use core::mem::needs_drop;
use khaen_core::abi::{
    KhaenDiagFirV1, KhaenPathV1, KhaenPlanHeaderV1, KhaenRoleV1, ABI_VERSION, KHAEN_PLAN_MAGIC,
};
use khaen_core::rt::{
    PlanView, RtStatus, BLOCK_SIZES, MAX_DIAG_FIRS, MAX_INPUT_CHANNELS, MAX_MATRIX_PATHS,
    MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, MAX_WORKER_STACK_BYTES, RT_WORKERS, RT_WORKERS_FALLBACK,
    SAMPLE_RATES_HZ, TAPS_EXTENDED_MAX,
};

const FS: u32 = 48_000;
const BLOCK: u16 = 128;
const N_IN: u16 = 20;
const N_OUT: u16 = 22;
const PATHS: usize = 25;
const DIAGS: usize = 7;
const ROLES: usize = 7;
const PATH_TAPS: u32 = 4096;
const DIAG_TAPS: u32 = 1024;
const COEFF_BYTES: usize = PATHS * PATH_TAPS as usize * 4 + DIAGS * DIAG_TAPS as usize * 4;

// ---------------------------------------------------------------------------
// 夹具：canonical 4.1.2 生产图（5x5 4096 taps + 7 对角 1024 taps + 7 roles）
// ---------------------------------------------------------------------------

struct Fixture {
    header: KhaenPlanHeaderV1,
    paths: Vec<KhaenPathV1>,
    diags: Vec<KhaenDiagFirV1>,
    roles: Vec<KhaenRoleV1>,
    coeffs: Vec<u8>,
}

fn path(in_index: u16, out_index: u16, taps: u32) -> KhaenPathV1 {
    KhaenPathV1 {
        in_index,
        out_index,
        taps,
        delay_samples: 0,
        polarity: 1,
        flags: 0,
        reserved0: 0,
        coeff_offset_bytes: 0,
        coeff_len_bytes: taps * 4,
        reserved1: 0,
    }
}

fn diag(out_index: u16, taps: u32) -> KhaenDiagFirV1 {
    KhaenDiagFirV1 {
        out_index,
        flags: 0,
        taps,
        delay_samples: 0,
        reserved0: 0,
        coeff_offset_bytes: 0,
        coeff_len_bytes: taps * 4,
        reserved1: 0,
    }
}

fn role(role_id: u16, out_index: u16) -> KhaenRoleV1 {
    KhaenRoleV1 {
        role_id,
        out_index,
        flags: 1,
        reserved0: 0,
        delay_samples: 0,
        gain_q15: 32_767,
        polarity: 1,
        reserved1: 0,
        peq_count: 0,
        reserved2: 0,
        name_hash: role_id as u32,
        reserved3: 0,
        reserved4: 0,
    }
}

impl Fixture {
    fn header_for(
        paths: &[KhaenPathV1],
        diags: &[KhaenDiagFirV1],
        roles: &[KhaenRoleV1],
    ) -> KhaenPlanHeaderV1 {
        KhaenPlanHeaderV1 {
            magic: KHAEN_PLAN_MAGIC,
            abi_version: ABI_VERSION,
            header_size: 128,
            plan_version: 7,
            sample_rate: FS,
            block_size: BLOCK,
            n_in: N_IN,
            n_out: N_OUT,
            low_band_count: 5,
            path_count: paths.len() as u16,
            diag_fir_count: diags.len() as u16,
            max_taps: PATH_TAPS,
            workers: RT_WORKERS,
            flags: 0,
            precision: 0,
            reserved0: 0,
            graph_id: [0xAB; 32],
            total_coeff_bytes: COEFF_BYTES as u32,
            path_table_offset: 128,
            diag_table_offset: 0,
            in_stride: 128,
            out_stride: 128,
            role_table_offset: 0,
            role_table_count: roles.len() as u16,
            reserved1: 0,
            affinity_table_offset: 0,
            affinity_count: 0,
            reserved2: 0,
            ctl_meta_offset: 0,
            ctl_meta_len: 0,
            reserved3: [0; 3],
            header_crc32c: 0,
        }
    }

    fn canonical() -> Self {
        let mut paths = Vec::new();
        for out in 0..5u16 {
            for inp in 0..5u16 {
                paths.push(path(inp, out, PATH_TAPS));
            }
        }
        let diags: Vec<KhaenDiagFirV1> = (0..DIAGS as u16).map(|out| diag(out, DIAG_TAPS)).collect();
        let roles: Vec<KhaenRoleV1> = (0..ROLES as u16).map(|id| role(id, id)).collect();
        let header = Self::header_for(&paths, &diags, &roles);
        Self {
            header,
            paths,
            diags,
            roles,
            coeffs: vec![0u8; COEFF_BYTES],
        }
    }

    fn sparse() -> Self {
        let paths: Vec<KhaenPathV1> = Vec::new();
        let diags: Vec<KhaenDiagFirV1> = Vec::new();
        let roles: Vec<KhaenRoleV1> = Vec::new();
        let mut header = Self::header_for(&paths, &diags, &roles);
        header.n_in = 2;
        header.n_out = 2;
        header.low_band_count = 0;
        header.total_coeff_bytes = 0;
        header.max_taps = 0;
        Self {
            header,
            paths,
            diags,
            roles,
            coeffs: Vec::new(),
        }
    }

    fn view(&self) -> PlanView<'_> {
        PlanView {
            header: &self.header,
            paths: &self.paths,
            diags: &self.diags,
            roles: &self.roles,
            coeffs: &self.coeffs,
        }
    }
}

fn base_env() -> AdmissionEnv {
    AdmissionEnv {
        sample_rate_hz: FS,
        block_size: BLOCK,
        rt_workers: RT_WORKERS,
        mlock_limit_bytes: 0,
        preallocated_bytes: 0,
        rt_privilege_ok: true,
        clock_source_ok: true,
        cpu_set_ok: true,
        // T105：v1.1/I15 sched 字段（默认未声明；T45 计划会回写实际值）。
        cpu_set_mask: 0,
        allowed_cpu: [0; 4],
        actual_cpu: [0; 4],
        expected_workers: 0,
        sched_priority: [70; 4],
        irq_dependency_mask: 0,
    }
}

fn canonical_memory() -> MemoryPlan {
    let fixture = Fixture::canonical();
    plan_memory(&fixture.view(), &base_env()).expect("canonical memory plan must compute")
}

fn canonical_aligned_total() -> u64 {
    canonical_memory()
        .aligned_total_bytes(DEFAULT_PAGE_SIZE_BYTES)
        .expect("aligned total")
}

fn admit_result(
    fixture: &Fixture,
    env: &AdmissionEnv,
    backend: &mut MockMemory,
) -> Result<AdmissionReport, RtStatus> {
    admit_with(&fixture.view(), env, backend)
}

fn code_of(result: &Result<AdmissionReport, RtStatus>) -> u32 {
    match result {
        Ok(_) => 0,
        Err(status) => status.code(),
    }
}

// ---------------------------------------------------------------------------
// A. mlock/RLIMIT/栈/控制面/图 常量
// ---------------------------------------------------------------------------

#[test]
fn t18_mlock_flag_values() {
    assert_eq!(MCL_CURRENT, 0x1);
    assert_eq!(MCL_FUTURE, 0x2);
    assert_eq!(MLOCK_ALL_FLAGS, MCL_CURRENT | MCL_FUTURE);
    assert_eq!(MLOCK_ALL_FLAGS, 0x3);
}

#[test]
fn t18_mlock_flags_complete_predicate() {
    assert!(mlock_flags_complete(MLOCK_ALL_FLAGS));
    assert!(mlock_flags_complete(MCL_CURRENT | MCL_FUTURE));
    assert!(!mlock_flags_complete(MCL_CURRENT));
    assert!(!mlock_flags_complete(MCL_FUTURE));
    assert!(!mlock_flags_complete(0));
    // 额外未知位不影响“同时含 CURRENT+FUTURE”判定；调用侧仍固定传 MLOCK_ALL_FLAGS。
    assert!(mlock_flags_complete(MLOCK_ALL_FLAGS | 0x4));
}

#[test]
fn t18_rlimit_constants() {
    // Linux RLIMIT_MEMLOCK = 8（x86_64/aarch64）；RLIM_INFINITY = u64::MAX。
    assert_eq!(RLIMIT_MEMLOCK_RESOURCE, 8);
    assert_eq!(RLIM_INFINITY, u64::MAX);
}

#[test]
fn t18_precreated_threads_and_stacks_budget() {
    assert_eq!(IO_THREADS, 2);
    assert_eq!(WATCHDOG_THREADS, 1);
    assert_eq!(CONTROL_THREADS, 1);
    assert_eq!(precreated_threads(RT_WORKERS), 8);
    assert_eq!(precreated_threads(RT_WORKERS_FALLBACK), 6);
    assert_eq!(precreated_stacks_bytes(RT_WORKERS), 8 * 524_288);
    assert_eq!(precreated_stacks_bytes(RT_WORKERS_FALLBACK), 6 * 524_288);
    assert_eq!(precreated_stacks_bytes(RT_WORKERS), 4_194_304);
    // 每线程栈 = core-v1 §3.3 MAX_WORKER_STACK_BYTES。
    assert_eq!(STACK_BYTES_PER_THREAD, MAX_WORKER_STACK_BYTES as u64);
    assert_eq!(DSP_STACK_BYTES, IO_STACK_BYTES);
    assert_eq!(IO_STACK_BYTES, WATCHDOG_STACK_BYTES);
    assert_eq!(WATCHDOG_STACK_BYTES, CONTROL_STACK_BYTES);
}

#[test]
fn t18_control_plane_capacity_breakdown() {
    // 128 + 64 + 8*64 + 1024*64 + 4*128 + 128 + 64 + 4*64 + 4*64 + 6*4*32*4。
    assert_eq!(control_plane_structs_bytes(), 70_528);
    assert_eq!(CONTROL_RESERVE_BYTES, 65_536);
    assert_eq!(control_plane_total_bytes(), 136_064);
    // 预留必须覆盖控制面结构之外的容量纪律；且至少 64 KiB。
    assert!(CONTROL_RESERVE_BYTES >= 64 * 1024);
}

#[test]
fn t18_stack_budget_rejects_over_limit() {
    // §3.3：每 RT 线程栈上限 512 KiB；超限即拒启（T18 契约，唯一拒绝码 114）。
    assert_eq!(STACK_BYTES_PER_THREAD, 524_288);
    let over = STACK_BYTES_PER_THREAD + 1;
    assert!(over > MAX_WORKER_STACK_BYTES as u64);
    assert_eq!(MAX_WORKER_STACK_BYTES, 524_288);
}

#[test]
fn t18_graph_capacity_formulas() {
    assert_eq!(graph_scratch_bytes(128, 4), 16_384);
    assert_eq!(graph_scratch_bytes(64, 4), 8_192);
    assert_eq!(graph_scratch_bytes(128, 2), 8_192);
    assert_eq!(path_history_bytes(4096, 128), 65_536);
    assert_eq!(path_history_bytes(4096, 64), 65_536); // Q*FFT_SIZE 恒定 = 2*taps。
    assert_eq!(path_history_bytes(1024, 128), 16_384);
    assert_eq!(path_history_bytes(0, 128), 0);
}

// ---------------------------------------------------------------------------
// B. 算术原语与预触原语
// ---------------------------------------------------------------------------

#[test]
fn t18_align_up_boundaries() {
    assert_eq!(align_up(0, 4096).unwrap(), 0);
    assert_eq!(align_up(1, 4096).unwrap(), 4096);
    assert_eq!(align_up(4095, 4096).unwrap(), 4096);
    assert_eq!(align_up(4096, 4096).unwrap(), 4096);
    assert_eq!(align_up(4097, 4096).unwrap(), 8192);
    assert_eq!(align_up(65, 64).unwrap(), 128);
    assert_eq!(align_up(64, 64).unwrap(), 64);
}

#[test]
fn t18_align_up_rejects_zero_align() {
    assert_eq!(align_up(1, 0), Err(RtStatus::ERR_BAD_LAYOUT));
    assert_eq!(pages_for(1, 0), Err(RtStatus::ERR_BAD_LAYOUT));
}

#[test]
fn t18_align_up_overflow_is_capacity() {
    assert_eq!(align_up(u64::MAX, 4096), Err(RtStatus::ERR_CAPACITY));
    assert_eq!(pages_for(u64::MAX, 4096), Ok(u64::MAX / 4096 + 1));
}

#[test]
fn t18_pages_for_boundaries() {
    assert_eq!(pages_for(0, 4096).unwrap(), 0);
    assert_eq!(pages_for(1, 4096).unwrap(), 1);
    assert_eq!(pages_for(4096, 4096).unwrap(), 1);
    assert_eq!(pages_for(4097, 4096).unwrap(), 2);
    assert_eq!(pages_for(8 * 1024, 1024).unwrap(), 8);
}

#[test]
fn t18_pre_touch_bytes_touches_each_page() {
    let page = 1024u64;
    let mut buf = vec![0u8; 3 * 1024 + 100];
    let touched = pre_touch_bytes(buf.as_mut_slice(), page);
    assert_eq!(touched, 4);
    // 每页首字节被写触为 0xA5；最后一页内 100 字节也覆盖。
    for offset in [0usize, 1024, 2048, 3072] {
        assert_eq!(buf[offset], 0xA5);
    }
    // 非页首字节不被写触（差异可观察）。
    assert_eq!(buf[1], 0);
    assert_eq!(buf[1023], 0);
}

#[test]
fn t18_pre_touch_bytes_handles_empty_and_zero_page() {
    let mut empty: [u8; 0] = [];
    assert_eq!(pre_touch_bytes(&mut empty, 4096), 0);
    let mut buf = [0u8; 16];
    assert_eq!(pre_touch_bytes(&mut buf, 0), 0);
    assert_eq!(buf, [0u8; 16]);
}

// ---------------------------------------------------------------------------
// C. plan_memory 容量模型
// ---------------------------------------------------------------------------

#[test]
fn t18_memory_plan_canonical_components() {
    let memory = canonical_memory();
    assert_eq!(memory.capture_payload_bytes, 40_960);
    assert_eq!(memory.playback_payload_bytes, 45_056);
    assert_eq!(memory.worker_partial_bytes, 45_056);
    assert_eq!(memory.quarantine_bytes, 90_112);
    assert_eq!(memory.safety_silence_bytes, 11_264);
    assert_eq!(memory.desc_bytes, 512);
    assert_eq!(memory.arena_bytes, 232_960);
    assert_eq!(memory.graph_scratch_bytes, 16_384);
    assert_eq!(memory.graph_current_bytes, 2_209_216);
    assert_eq!(memory.graph_pending_bytes, 2_209_216);
    assert_eq!(memory.stacks_bytes, 4_194_304);
    assert_eq!(memory.control_plane_bytes, 136_064);
    assert_eq!(memory.path_count, PATHS as u32);
    assert_eq!(memory.diag_count, DIAGS as u32);
}

#[test]
fn t18_memory_plan_canonical_totals() {
    let memory = canonical_memory();
    let components = memory.arena_bytes
        + memory.graph_current_bytes
        + memory.graph_pending_bytes
        + memory.stacks_bytes
        + memory.control_plane_bytes;
    assert_eq!(memory.total_bytes, components);
    assert_eq!(memory.total_bytes, 8_981_760);
    assert_eq!(memory.graphs_bytes(), 4_418_432);
    assert_eq!(memory.aligned_total_bytes(4096).unwrap(), 8_990_720);
    assert!(memory.aligned_total_bytes(4096).unwrap() > memory.total_bytes);
}

#[test]
fn t18_memory_plan_block64_halves_payloads_keeps_history() {
    let mut fixture = Fixture::canonical();
    fixture.header.block_size = 64;
    let mut env = base_env();
    env.block_size = 64;
    let memory = plan_memory(&fixture.view(), &env).unwrap();
    assert_eq!(memory.capture_payload_bytes, 20_480);
    assert_eq!(memory.playback_payload_bytes, 22_528);
    assert_eq!(memory.worker_partial_bytes, 22_528);
    assert_eq!(memory.quarantine_bytes, 45_056);
    assert_eq!(memory.safety_silence_bytes, 5_632);
    assert_eq!(memory.arena_bytes, 116_736);
    assert_eq!(memory.graph_scratch_bytes, 8_192);
    // 频域历史 Q*FFT_SIZE 与 B 无关：B 减半不省历史。
    assert_eq!(memory.graph_current_bytes, 2_201_024);
    assert_eq!(memory.total_bytes, 8_849_152);
}

#[test]
fn t18_memory_plan_two_worker_profile() {
    let mut fixture = Fixture::canonical();
    fixture.header.workers = RT_WORKERS_FALLBACK;
    let mut env = base_env();
    env.rt_workers = RT_WORKERS_FALLBACK;
    let memory = plan_memory(&fixture.view(), &env).unwrap();
    assert_eq!(memory.worker_partial_bytes, 22_528);
    assert_eq!(memory.graph_scratch_bytes, 8_192);
    assert_eq!(memory.arena_bytes, 210_432);
    assert_eq!(memory.graph_current_bytes, 2_201_024);
    assert_eq!(memory.stacks_bytes, 6 * 524_288);
    assert_eq!(memory.control_plane_bytes, 136_064);
    assert_eq!(memory.total_bytes, 7_894_272);
}

#[test]
fn t18_memory_plan_96k_same_memory() {
    let fixture_48 = Fixture::canonical();
    let mut fixture_96 = Fixture::canonical();
    fixture_96.header.sample_rate = 96_000;
    let mut env_96 = base_env();
    env_96.sample_rate_hz = 96_000;
    let memory_48 = plan_memory(&fixture_48.view(), &base_env()).unwrap();
    let memory_96 = plan_memory(&fixture_96.view(), &env_96).unwrap();
    // 采样率不影响内存容量模型（B/通道/taps 才是输入）。
    assert_eq!(memory_48, memory_96);
}

#[test]
fn t18_memory_plan_counts_tables_and_ctl_meta() {
    let fixture = Fixture::canonical();
    let base = plan_memory(&fixture.view(), &base_env()).unwrap();
    let mut with_meta = Fixture::canonical();
    with_meta.header.ctl_meta_len = 256;
    let with_meta_memory = plan_memory(&with_meta.view(), &base_env()).unwrap();
    assert_eq!(
        with_meta_memory.graph_current_bytes,
        base.graph_current_bytes + 256
    );
    // 单图至少包含 header 128B + path/diag/role 表 + 系数 + scratch。
    assert!(base.graph_current_bytes > 128 + 25 * 32 + 7 * 32 + 7 * 32);
    assert_eq!(PATH_ENTRY_BYTES, 32);
    assert_eq!(DIAG_ENTRY_BYTES, 32);
    assert_eq!(ROLE_ENTRY_BYTES, 32);
}

#[test]
fn t18_memory_plan_coeff_failsafe_uses_declared_max() {
    let mut fixture = Fixture::canonical();
    let base = plan_memory(&fixture.view(), &base_env()).unwrap();
    fixture.header.total_coeff_bytes = COEFF_BYTES as u32 + 128;
    let declared = plan_memory(&fixture.view(), &base_env()).unwrap();
    assert_eq!(declared.graph_current_bytes, base.graph_current_bytes + 128);
}

#[test]
fn t18_memory_plan_sparse_graph() {
    let fixture = Fixture::sparse();
    let memory = plan_memory(&fixture.view(), &base_env()).unwrap();
    assert_eq!(memory.capture_payload_bytes, 4_096);
    assert_eq!(memory.playback_payload_bytes, 4_096);
    assert_eq!(memory.worker_partial_bytes, 4_096);
    assert_eq!(memory.quarantine_bytes, 8_192);
    assert_eq!(memory.safety_silence_bytes, 1_024);
    assert_eq!(memory.arena_bytes, 22_016);
    assert_eq!(memory.graph_current_bytes, 16_512);
    assert_eq!(memory.total_bytes, 4_385_408);
    assert_eq!(memory.path_count, 0);
    assert_eq!(memory.diag_count, 0);
}

#[test]
fn t18_memory_plan_region_bytes_and_aligned() {
    let memory = canonical_memory();
    assert_eq!(memory.region_bytes(RegionId::Arena), 232_960);
    assert_eq!(memory.region_bytes(RegionId::GraphCurrent), 2_209_216);
    assert_eq!(memory.region_bytes(RegionId::GraphPending), 2_209_216);
    assert_eq!(memory.region_bytes(RegionId::Stacks), 4_194_304);
    assert_eq!(memory.region_bytes(RegionId::ControlPlane), 136_064);
    assert_eq!(
        memory.aligned_region_bytes(RegionId::Arena, 4096).unwrap(),
        233_472
    );
    assert_eq!(
        memory
            .aligned_region_bytes(RegionId::GraphCurrent, 4096)
            .unwrap(),
        2_211_840
    );
    assert_eq!(
        memory.aligned_region_bytes(RegionId::Stacks, 4096).unwrap(),
        4_194_304
    );
    assert_eq!(
        memory
            .aligned_region_bytes(RegionId::ControlPlane, 4096)
            .unwrap(),
        139_264
    );
    let mut sum = 0u64;
    for region in RegionId::ALL {
        let aligned = memory.aligned_region_bytes(region, 4096).unwrap();
        assert_eq!(aligned % 4096, 0);
        sum += aligned;
    }
    assert_eq!(sum, memory.aligned_total_bytes(4096).unwrap());
}

// ---------------------------------------------------------------------------
// D. validate：env 范围 / plan-env 一致性 / 容量上限
// ---------------------------------------------------------------------------

#[test]
fn t18_validate_rejects_rate() {
    let fixture = Fixture::canonical();
    for bad in [0u32, 1, 44_100, 88_200, 192_000] {
        let mut env = base_env();
        env.sample_rate_hz = bad;
        assert_eq!(
            validate(&fixture.view(), &env),
            Err(RtStatus::ERR_RATE),
            "rate {bad}"
        );
    }
}

#[test]
fn t18_validate_rejects_block() {
    let fixture = Fixture::canonical();
    for bad in [0u16, 1, 32, 256, 512, 1024] {
        let mut env = base_env();
        env.block_size = bad;
        assert_eq!(
            validate(&fixture.view(), &env),
            Err(RtStatus::ERR_BLOCK_SIZE),
            "block {bad}"
        );
    }
}

#[test]
fn t18_validate_rejects_workers() {
    let fixture = Fixture::canonical();
    for bad in [0u8, 1, 3, 5, 8] {
        let mut env = base_env();
        env.rt_workers = bad;
        assert_eq!(
            validate(&fixture.view(), &env),
            Err(RtStatus::ERR_ADMISSION_SCHED),
            "workers {bad}"
        );
    }
}

#[test]
fn t18_validate_rejects_missing_rt_privilege() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.rt_privilege_ok = false;
    assert_eq!(
        validate(&fixture.view(), &env),
        Err(RtStatus::ERR_ADMISSION_MEMORY)
    );
}

#[test]
fn t18_validate_rejects_missing_clock() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.clock_source_ok = false;
    assert_eq!(
        validate(&fixture.view(), &env),
        Err(RtStatus::ERR_ADMISSION_CLOCK)
    );
}

#[test]
fn t18_validate_rejects_missing_cpu_set() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.cpu_set_ok = false;
    assert_eq!(
        validate(&fixture.view(), &env),
        Err(RtStatus::ERR_ADMISSION_SCHED)
    );
}

#[test]
fn t18_validate_rejects_plan_rate_mismatch() {
    let mut fixture = Fixture::canonical();
    fixture.header.sample_rate = 96_000; // env 仍 48k。
    assert_eq!(
        validate(&fixture.view(), &base_env()),
        Err(RtStatus::ERR_RATE)
    );
    // 两者一致但都不是合法值也一样拒绝（plan 侧范围检查）。
    let mut bad = Fixture::canonical();
    bad.header.sample_rate = 44_100;
    let mut env = base_env();
    env.sample_rate_hz = 44_100;
    assert_eq!(validate(&bad.view(), &env), Err(RtStatus::ERR_RATE));
}

#[test]
fn t18_validate_rejects_plan_block_mismatch() {
    let mut fixture = Fixture::canonical();
    fixture.header.block_size = 64;
    assert_eq!(
        validate(&fixture.view(), &base_env()),
        Err(RtStatus::ERR_BLOCK_SIZE)
    );
}

#[test]
fn t18_validate_rejects_plan_worker_stale() {
    let mut fixture = Fixture::canonical();
    fixture.header.workers = RT_WORKERS_FALLBACK;
    assert_eq!(
        validate(&fixture.view(), &base_env()),
        Err(RtStatus::ERR_PLAN_STALE)
    );
    // 2 核独立 profile：env/plan 都是 2 时校验通过。
    let mut env = base_env();
    env.rt_workers = RT_WORKERS_FALLBACK;
    assert_eq!(validate(&fixture.view(), &env), Ok(()));
    // header 未声明（0）也视为不一致 → stale（不静默当 4）。
    let mut zero = Fixture::canonical();
    zero.header.workers = 0;
    assert_eq!(
        validate(&zero.view(), &base_env()),
        Err(RtStatus::ERR_PLAN_STALE)
    );
}

#[test]
fn t18_validate_rejects_precision() {
    let mut fixture = Fixture::canonical();
    fixture.header.precision = 1; // f64 仅 reference/设计器，runtime 只收 f32。
    assert_eq!(
        validate(&fixture.view(), &base_env()),
        Err(RtStatus::ERR_PRECISION)
    );
}

#[test]
fn t18_validate_rejects_channels() {
    let mut zero_in = Fixture::canonical();
    zero_in.header.n_in = 0;
    assert_eq!(
        validate(&zero_in.view(), &base_env()),
        Err(RtStatus::ERR_CHANNELS)
    );
    let mut zero_out = Fixture::canonical();
    zero_out.header.n_out = 0;
    assert_eq!(
        validate(&zero_out.view(), &base_env()),
        Err(RtStatus::ERR_CHANNELS)
    );
    let mut too_many_in = Fixture::canonical();
    too_many_in.header.n_in = MAX_INPUT_CHANNELS as u16 + 1;
    assert_eq!(
        validate(&too_many_in.view(), &base_env()),
        Err(RtStatus::ERR_CHANNELS)
    );
    let mut too_many_out = Fixture::canonical();
    too_many_out.header.n_out = MAX_OUTPUT_CHANNELS as u16 + 1;
    assert_eq!(
        validate(&too_many_out.view(), &base_env()),
        Err(RtStatus::ERR_CHANNELS)
    );
}

#[test]
fn t18_validate_rejects_oversize_tables() {
    let mut many_paths = Fixture::canonical();
    while many_paths.paths.len() <= MAX_MATRIX_PATHS {
        many_paths.paths.push(path(0, 0, 64));
    }
    many_paths.header.path_count = many_paths.paths.len() as u16;
    assert_eq!(
        validate(&many_paths.view(), &base_env()),
        Err(RtStatus::ERR_CAPACITY)
    );

    let mut many_diags = Fixture::canonical();
    while many_diags.diags.len() <= MAX_DIAG_FIRS {
        many_diags.diags.push(diag(0, 64));
    }
    many_diags.header.diag_fir_count = many_diags.diags.len() as u16;
    assert_eq!(
        validate(&many_diags.view(), &base_env()),
        Err(RtStatus::ERR_CAPACITY)
    );

    let mut many_roles = Fixture::canonical();
    while many_roles.roles.len() <= MAX_OUTPUT_ROLES {
        many_roles.roles.push(role(0, 0));
    }
    many_roles.header.role_table_count = many_roles.roles.len() as u16;
    assert_eq!(
        validate(&many_roles.view(), &base_env()),
        Err(RtStatus::ERR_ROLES)
    );
}

#[test]
fn t18_validate_rejects_taps_over_extended() {
    let mut fixture = Fixture::canonical();
    fixture.header.max_taps = TAPS_EXTENDED_MAX + 1;
    assert_eq!(
        validate(&fixture.view(), &base_env()),
        Err(RtStatus::ERR_TAPS)
    );
    // 边界值 16384 允许（是否通过 extended 预算由 T56/T45 判定，不在 T18 重复）。
    fixture.header.max_taps = TAPS_EXTENDED_MAX;
    assert_eq!(validate(&fixture.view(), &base_env()), Ok(()));
}

#[test]
fn t18_validate_accepts_canonical() {
    let fixture = Fixture::canonical();
    assert_eq!(validate(&fixture.view(), &base_env()), Ok(()));
    // 96k / 64 / 2-worker 边界组合也应通过纯校验。
    let mut env = base_env();
    env.sample_rate_hz = 96_000;
    let mut fixture_96 = Fixture::canonical();
    fixture_96.header.sample_rate = 96_000;
    assert_eq!(validate(&fixture_96.view(), &env), Ok(()));
    assert!(SAMPLE_RATES_HZ.contains(&env.sample_rate_hz));
    assert!(BLOCK_SIZES.contains(&env.block_size));
}

// ---------------------------------------------------------------------------
// E. admit_with 成功路径与报告
// ---------------------------------------------------------------------------

#[test]
fn t18_admit_ok_returns_report_fields() {
    let fixture = Fixture::canonical();
    let env = base_env();
    let mut backend = MockMemory::new();
    let report = admit_result(&fixture, &env, &mut backend).expect("admission must pass with mock");
    assert_eq!(report.plan_version, 7);
    assert_eq!(report.arena_bytes, 233_472);
    assert_eq!(report.mlocked_bytes, 8_990_720);
    assert_eq!(report.stack_bytes, 4_194_304);
    assert_eq!(report.workers, RT_WORKERS);
    assert_eq!(report.graph_id, [0xAB; 32]);
    assert_eq!(report.mlocked_bytes, canonical_aligned_total());
}

#[test]
fn t18_admit_ok_mlock_called_once_with_both_flags() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new();
    let _ = admit_result(&fixture, &base_env(), &mut backend);
    assert_eq!(backend.mlock_calls(), 1);
    assert_eq!(backend.mlock_flags(), MLOCK_ALL_FLAGS);
    assert!(mlock_flags_complete(backend.mlock_flags()));
    // env 未声明 RLIMIT 时后端读取一次。
    assert_eq!(backend.read_limit_calls(), 1);
}

#[test]
fn t18_admit_ok_pretouches_all_regions_in_order() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new();
    let report = admit_result(&fixture, &base_env(), &mut backend).expect("admission must pass");
    assert_eq!(backend.pretouch_calls(), RegionId::ALL.len() as u32);
    let memory = canonical_memory();
    for (index, region) in RegionId::ALL.iter().enumerate() {
        let call = backend
            .pretouch_call(index)
            .expect("pretouch call recorded");
        assert_eq!(call.region, *region as u8, "region order at {index}");
        assert_eq!(
            call.bytes,
            memory.aligned_region_bytes(*region, 4096).unwrap()
        );
        assert_eq!(call.bytes % 4096, 0);
    }
    assert_eq!(backend.touched_bytes(), report.mlocked_bytes);
}

#[test]
fn t18_admit_ok_pretouch_bytes_page_aligned_and_sum() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new();
    let report = admit_result(&fixture, &base_env(), &mut backend).expect("admission must pass");
    let mut sum = 0u64;
    for index in 0..backend.pretouch_calls() as usize {
        sum += backend.pretouch_call(index).unwrap().bytes;
    }
    assert_eq!(sum, report.mlocked_bytes);
    assert_eq!(backend.touched_bytes(), report.mlocked_bytes);
}

#[test]
fn t18_admit_ok_report_copy_and_no_drop() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<AdmissionReport>();
    assert!(!needs_drop::<AdmissionReport>());
    assert!(!needs_drop::<AdmissionEnv>());
    assert!(!needs_drop::<MemoryPlan>());
    assert!(!needs_drop::<MockMemory>());
    assert!(!needs_drop::<RegionId>());
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new();
    let report = admit_result(&fixture, &base_env(), &mut backend).unwrap();
    let copied = report;
    assert_eq!(copied, report);
}

#[test]
fn t18_admit_ok_retry_after_failure() {
    let fixture = Fixture::canonical();
    let env = base_env();
    let mut failing = MockMemory::new().failing_mlock(RtStatus::ERR_ADMISSION_MEMORY.code());
    assert!(admit_result(&fixture, &env, &mut failing).is_err());
    let mut retry = MockMemory::new();
    let report = admit_result(&fixture, &env, &mut retry).expect("retry with fixed backend passes");
    assert_eq!(report.mlocked_bytes, 8_990_720);
}

// ---------------------------------------------------------------------------
// F. 预算 / RLIMIT 判定
// ---------------------------------------------------------------------------

#[test]
fn t18_admit_declared_budget_below_requirement_is_capacity() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.preallocated_bytes = canonical_aligned_total() - 1;
    let mut backend = MockMemory::new();
    let result = admit_result(&fixture, &env, &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_CAPACITY.code());
    // 容量判定在 mlock/RLIMIT 之前：无系统副作用。
    assert_eq!(backend.mlock_calls(), 0);
    assert_eq!(backend.read_limit_calls(), 0);
    assert_eq!(backend.pretouch_calls(), 0);
}

#[test]
fn t18_admit_declared_budget_exact_passes() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.preallocated_bytes = canonical_aligned_total();
    let mut backend = MockMemory::new();
    let report = admit_result(&fixture, &env, &mut backend).expect("exact declared budget passes");
    assert_eq!(report.mlocked_bytes, canonical_aligned_total());
    assert_eq!(backend.mlock_calls(), 1);
}

#[test]
fn t18_admit_declared_budget_over_limit_fails() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.preallocated_bytes = canonical_aligned_total() + 4096;
    env.mlock_limit_bytes = canonical_aligned_total();
    let mut backend = MockMemory::new();
    let result = admit_result(&fixture, &env, &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
    assert_eq!(backend.mlock_calls(), 0);
}

#[test]
fn t18_admit_memlock_zero_refuses() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new().with_memlock_limit(0);
    let result = admit_result(&fixture, &base_env(), &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
    assert_eq!(backend.read_limit_calls(), 1);
    assert_eq!(backend.mlock_calls(), 0);
    assert_eq!(backend.pretouch_calls(), 0);
}

#[test]
fn t18_admit_read_limit_error_refuses() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new().failing_read_limit(RtStatus::ERR_ADMISSION_MEMORY.code());
    let result = admit_result(&fixture, &base_env(), &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
    assert_eq!(backend.mlock_calls(), 0);
}

#[test]
fn t18_admit_env_limit_short_refuses() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.mlock_limit_bytes = canonical_aligned_total() - 4096;
    let mut backend = MockMemory::new();
    let result = admit_result(&fixture, &env, &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
    assert_eq!(backend.read_limit_calls(), 0); // env 已带探测值，不再读后端。
    assert_eq!(backend.mlock_calls(), 0);
}

#[test]
fn t18_admit_env_limit_ok_passes() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.mlock_limit_bytes = canonical_aligned_total();
    let mut backend = MockMemory::new();
    let report =
        admit_result(&fixture, &env, &mut backend).expect("limit equal to requirement passes");
    assert_eq!(report.mlocked_bytes, canonical_aligned_total());
}

#[test]
fn t18_admit_infinite_limit_passes() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.mlock_limit_bytes = RLIM_INFINITY;
    let mut backend = MockMemory::new().with_memlock_limit(0); // env 优先，不读后端。
    let report = admit_result(&fixture, &env, &mut backend).expect("infinite limit passes");
    assert_eq!(report.workers, RT_WORKERS);
}

#[test]
fn t18_admit_bad_page_size_refuses() {
    let fixture = Fixture::canonical();
    for bad_page in [0u64, 1, 3000, 12_288] {
        let mut backend = MockMemory::new().with_page_size(bad_page);
        let result = admit_result(&fixture, &base_env(), &mut backend);
        assert_eq!(
            code_of(&result),
            RtStatus::ERR_ADMISSION_MEMORY.code(),
            "page {bad_page}"
        );
        assert_eq!(
            backend.mlock_calls(),
            0,
            "page {bad_page} must fail before mlock"
        );
        assert_eq!(backend.read_limit_calls(), 0);
    }
}

// ---------------------------------------------------------------------------
// G. mock 失败拒启与执行顺序
// ---------------------------------------------------------------------------

#[test]
fn t18_admit_validation_failure_makes_no_backend_calls() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.rt_privilege_ok = false;
    let mut backend = MockMemory::new();
    let result = admit_result(&fixture, &env, &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
    assert_eq!(backend.mlock_calls(), 0);
    assert_eq!(backend.read_limit_calls(), 0);
    assert_eq!(backend.pretouch_calls(), 0);
}

#[test]
fn t18_admit_mlock_failure_is_refused() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new().failing_mlock(RtStatus::ERR_ADMISSION_MEMORY.code());
    let result = admit_result(&fixture, &base_env(), &mut backend);
    assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
}

#[test]
fn t18_admit_mlock_failure_skips_pretouch() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new().failing_mlock(RtStatus::ERR_ADMISSION_MEMORY.code());
    let result = admit_result(&fixture, &base_env(), &mut backend);
    assert!(result.is_err());
    assert_eq!(backend.pretouch_calls(), 0);
}

#[test]
fn t18_admit_mlock_backend_code_is_normalized() {
    // 后端返回任意码都归一为 114（任务动作是拒启，不是透传实现细节）。
    let fixture = Fixture::canonical();
    for code in [RtStatus::ERR_ADMISSION_MEMORY.code(), 5, 12, 999] {
        let mut backend = MockMemory::new().failing_mlock(code);
        let result = admit_result(&fixture, &base_env(), &mut backend);
        assert_eq!(code_of(&result), RtStatus::ERR_ADMISSION_MEMORY.code());
    }
}

#[test]
fn t18_admit_pretouch_failure_at_index_refused() {
    let fixture = Fixture::canonical();
    for index in 0..RegionId::ALL.len() as u32 {
        let mut backend = MockMemory::new().failing_pretouch_at(index);
        let result = admit_result(&fixture, &base_env(), &mut backend);
        assert_eq!(
            code_of(&result),
            RtStatus::ERR_ADMISSION_MEMORY.code(),
            "fail at {index}"
        );
        assert_eq!(backend.mlock_calls(), 1);
        assert_eq!(
            backend.pretouch_calls(),
            index + 1,
            "stops at failing region"
        );
    }
}

#[test]
fn t18_admit_pretouch_failure_region_refused() {
    let fixture = Fixture::canonical();
    for region in RegionId::ALL {
        let mut backend = MockMemory::new().failing_pretouch_region(region);
        let result = admit_result(&fixture, &base_env(), &mut backend);
        assert_eq!(
            code_of(&result),
            RtStatus::ERR_ADMISSION_MEMORY.code(),
            "fail region {}",
            region.name()
        );
        // 失败的 region 之后不再预触。
        let index = RegionId::ALL
            .iter()
            .position(|item| *item == region)
            .unwrap() as u32;
        assert_eq!(backend.pretouch_calls(), index + 1);
    }
}

#[test]
fn t18_admit_pretouch_failure_after_other_regions_stops() {
    let fixture = Fixture::canonical();
    let mut backend = MockMemory::new().failing_pretouch_at(3); // Stacks 失败。
    let result = admit_result(&fixture, &base_env(), &mut backend);
    assert!(result.is_err());
    assert_eq!(backend.pretouch_calls(), 4);
    // 前三个区域按顺序记录、均成功；失败区域也记录（第 4 次）。
    assert_eq!(
        backend.pretouch_call(0).unwrap().region,
        RegionId::Arena as u8
    );
    assert_eq!(
        backend.pretouch_call(1).unwrap().region,
        RegionId::GraphCurrent as u8
    );
    assert_eq!(
        backend.pretouch_call(2).unwrap().region,
        RegionId::GraphPending as u8
    );
    assert_eq!(
        backend.pretouch_call(3).unwrap().region,
        RegionId::Stacks as u8
    );
    assert_eq!(backend.pretouch_call(4), None);
}

#[test]
fn t18_admit_no_partial_success_report_on_any_failure() {
    let fixture = Fixture::canonical();
    let failures: Vec<MockMemory> = vec![
        MockMemory::new().failing_read_limit(114),
        MockMemory::new().with_memlock_limit(0),
        MockMemory::new().failing_mlock(114),
        MockMemory::new().failing_pretouch_at(0),
        MockMemory::new().failing_pretouch_region(RegionId::ControlPlane),
        MockMemory::new().with_page_size(0),
    ];
    for mut backend in failures {
        let result = admit_result(&fixture, &base_env(), &mut backend);
        assert!(
            result.is_err(),
            "mock failure must never produce an AdmissionReport"
        );
    }
}

// ---------------------------------------------------------------------------
// H. 构建门 / 平台后端
// ---------------------------------------------------------------------------

#[test]
fn t18_production_build_gate_table() {
    assert_eq!(
        production_build_gate(true, false),
        Err(RtStatus::ERR_UNSUPPORTED)
    );
    assert_eq!(production_build_gate(true, true), Ok(()));
    assert_eq!(production_build_gate(false, false), Ok(()));
    assert_eq!(production_build_gate(false, true), Ok(()));
    // 生产 debug 构建拒绝码固定为 122（core-v1 §9.3；不新增码）。
    assert_eq!(
        production_build_gate(true, false).unwrap_err().code(),
        RtStatus::ERR_UNSUPPORTED.code()
    );
}

#[test]
fn t18_platform_memory_available_flag() {
    assert_eq!(
        platform_memory_available(),
        cfg!(all(target_os = "linux", target_pointer_width = "64"))
    );
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
#[test]
fn t18_admit_production_entry_refuses_on_non_linux() {
    // 非 Linux64 开发平台：真实 mlockall/getrlimit 后端不可用 → admit() fail closed。
    let fixture = Fixture::canonical();
    assert!(!platform_memory_available());
    assert_eq!(PlatformMemory::probe(), Err(RtStatus::ERR_UNSUPPORTED));
    let result = admit(&fixture.view(), &base_env());
    assert_eq!(code_of(&result), RtStatus::ERR_UNSUPPORTED.code());
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn t18_platform_backend_probes_page_size_on_linux() {
    let mut backend = PlatformMemory::probe().expect("linux platform backend must probe");
    let page = backend.page_size();
    assert!(page.is_power_of_two());
    assert!(page >= 1024);
    assert_eq!(backend.page_size(), page);
}

// ---------------------------------------------------------------------------
// I. 其他契约细节
// ---------------------------------------------------------------------------

#[test]
fn t18_region_names_and_order() {
    assert_eq!(RegionId::ALL.len(), 5);
    assert_eq!(RegionId::Arena.name(), "arena");
    assert_eq!(RegionId::GraphCurrent.name(), "graph_current");
    assert_eq!(RegionId::GraphPending.name(), "graph_pending");
    assert_eq!(RegionId::Stacks.name(), "stacks");
    assert_eq!(RegionId::ControlPlane.name(), "control_plane");
    // 双图必须相邻且顺序固定（current 在 pending 之前）。
    assert_eq!(RegionId::ALL[1], RegionId::GraphCurrent);
    assert_eq!(RegionId::ALL[2], RegionId::GraphPending);
}

#[test]
fn t18_mock_pretouch_call_bounds() {
    let mut backend = MockMemory::new();
    assert_eq!(backend.pretouch_call(0), None);
    assert_eq!(backend.touched_bytes(), 0);
    let _ = backend.pre_touch(RegionId::Arena, 4096);
    assert_eq!(
        backend.pretouch_call(0),
        Some(PretouchCall {
            region: RegionId::Arena as u8,
            bytes: 4096
        })
    );
    assert_eq!(backend.pretouch_call(1), None);
    assert_eq!(backend.touched_bytes(), 4096);
}

#[test]
fn t18_admit_large_page_alignment_changes_budget_not_semantics() {
    let fixture = Fixture::canonical();
    let mut env = base_env();
    env.preallocated_bytes = 0;
    let memory = canonical_memory();
    let aligned_64k = memory.aligned_total_bytes(65_536).unwrap();
    assert!(aligned_64k >= memory.aligned_total_bytes(4096).unwrap());
    let mut backend = MockMemory::new().with_page_size(65_536);
    let report = admit_result(&fixture, &env, &mut backend).expect("64 KiB pages admission");
    assert_eq!(report.mlocked_bytes, aligned_64k);
    assert_eq!(backend.pretouch_calls(), 5);
    for index in 0..backend.pretouch_calls() as usize {
        assert_eq!(backend.pretouch_call(index).unwrap().bytes % 65_536, 0);
    }
}
