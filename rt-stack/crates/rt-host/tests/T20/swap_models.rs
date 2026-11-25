// T20（swap）离线模型测试：block-version+4 worker ack / 迟到 partial+graph quarantine /
// nonRT 回收 / 无 RT 最后 Arc drop / crossfade 双 CPU+RAM 预算拒绝。
//
// 本文件经 src/swap.rs 末尾的 cfg(test) mod swap_tests { include!(...) } 进入单测构建
// （cargo 不自动发现 tests/<子目录>；公共注册属 integrator，勿写入 public_registration.rs）。
// 运行：cargo test -p tspi-rt-host swap
// 全部测试名 t20_ 前缀；离线：不打开设备、不执行真实 mlockall（admission 用 MockMemory）。

use super::*;

use crate::admission::{MockMemory, MCL_CURRENT, MCL_FUTURE, MLOCK_ALL_FLAGS, RLIM_INFINITY};
use core::mem::offset_of;
use tspi_dsp_core::plan::PlanBuf;
use tspi_dsp_core::workers::set_cutoff;

// ---------------------------------------------------------------------------
// 夹具：2x2 path + 2 diag + 2 role 的小图（快速、覆盖表/系数/延迟口径）
// ---------------------------------------------------------------------------

const FS: u32 = 48_000;
const BLOCK: u16 = 128;
const N_IN: u16 = 2;
const N_OUT: u16 = 2;
const PATH_TAPS: u32 = 64;
const DIAG_TAPS: u32 = 32;
const PATH_COUNT: usize = (N_IN as usize) * (N_OUT as usize);
const DIAG_COUNT: usize = 2;
const ROLE_COUNT: usize = 2;
const PATH_COEFF_BYTES: usize = PATH_COUNT * PATH_TAPS as usize * 4;
const DIAG_COEFF_BYTES: usize = DIAG_COUNT * DIAG_TAPS as usize * 4;
const TOTAL_COEFF_BYTES: usize = PATH_COEFF_BYTES + DIAG_COEFF_BYTES;
const TABLES_BYTES: usize = 128 + PATH_COUNT * 32 + DIAG_COUNT * 32 + ROLE_COUNT * 32;

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn header_bytes(header: &TspiPlanHeaderV1) -> [u8; 128] {
    let mut out: Vec<u8> = Vec::with_capacity(128);
    push_u32(&mut out, header.magic); // 0
    push_u16(&mut out, header.abi_version); // 4
    push_u16(&mut out, header.header_size); // 6
    push_u32(&mut out, header.plan_version); // 8
    push_u32(&mut out, header.sample_rate); // 12
    push_u16(&mut out, header.block_size); // 16
    push_u16(&mut out, header.n_in); // 18
    push_u16(&mut out, header.n_out); // 20
    push_u16(&mut out, header.low_band_count); // 22
    push_u16(&mut out, header.path_count); // 24
    push_u16(&mut out, header.diag_fir_count); // 26
    push_u32(&mut out, header.max_taps); // 28
    out.push(header.workers); // 32
    out.push(header.flags); // 33
    out.push(header.precision); // 34
    out.push(header.reserved0); // 35
    out.extend_from_slice(&header.graph_id); // 36..68
    push_u32(&mut out, header.total_coeff_bytes); // 68
    push_u32(&mut out, header.path_table_offset); // 72
    push_u32(&mut out, header.diag_table_offset); // 76
    push_u32(&mut out, header.in_stride); // 80
    push_u32(&mut out, header.out_stride); // 84
    push_u32(&mut out, header.role_table_offset); // 88
    push_u16(&mut out, header.role_table_count); // 92
    push_u16(&mut out, header.reserved1); // 94
    push_u32(&mut out, header.affinity_table_offset); // 96
    push_u16(&mut out, header.affinity_count); // 100
    push_u16(&mut out, header.reserved2); // 102
    push_u32(&mut out, header.ctl_meta_offset); // 104
    push_u32(&mut out, header.ctl_meta_len); // 108
    for value in header.reserved3 {
        push_u32(&mut out, value); // 112..124
    }
    push_u32(&mut out, header.header_crc32c); // 124
    let mut bytes = [0u8; 128];
    bytes.copy_from_slice(&out);
    bytes
}

fn path_bytes(path: &TspiPathV1) -> [u8; 32] {
    let mut out: Vec<u8> = Vec::with_capacity(32);
    push_u16(&mut out, path.in_index);
    push_u16(&mut out, path.out_index);
    push_u32(&mut out, path.taps);
    push_u32(&mut out, path.delay_samples);
    out.push(path.polarity as u8);
    out.push(path.flags);
    push_u16(&mut out, path.reserved0);
    push_u64(&mut out, path.coeff_offset_bytes);
    push_u32(&mut out, path.coeff_len_bytes);
    push_u32(&mut out, path.reserved1);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    bytes
}

fn diag_bytes(diag: &TspiDiagFirV1) -> [u8; 32] {
    let mut out: Vec<u8> = Vec::with_capacity(32);
    push_u16(&mut out, diag.out_index);
    push_u16(&mut out, diag.flags);
    push_u32(&mut out, diag.taps);
    push_u32(&mut out, diag.delay_samples);
    push_u32(&mut out, diag.reserved0);
    push_u64(&mut out, diag.coeff_offset_bytes);
    push_u32(&mut out, diag.coeff_len_bytes);
    push_u32(&mut out, diag.reserved1);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    bytes
}

fn role_bytes(role: &TspiRoleV1) -> [u8; 32] {
    let mut out: Vec<u8> = Vec::with_capacity(32);
    push_u16(&mut out, role.role_id);
    push_u16(&mut out, role.out_index);
    push_u16(&mut out, role.flags);
    push_u16(&mut out, role.reserved0);
    push_u32(&mut out, role.delay_samples);
    out.extend_from_slice(&role.gain_q15.to_le_bytes());
    out.push(role.polarity as u8);
    out.push(role.reserved1);
    push_u16(&mut out, role.peq_count);
    push_u16(&mut out, role.reserved2);
    push_u32(&mut out, role.name_hash);
    push_u32(&mut out, role.reserved3);
    push_u32(&mut out, role.reserved4);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    bytes
}

fn make_path(
    in_index: u16,
    out_index: u16,
    taps: u32,
    delay: u32,
    coeff_offset: u64,
) -> TspiPathV1 {
    TspiPathV1 {
        in_index,
        out_index,
        taps,
        delay_samples: delay,
        polarity: 1,
        flags: 0,
        reserved0: 0,
        coeff_offset_bytes: coeff_offset,
        coeff_len_bytes: taps * 4,
        reserved1: 0,
    }
}

fn make_diag(out_index: u16, taps: u32, delay: u32, coeff_offset: u64) -> TspiDiagFirV1 {
    TspiDiagFirV1 {
        out_index,
        flags: 1,
        taps,
        delay_samples: delay,
        reserved0: 0,
        coeff_offset_bytes: coeff_offset,
        coeff_len_bytes: taps * 4,
        reserved1: 0,
    }
}

fn make_role(role_id: u16, out_index: u16, delay: u32) -> TspiRoleV1 {
    TspiRoleV1 {
        role_id,
        out_index,
        flags: 1,
        reserved0: 0,
        delay_samples: delay,
        gain_q15: 32_767,
        polarity: 1,
        reserved1: 0,
        peq_count: 0,
        reserved2: 0,
        name_hash: role_id as u32 + 1,
        reserved3: 0,
        reserved4: 0,
    }
}

struct Fixture {
    header: TspiPlanHeaderV1,
    paths: Vec<TspiPathV1>,
    diags: Vec<TspiDiagFirV1>,
    roles: Vec<TspiRoleV1>,
    coeffs: Vec<u8>,
    token_graph_id: [u8; 32],
}

impl Fixture {
    fn build(flags: u8, path_delay: u32, role_delay: u32, diag_delay: u32) -> Self {
        let mut paths = Vec::with_capacity(PATH_COUNT);
        let mut out_index = 0u16;
        while (out_index as usize) < N_OUT as usize {
            let mut in_index = 0u16;
            while (in_index as usize) < N_IN as usize {
                let index = paths.len();
                let coeff_offset = (index * PATH_TAPS as usize * 4) as u64;
                paths.push(make_path(
                    in_index,
                    out_index,
                    PATH_TAPS,
                    path_delay,
                    coeff_offset,
                ));
                in_index += 1;
            }
            out_index += 1;
        }
        let mut diags = Vec::with_capacity(DIAG_COUNT);
        let mut index = 0usize;
        while index < DIAG_COUNT {
            let coeff_offset = (PATH_COEFF_BYTES + index * DIAG_TAPS as usize * 4) as u64;
            diags.push(make_diag(index as u16, DIAG_TAPS, diag_delay, coeff_offset));
            index += 1;
        }
        let mut roles = Vec::with_capacity(ROLE_COUNT);
        index = 0;
        while index < ROLE_COUNT {
            roles.push(make_role(index as u16, index as u16, role_delay));
            index += 1;
        }
        let path_table_offset = 128u32;
        let diag_table_offset = path_table_offset + (PATH_COUNT * 32) as u32;
        let role_table_offset = diag_table_offset + (DIAG_COUNT * 32) as u32;
        let header = TspiPlanHeaderV1 {
            magic: TSPI_PLAN_MAGIC,
            abi_version: ABI_VERSION,
            header_size: 128,
            plan_version: 0,
            sample_rate: FS,
            block_size: BLOCK,
            n_in: N_IN,
            n_out: N_OUT,
            low_band_count: 0,
            path_count: PATH_COUNT as u16,
            diag_fir_count: DIAG_COUNT as u16,
            max_taps: PATH_TAPS,
            workers: RT_WORKERS,
            flags,
            precision: 0,
            reserved0: 0,
            graph_id: [0xA5; 32],
            total_coeff_bytes: TOTAL_COEFF_BYTES as u32,
            path_table_offset,
            diag_table_offset,
            in_stride: 128,
            out_stride: 128,
            role_table_offset,
            role_table_count: ROLE_COUNT as u16,
            affinity_table_offset: 0,
            affinity_count: 0,
            reserved1: 0,
            reserved2: 0,
            ctl_meta_offset: 0,
            ctl_meta_len: 0,
            reserved3: [0u32; 3],
            header_crc32c: 0,
        };
        Self {
            header,
            paths,
            diags,
            roles,
            coeffs: vec![0u8; TOTAL_COEFF_BYTES],
            token_graph_id: [0xA5; 32],
        }
    }

    fn canonical() -> Self {
        Self::build(0, 0, 0, 0)
    }

    fn crossfade() -> Self {
        Self::build(PLAN_FLAG_CROSSFADE, 0, 0, 0)
    }

    fn crossfade_with_delay(path_delay: u32, role_delay: u32, diag_delay: u32) -> Self {
        Self::build(PLAN_FLAG_CROSSFADE, path_delay, role_delay, diag_delay)
    }

    fn with_graph(seed: u8) -> Self {
        let mut fixture = Self::canonical();
        fixture.header.graph_id = [seed; 32];
        fixture.token_graph_id = [seed; 32];
        fixture
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(TABLES_BYTES + self.coeffs.len());
        out.extend_from_slice(&header_bytes(&self.header));
        let mut index = 0usize;
        while index < self.paths.len() {
            if let Some(path) = self.paths.get(index) {
                out.extend_from_slice(&path_bytes(path));
            }
            index += 1;
        }
        index = 0;
        while index < self.diags.len() {
            if let Some(diag) = self.diags.get(index) {
                out.extend_from_slice(&diag_bytes(diag));
            }
            index += 1;
        }
        index = 0;
        while index < self.roles.len() {
            if let Some(role) = self.roles.get(index) {
                out.extend_from_slice(&role_bytes(role));
            }
            index += 1;
        }
        debug_assert_eq!(out.len(), TABLES_BYTES);
        out.extend_from_slice(&self.coeffs);
        out
    }

    fn token(&self) -> PlanToken {
        PlanToken::new(PlanBuf::new(self.bytes()), self.token_graph_id)
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

    /// 逐输出统一的 total_delay_q16（夹具延迟均匀，故与 plan_total_delay_q16 的 max 口径一致）。
    fn uniform_delay_q16(&self) -> u64 {
        let path_delay = self.paths.first().map_or(0, |path| path.delay_samples) as u64;
        let role_delay = self.roles.first().map_or(0, |role| role.delay_samples) as u64;
        let diag_delay = self.diags.first().map_or(0, |diag| diag.delay_samples) as u64;
        (path_delay + role_delay + diag_delay) << 16
    }
}

fn base_env() -> AdmissionEnv {
    let mut env = AdmissionEnv::new();
    env.sample_rate_hz = FS;
    env.block_size = BLOCK;
    env.rt_workers = RT_WORKERS;
    env.rt_privilege_ok = true;
    env.clock_source_ok = true;
    env.cpu_set_ok = true;
    env.mlock_limit_bytes = 0;
    env.preallocated_bytes = 0;
    env
}

fn two_worker_env() -> AdmissionEnv {
    let mut env = base_env();
    env.rt_workers = RT_WORKERS_FALLBACK;
    env
}

fn control() -> TspiRtControlV1 {
    TspiRtControlV1::new()
}

fn swap_ack() -> TspiSwapAckV1 {
    TspiSwapAckV1::new()
}

fn admit_fixture(reg: &mut PlanRegistry, fixture: &Fixture) -> PlanVersion {
    let env = base_env();
    reg.admit_with(fixture.token(), &env, &mut MockMemory::new())
        .expect("canonical 2x2 plan must be admitted")
}

fn admit_fixture_mem(
    reg: &mut PlanRegistry,
    fixture: &Fixture,
    mem: &mut MockMemory,
) -> Result<PlanVersion, RtStatus> {
    let env = base_env();
    reg.admit_with(fixture.token(), &env, mem)
}

/// 完整切图：admit -> stage -> arm -> [RT] block 边界 commit -> complete_swap。
fn commit_fixture(
    reg: &mut PlanRegistry,
    fixture: &Fixture,
    ctl: &mut TspiRtControlV1,
    ack: &mut TspiSwapAckV1,
    epoch: u64,
) -> PlanVersion {
    let version = admit_fixture(reg, fixture);
    let token = reg.stage(version).expect("stage must succeed");
    assert!(reg.arm(token, ctl, ack, epoch).is_ok(), "arm must succeed");
    assert_eq!(
        commit_swap_at_block_boundary(ctl).expect("commit must succeed"),
        version
    );
    assert_eq!(
        reg.complete_swap(ctl, ack).expect("complete must succeed"),
        version
    );
    version
}

/// 建好 current=v1 的注册表（v1 已提交，无旧图可 retire）。
fn setup_current_v1() -> (PlanRegistry, TspiRtControlV1, TspiSwapAckV1, PlanVersion) {
    let mut reg = PlanRegistry::new();
    let mut ctl = control();
    let mut ack = swap_ack();
    let version = commit_fixture(&mut reg, &Fixture::with_graph(0x11), &mut ctl, &mut ack, 7);
    (reg, ctl, ack, version)
}

fn all_ack_block(plan_version: PlanVersion, expected: u8) -> TspiBlockAckV1 {
    let ack = TspiBlockAckV1::new(1, 0, plan_version, expected);
    let mask = ack_mask(expected);
    let mut phase = 0usize;
    while phase < SWAP_ACK_PHASES {
        ack.phase_done[phase].store(mask, Ordering::Release);
        phase += 1;
    }
    ack
}

fn partial_block(plan_version: PlanVersion, expected: u8) -> TspiBlockAckV1 {
    let ack = TspiBlockAckV1::new(2, 0, plan_version, expected);
    let mask = ack_mask(expected);
    ack.phase_done[0].store(mask, Ordering::Release);
    ack.phase_done[1].store(mask & 0b01, Ordering::Release);
    ack
}

// ---------------------------------------------------------------------------
// A. 布局/类型（§11.13/§13.2）
// ---------------------------------------------------------------------------

#[test]
fn t20_swap_ack_layout_matches_spec() {
    assert_eq!(size_of::<TspiSwapAckV1>(), SWAP_ACK_BYTES);
    assert_eq!(align_of::<TspiSwapAckV1>(), 64);
    assert_eq!(offset_of!(TspiSwapAckV1, plan_version), 0);
    assert_eq!(offset_of!(TspiSwapAckV1, expected_workers), 4);
    assert_eq!(offset_of!(TspiSwapAckV1, flags), 5);
    assert_eq!(offset_of!(TspiSwapAckV1, reserved0), 6);
    assert_eq!(offset_of!(TspiSwapAckV1, ack_mask), 8);
    assert_eq!(offset_of!(TspiSwapAckV1, retire_ready), 12);
    assert_eq!(offset_of!(TspiSwapAckV1, retire_epoch), 16);
    assert_eq!(offset_of!(TspiSwapAckV1, next_plan_version), 24);
    assert_eq!(offset_of!(TspiSwapAckV1, reserved1), 28);
    assert_eq!(offset_of!(TspiSwapAckV1, old_plan_ptr), 32);
    assert_eq!(offset_of!(TspiSwapAckV1, reserved2), 40);
    assert_eq!(offset_of!(TspiSwapAckV1, reserved3), 48);
    assert_eq!(offset_of!(TspiSwapAckV1, reserved4), 56);
}

#[test]
fn t20_crossfade_budget_layout_matches_spec() {
    assert_eq!(size_of::<TspiCrossfadeBudgetV1>(), CROSSFADE_BUDGET_BYTES);
    assert_eq!(align_of::<TspiCrossfadeBudgetV1>(), 64);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, old_plan_version), 0);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, new_plan_version), 4);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, old_total_delay_q16), 8);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, new_total_delay_q16), 12);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, crossfade_frames), 16);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, flags), 20);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, peak_cpu_ns), 24);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, peak_ram_bytes), 32);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, available_cpu_ns), 40);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, available_ram_bytes), 48);
    assert_eq!(offset_of!(TspiCrossfadeBudgetV1, reserved), 56);
}

#[test]
fn t20_rt_visible_types_have_no_drop() {
    assert!(!needs_drop::<TspiSwapAckV1>());
    assert!(!needs_drop::<TspiCrossfadeBudgetV1>());
    assert!(!needs_drop::<GraphQuarantine>());
    assert!(!needs_drop::<RtStatus>());
    assert!(!needs_drop::<PlanVersion>());
    assert!(!needs_drop::<PlanView<'static>>());
    assert!(!needs_drop::<TspiBlockAckV1>());
    assert!(!needs_drop::<TspiRtControlV1>());
}

#[test]
fn t20_rt_entry_points_take_borrowed_rt_structs_only() {
    // 函数指针类型断言：RT 入口只接收 &RtControl/&SwapAck/&Quarantine/&mut BlockAck 等
    // 非拥有结构，返回 RtStatus/Result<PlanVersion,RtStatus>/Option 等 Copy 值（R-DROP-03/05）。
    let commit: fn(&TspiRtControlV1) -> Result<PlanVersion, RtStatus> =
        commit_swap_at_block_boundary;
    let note_block: fn(&TspiSwapAckV1, &TspiBlockAckV1) -> RtStatus =
        TspiSwapAckV1::note_block_all_ack;
    let ack_worker: fn(&TspiSwapAckV1, u8) -> RtStatus = TspiSwapAckV1::ack_worker;
    let enter: fn(&GraphQuarantine, u64, PlanVersion, u64, u8) -> Result<usize, RtStatus> =
        GraphQuarantine::enter;
    let note: fn(&GraphQuarantine, usize, u8, PlanVersion, u64) -> RtStatus =
        GraphQuarantine::note_ack;
    let _ = (commit, note_block, ack_worker, enter, note);
    // RT 结构只携带旧 arena 的裸地址（u64），从不携带 Arc/引用所有权。
    let ack = TspiSwapAckV1::new();
    let _: u64 = ack.old_plan_ptr;
    // 空 ctl 上 commit 是 fail-closed（不是 panic）：IDLE -> ERR_SWITCH_BUSY(120)。
    let ctl = TspiRtControlV1::new();
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err().code(),
        RtStatus::ERR_SWITCH_BUSY.code()
    );
}

#[test]
fn t20_graph_quarantine_constants_match_spec() {
    assert_eq!(GRAPH_QUARANTINE_SLOTS, QUARANTINE_SLOTS);
    assert_eq!(GRAPH_QUARANTINE_SLOTS, 8);
    assert_eq!(GRAPH_QUARANTINE_FREE, 0);
    assert_eq!(GRAPH_QUARANTINE_OCCUPIED, 1);
    assert_eq!(SWITCH_SWITCHED, 2);
    assert_eq!(SWITCH_ABORT, 3);
    assert_eq!(ERR_CROSSFADE_BUDGET.code(), 123);
    assert_eq!(RT_E_CROSSFADE_ALIGN.code(), 309);
    assert_eq!(DELAY_Q16_SCALE, 65_536);
    assert_eq!(total_delay_q16(3, 32768), 3 * 65_536 + 32_768);
}

// ---------------------------------------------------------------------------
// B. TspiSwapAckV1（block-version + 4 worker ack；§7.3/§11.13）
// ---------------------------------------------------------------------------

#[test]
fn t20_swap_ack_worker_acks_accumulate_and_set_retire_ready() {
    let mut ack = swap_ack();
    assert!(ack.init(1, RT_WORKERS, 9, 1, 0).is_ok());
    assert!(!ack.is_retire_ready());
    assert!(ack.ack_worker(0).is_ok());
    assert!(ack.ack_worker(1).is_ok());
    assert!(ack.ack_worker(2).is_ok());
    assert_eq!(ack.ack_mask_value(), 0b0111);
    assert!(!ack.is_retire_ready());
    assert!(ack.ack_worker(3).is_ok());
    assert_eq!(ack.ack_mask_value(), 0xF);
    assert!(ack.is_retire_ready());
    assert_eq!(ack.retire_ready_value(), 1);
    assert_eq!(ack.retire_epoch, 9);
    assert_eq!(ack.old_plan_ptr, 0);
    assert!(ack.validate().is_ok());
}

#[test]
fn t20_swap_ack_two_worker_profile_uses_parameterized_mask() {
    let mut ack = swap_ack();
    assert!(ack.init(2, RT_WORKERS_FALLBACK, 0, 2, 0).is_ok());
    assert_eq!(ack.required_mask(), 0b11);
    assert!(ack.ack_worker(0).is_ok());
    assert!(!ack.all_acked());
    assert!(ack.ack_worker(1).is_ok());
    assert!(ack.is_retire_ready());
    assert_eq!(ack.ack_mask_value(), 0b11);
    assert_eq!(ack.ack_worker(2), RtStatus::ERR_ADMISSION_SCHED);
    assert_eq!(ack.ack_worker(4), RtStatus::ERR_ADMISSION_SCHED);
}

#[test]
fn t20_swap_ack_version_label_mismatch_rejected() {
    let mut ack = swap_ack();
    assert!(ack.init(3, RT_WORKERS, 0, 3, 0x1000).is_ok());
    assert_eq!(ack.ack_worker_version(0, 2), RtStatus::RT_E_PLAN_MISMATCH);
    assert_eq!(ack.ack_mask_value(), 0);
    assert!(ack.ack_worker_version(0, 3).is_ok());
    assert_eq!(ack.ack_mask_value(), 1);
}

#[test]
fn t20_swap_ack_duplicate_worker_ack_idempotent() {
    let mut ack = swap_ack();
    assert!(ack.init(1, RT_WORKERS, 0, 1, 0).is_ok());
    assert!(ack.ack_worker(2).is_ok());
    assert!(ack.ack_worker(2).is_ok());
    assert_eq!(ack.ack_mask_value(), 0b100);
    assert!(!ack.is_retire_ready());
}

#[test]
fn t20_swap_ack_rejects_invalid_expected_workers() {
    let mut ack = swap_ack();
    assert_eq!(ack.init(1, 3, 0, 1, 0), RtStatus::ERR_ADMISSION_SCHED);
    let mut ack = swap_ack();
    ack.expected_workers = 3;
    assert_eq!(ack.validate(), RtStatus::ERR_ADMISSION_SCHED);
    assert_eq!(ack.ack_worker(0), RtStatus::ERR_ADMISSION_SCHED);
    assert_eq!(ack.required_mask(), 0);
}

#[test]
fn t20_swap_ack_validate_rejects_unknown_flags_and_reserved() {
    let mut ack = swap_ack();
    assert!(ack.init(1, RT_WORKERS, 0, 1, 0).is_ok());
    ack.flags = 0x80;
    assert_eq!(ack.validate(), RtStatus::RT_E_ABI_RUNTIME);
    ack.flags = 0;
    ack.reserved2 = 5;
    assert_eq!(ack.validate(), RtStatus::RT_E_ABI_RUNTIME);
    ack.reserved2 = 0;
    ack.ack_mask.store(0b1_0000, Ordering::Release);
    assert_eq!(ack.validate(), RtStatus::RT_E_ABI_RUNTIME);
}

#[test]
fn t20_swap_ack_retire_ready_requires_full_mask() {
    let mut ack = swap_ack();
    assert!(ack.init(1, RT_WORKERS, 0, 1, 0).is_ok());
    ack.ack_mask.store(0xF, Ordering::Release);
    ack.retire_ready.store(1, Ordering::Release);
    assert!(ack.validate().is_ok());
    // flags 镜像位与 retire_ready 原子不一致 = 不变量破坏。
    ack.flags = SWAP_ACK_FLAG_RETIRE_READY;
    ack.retire_ready.store(0, Ordering::Release);
    assert_eq!(ack.validate(), RtStatus::RT_E_INVARIANT);
}

#[test]
fn t20_swap_ack_block_all_ack_sets_all_workers() {
    let mut ack = swap_ack();
    assert!(ack.init(5, RT_WORKERS, 0, 5, 0x2000).is_ok());
    let block = all_ack_block(5, RT_WORKERS);
    assert!(ack.note_block_all_ack(&block).is_ok());
    assert_eq!(ack.ack_mask_value(), 0xF);
    assert!(ack.is_retire_ready());
}

#[test]
fn t20_swap_ack_block_partial_phase_rejected() {
    let mut ack = swap_ack();
    assert!(ack.init(5, RT_WORKERS, 0, 5, 0).is_ok());
    let block = partial_block(5, RT_WORKERS);
    assert_eq!(ack.note_block_all_ack(&block), RtStatus::RT_E_PARTIAL_LATE);
    assert_eq!(ack.ack_mask_value(), 0);
    assert_eq!(ack.retire_ready_value(), 0);
}

#[test]
fn t20_swap_ack_block_cutoff_rejected() {
    let mut ack = swap_ack();
    assert!(ack.init(5, RT_WORKERS, 0, 5, 0).is_ok());
    let block = all_ack_block(5, RT_WORKERS);
    assert!(set_cutoff(&block).is_ok());
    assert_eq!(ack.note_block_all_ack(&block), RtStatus::RT_E_PARTIAL_LATE);
    assert!(!ack.is_retire_ready());
}

#[test]
fn t20_swap_ack_block_version_label_rejected() {
    let mut ack = swap_ack();
    assert!(ack.init(5, RT_WORKERS, 0, 5, 0).is_ok());
    let block = all_ack_block(4, RT_WORKERS);
    assert_eq!(ack.note_block_all_ack(&block), RtStatus::RT_E_PLAN_MISMATCH);
    // 旧版本 block 的 ack 不得推进新版本 swap ack（迟到 worker 场景）。
    assert_eq!(ack.ack_mask_value(), 0);
}

#[test]
fn t20_swap_ack_block_expected_mismatch_rejected() {
    let mut ack = swap_ack();
    assert!(ack.init(5, RT_WORKERS, 0, 5, 0).is_ok());
    let block = all_ack_block(5, RT_WORKERS_FALLBACK);
    assert_eq!(ack.note_block_all_ack(&block), RtStatus::RT_E_PLAN_MISMATCH);
}

#[test]
fn t20_swap_ack_reset_clears_state() {
    let mut ack = swap_ack();
    assert!(ack.init(2, RT_WORKERS, 3, 2, 0x9000).is_ok());
    let block = all_ack_block(2, RT_WORKERS);
    assert!(ack.note_block_all_ack(&block).is_ok());
    ack.sync_retire_ready_flag();
    assert_eq!(ack.flags, SWAP_ACK_FLAG_RETIRE_READY);
    ack.reset();
    assert_eq!(ack.plan_version, 0);
    assert_eq!(ack.expected_workers, 0);
    assert_eq!(ack.flags, 0);
    assert_eq!(ack.ack_mask_value(), 0);
    assert_eq!(ack.retire_ready_value(), 0);
    assert_eq!(ack.old_plan_ptr, 0);
    assert_eq!(ack.retire_epoch, 0);
}

#[test]
fn t20_swap_ack_init_and_sync_retire_ready_flag() {
    let mut ack = swap_ack();
    assert_eq!(
        ack.init(0, RT_WORKERS, 0, 0, 0),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert_eq!(
        ack.init(2, RT_WORKERS, 0, 1, 0),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert!(ack.init(2, RT_WORKERS, 4, 2, 0x8000).is_ok());
    assert_eq!(ack.next_plan_version, 2);
    assert_eq!(ack.old_plan_ptr, 0x8000);
    assert_eq!(ack.retire_epoch, 4);
    ack.sync_retire_ready_flag();
    assert_eq!(ack.flags, 0);
}

// ---------------------------------------------------------------------------
// C. GraphQuarantine（迟到 partial/graph 直到 all-ack；§7.6/INV-L5）
// ---------------------------------------------------------------------------

#[test]
fn t20_graph_quarantine_enter_sets_labels_and_mask() {
    let quarantine = GraphQuarantine::new();
    let slot = quarantine
        .enter(11, 7, 0xDEAD_0000, RT_WORKERS)
        .expect("slot");
    assert_eq!(slot, 0);
    let entry = quarantine.entry(slot).expect("occupied");
    assert_eq!(entry.epoch_id, 11);
    assert_eq!(entry.plan_version, 7);
    assert_eq!(entry.old_plan_ptr, 0xDEAD_0000);
    assert_eq!(entry.expected_workers, RT_WORKERS);
    assert_eq!(entry.remaining_mask, 0xF);
    assert!(quarantine.is_occupied(slot));
    assert_eq!(quarantine.in_use(), 1);
    assert_eq!(quarantine.free_slot_count(), 7);
    assert_eq!(quarantine.find(7), Some(slot));
}

#[test]
fn t20_graph_quarantine_not_released_before_all_ack() {
    let quarantine = GraphQuarantine::new();
    let slot = quarantine.enter(1, 3, 0x1000, RT_WORKERS).expect("slot");
    assert_eq!(quarantine.note_ack(slot, 0, 3, 1), RtStatus::OK);
    assert_eq!(quarantine.note_ack(slot, 1, 3, 1), RtStatus::OK);
    assert_eq!(quarantine.note_ack(slot, 2, 3, 1), RtStatus::OK);
    assert_eq!(quarantine.remaining_mask(slot), Some(0b1000));
    assert_eq!(
        quarantine.try_release(slot).unwrap_err(),
        RtStatus::RT_E_SLOT_STATE
    );
    assert!(quarantine.is_occupied(slot));
    // 槽未释放前不可复用：下一次 enter 拿新槽，不能拿 0 号。
    assert_eq!(quarantine.enter(2, 4, 0x2000, RT_WORKERS), Ok(1));
    assert_eq!(quarantine.note_ack(slot, 3, 3, 1), RtStatus::OK);
    assert_eq!(quarantine.remaining_mask(slot), Some(0));
    assert_eq!(quarantine.try_release(slot), Ok(3));
    assert!(!quarantine.is_occupied(slot));
}

#[test]
fn t20_graph_quarantine_late_ack_label_mismatch() {
    let quarantine = GraphQuarantine::new();
    let slot = quarantine.enter(9, 5, 0x5000, RT_WORKERS).expect("slot");
    // 版本不符 = 迟到 worker 指向旧标签；不得清当前槽位。
    assert_eq!(
        quarantine.note_ack(slot, 0, 4, 9),
        RtStatus::RT_E_PARTIAL_LATE
    );
    assert_eq!(quarantine.remaining_mask(slot), Some(0xF));
    // epoch 不符 = 跨 epoch 复用。
    assert_eq!(
        quarantine.note_ack(slot, 0, 5, 8),
        RtStatus::RT_E_EPOCH_MISMATCH
    );
    assert_eq!(quarantine.remaining_mask(slot), Some(0xF));
    // worker 越界。
    assert_eq!(
        quarantine.note_ack(slot, 7, 5, 9),
        RtStatus::ERR_ADMISSION_SCHED
    );
    // 未占用槽 / 越界槽。
    assert_eq!(quarantine.note_ack(7, 0, 5, 9), RtStatus::RT_E_SLOT_STATE);
    assert_eq!(quarantine.note_ack(8, 0, 5, 9), RtStatus::ERR_BAD_LAYOUT);
}

#[test]
fn t20_graph_quarantine_exhaustion_rejects_new_enter() {
    let quarantine = GraphQuarantine::new();
    let mut version = 1u32;
    while version <= GRAPH_QUARANTINE_SLOTS as u32 {
        assert!(
            quarantine
                .enter(1, version, 0x1000 + version as u64, RT_WORKERS)
                .is_ok(),
            "slot for version {}",
            version
        );
        version += 1;
    }
    assert_eq!(quarantine.in_use(), GRAPH_QUARANTINE_SLOTS as u32);
    assert_eq!(quarantine.free_slot_count(), 0);
    assert_eq!(
        quarantine.enter(1, 99, 0x9000, RT_WORKERS).unwrap_err(),
        RtStatus::RT_E_QUARANTINE_FULL
    );
    assert_eq!(
        quarantine.enter(1, 0, 0, RT_WORKERS).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert_eq!(
        quarantine.enter(1, 99, 0x9000, 3).unwrap_err(),
        RtStatus::ERR_ADMISSION_SCHED
    );
}

#[test]
fn t20_graph_quarantine_release_all_acked() {
    let quarantine = GraphQuarantine::new();
    let first = quarantine.enter(1, 1, 0x1000, RT_WORKERS).expect("slot");
    let second = quarantine.enter(2, 2, 0x2000, RT_WORKERS).expect("slot");
    let mut worker = 0u8;
    while worker < RT_WORKERS {
        assert!(quarantine.note_ack(first, worker, 1, 1).is_ok());
        worker += 1;
    }
    assert_eq!(quarantine.release_all_acked(), 1);
    assert!(!quarantine.is_occupied(first));
    assert!(quarantine.is_occupied(second));
    assert_eq!(quarantine.remaining_mask(second), Some(0xF));
}
// ---------------------------------------------------------------------------
// D. plan 解析/准入（[NONRT]；§11.10/§5.3）
// ---------------------------------------------------------------------------

fn admit_err(reg: &mut PlanRegistry, fixture: &Fixture) -> RtStatus {
    match admit_fixture_mem(reg, fixture, &mut MockMemory::new()) {
        Ok(version) => panic!("admit must fail, got version {}", version),
        Err(status) => status,
    }
}

#[test]
fn t20_admit_assigns_monotonic_versions() {
    let mut reg = PlanRegistry::new();
    let first = admit_fixture(&mut reg, &Fixture::with_graph(0x31));
    let second = admit_fixture(&mut reg, &Fixture::with_graph(0x32));
    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(reg.admitted_versions(), vec![1, 2]);
    assert_eq!(reg.plan_state(first), Some(PlanState::Admitted));
    assert_eq!(reg.arena_strong_count(first), Some(1));
    assert!(reg.arena_ptr(first).is_some());
    assert_eq!(reg.stats().admitted, 2);
}

#[test]
fn t20_admit_rejects_bad_magic() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.magic = 0;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_MAGIC);
}

#[test]
fn t20_admit_rejects_bad_abi_version() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    // GAP-T66-3：plan ABI v2（per-role IIR 区）是合法版本，v1 旧 plan 仍合法；
    // 只有 v1/v2 之外的版本才 ERR_ABI_VERSION。
    fixture.header.abi_version = ABI_VERSION + 2;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_ABI_VERSION);
}

#[test]
fn t20_admit_accepts_plan_abi_v2_without_iir() {
    // GAP-T66-3 向后/向前兼容：v2 且无 IIR 表（reserved3 全 0）等价 v1 语义，必须准入。
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.abi_version = tspi_dsp_core::abi::TSPI_PLAN_ABI_V2;
    let version = admit_fixture(&mut reg, &fixture);
    assert_eq!(version, 1);
    assert!(reg.iir_entries(version).expect("v2 plan").is_empty());
}

#[test]
fn t20_admit_rejects_bad_header_layout() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.header_size = 64;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
    let mut fixture = Fixture::canonical();
    fixture.header.reserved0 = 1;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
    let mut fixture = Fixture::canonical();
    fixture.header.flags = 0x80;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
}

/// E4（core-v1 v1.1.3 §0.1/§11.10）：plan flags bit0..4 有效，bit5..7 必须 0。
/// 旧 PLAN_FLAG_MASK=0x3F 会静默接受 bit5（v1.1 的 affinity_table_present 位义已被 E4 取消）。
#[test]
fn t20_admit_plan_flags_e4_rejects_bits5_to_7() {
    assert_eq!(PLAN_FLAG_MASK, 0x1F, "E4: plan flags bit5..7 reserved");

    // 已定义的非 crossfade 位（bit0 stereo_strict / bit1 modal_support / bit3 extended_taps_admitted /
    // bit4 fallback_graph）单独/组合置位都必须接受。同图重复准入会被 ERR_PLAN_STALE 拒，
    // 因此每个接受用独立 registry（与 t20_admit_assigns_monotonic_versions 用不同 graph 同理）。
    for known in [1u8 << 0, 1 << 1, 1 << 3, 1 << 4] {
        let mut reg = PlanRegistry::new();
        let mut fixture = Fixture::canonical();
        fixture.header.flags = known;
        assert!(
            admit_fixture_mem(&mut reg, &fixture, &mut MockMemory::new()).is_ok(),
            "known plan flag {:#04x} must be accepted",
            known
        );
    }
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.flags = (1 << 0) | (1 << 1) | (1 << 3) | (1 << 4);
    assert!(admit_fixture_mem(&mut reg, &fixture, &mut MockMemory::new()).is_ok());

    // bit2=crossfade 位义不变：普通 admit 仍拒（必须走 admit_crossfade，G/I06 已有测试）。
    let mut fixture = Fixture::canonical();
    fixture.header.flags = PLAN_FLAG_CROSSFADE;
    assert_eq!(
        admit_err(&mut reg, &fixture),
        ERR_CROSSFADE_BUDGET,
        "bit2 must still require crossfade admission"
    );

    // bit5/6/7 任意置位（含组合）都必须以 ERR_BAD_LAYOUT 拒收（E4）。
    // 每个拒收用独立 registry：确保失败点就是 plan flags 校验，而不是后续同图去重（ERR_PLAN_STALE）。
    for bit in [5u8, 6, 7] {
        let mut fixture = Fixture::canonical();
        fixture.header.flags = 1u8 << bit;
        assert_eq!(
            admit_err(&mut PlanRegistry::new(), &fixture),
            RtStatus::ERR_BAD_LAYOUT,
            "flags bit{} must be rejected",
            bit
        );
    }
    let mut fixture = Fixture::canonical();
    fixture.header.flags = 0xE0;
    assert_eq!(admit_err(&mut PlanRegistry::new(), &fixture), RtStatus::ERR_BAD_LAYOUT);

    // bit0..4 与 bit5..7 混置同样拒收（不因已知位存在而放过保留位）。
    let mut fixture = Fixture::canonical();
    fixture.header.flags = PLAN_FLAG_MASK | (1 << 5);
    assert_eq!(admit_err(&mut PlanRegistry::new(), &fixture), RtStatus::ERR_BAD_LAYOUT);
}

#[test]
fn t20_admit_rejects_bad_rate_and_block() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.sample_rate = 44_100;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_RATE);
    let mut fixture = Fixture::canonical();
    fixture.header.block_size = 256;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BLOCK_SIZE);
}

#[test]
fn t20_admit_rejects_bad_precision_and_workers() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.precision = 1;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PRECISION);
    let mut fixture = Fixture::canonical();
    fixture.header.workers = 3;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_ADMISSION_SCHED);
}

#[test]
fn t20_admit_rejects_channels_and_stride() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.n_in = 0;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_CHANNELS);
    let mut fixture = Fixture::canonical();
    fixture.header.in_stride = BLOCK as u32 - 8;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_STRIDE_ALIGN);
    let mut fixture = Fixture::canonical();
    fixture.header.out_stride = 130;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_STRIDE_ALIGN);
}

#[test]
fn t20_admit_rejects_unsorted_paths() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.paths.swap(1, 2);
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PATH_TABLE);
}

#[test]
fn t20_admit_rejects_path_out_of_range() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.paths[0].in_index = 9;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PATH_TABLE);
    let mut fixture = Fixture::canonical();
    fixture.paths[0].out_index = 9;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PATH_TABLE);
}

#[test]
fn t20_admit_rejects_taps_over_max_and_extended_without_flag() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.paths[0].taps = PATH_TAPS * 2;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_TAPS);
    // extended taps（>4096）必须在 head flags bit3 显式准入。
    let mut fixture = Fixture::canonical();
    fixture.header.max_taps = 8192;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_TAPS);
    let mut fixture = Fixture::canonical();
    fixture.header.max_taps = TAPS_EXTENDED_MAX + 1;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_TAPS);
}

#[test]
fn t20_admit_rejects_coeff_layout() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.paths[0].coeff_len_bytes = 4;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PATH_TABLE);
    let mut fixture = Fixture::canonical();
    fixture.paths[0].coeff_offset_bytes = 4096;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PATH_TABLE);
    let mut fixture = Fixture::canonical();
    fixture.paths[0].coeff_offset_bytes = 8;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_PATH_TABLE);
    let mut fixture = Fixture::canonical();
    fixture.header.total_coeff_bytes = 1_000_000;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
}

#[test]
fn t20_admit_rejects_graph_id_mismatch() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.token_graph_id = [0x77; 32];
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_HASH);
}

#[test]
fn t20_admit_rejects_table_out_of_bounds() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.path_table_offset = 400;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
    let mut fixture = Fixture::canonical();
    fixture.header.role_table_offset = 0;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
    let mut fixture = Fixture::canonical();
    fixture.header.ctl_meta_len = 8;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_BAD_LAYOUT);
}

#[test]
fn t20_admit_rejects_bad_role_table() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.roles[0].role_id = 1;
    fixture.roles[1].role_id = 0;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_ROLES);
    let mut fixture = Fixture::canonical();
    fixture.roles[0].delay_samples = FS / 5 + 1;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_ROLES);
    let mut fixture = Fixture::canonical();
    fixture.roles[0].peq_count = 33;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_ROLES);
    let mut fixture = Fixture::canonical();
    fixture.roles[0].polarity = 0;
    assert_eq!(admit_err(&mut reg, &fixture), RtStatus::ERR_ROLES);
}

#[test]
fn t20_admit_runs_admission_mlock_and_pretouch() {
    let mut reg = PlanRegistry::new();
    let mut mem = MockMemory::new();
    let version = admit_fixture_mem(&mut reg, &Fixture::canonical(), &mut mem).expect("admit");
    assert_eq!(version, 1);
    assert_eq!(mem.mlock_calls(), 1);
    assert_eq!(mem.mlock_flags(), MLOCK_ALL_FLAGS);
    assert_eq!(mem.mlock_flags(), MCL_CURRENT | MCL_FUTURE);
    assert_eq!(mem.pretouch_calls(), 5);
    let plan_view = reg.plan_view(version).expect("view");
    assert_eq!(plan_view.header.plan_version, version);
    assert_eq!(plan_view.paths.len(), PATH_COUNT);
    assert_eq!(plan_view.diags.len(), DIAG_COUNT);
    assert_eq!(plan_view.roles.len(), ROLE_COUNT);
    assert_eq!(plan_view.coeffs.len(), TOTAL_COEFF_BYTES);
    assert!(reg.plan_view(version).expect("view").header.graph_id != [0u8; 32]);
}

#[test]
fn t20_admit_failure_keeps_version_free() {
    let mut reg = PlanRegistry::new();
    let mut bad = Fixture::canonical();
    bad.header.magic = 0;
    assert_eq!(admit_err(&mut reg, &bad), RtStatus::ERR_MAGIC);
    assert!(reg.admitted_versions().is_empty());
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x33));
    assert_eq!(version, 1, "失败不得消耗 plan 版本号");
    assert_eq!(reg.stats().admitted, 1);
}

#[test]
fn t20_admit_rejects_unchanged_graph() {
    let mut reg = PlanRegistry::new();
    let mut ctl = control();
    let mut ack = swap_ack();
    let _ = commit_fixture(&mut reg, &Fixture::with_graph(0x41), &mut ctl, &mut ack, 1);
    let mut same = Fixture::with_graph(0x41);
    same.header.graph_id = [0x41; 32];
    assert_eq!(admit_err(&mut reg, &same), RtStatus::ERR_PLAN_STALE);
}

#[test]
fn t20_admit_rejects_mismatched_env() {
    let mut reg = PlanRegistry::new();
    let mut env = base_env();
    env.sample_rate_hz = 96_000;
    let status = reg
        .admit_with(Fixture::canonical().token(), &env, &mut MockMemory::new())
        .unwrap_err();
    assert_eq!(status, RtStatus::ERR_RATE);
    let mut env = two_worker_env();
    env.block_size = 64;
    let status = reg
        .admit_with(Fixture::canonical().token(), &env, &mut MockMemory::new())
        .unwrap_err();
    // plan(B=128,workers=4) 与 env 不符：block 先判 -> ERR_BLOCK_SIZE。
    assert_eq!(status, RtStatus::ERR_BLOCK_SIZE);
}

#[test]
fn t20_admit_respects_env_memlock_limit() {
    let mut reg = PlanRegistry::new();
    let mut env = base_env();
    env.mlock_limit_bytes = 4096;
    let status = reg
        .admit_with(Fixture::canonical().token(), &env, &mut MockMemory::new())
        .unwrap_err();
    assert_eq!(status, RtStatus::ERR_ADMISSION_MEMORY);
    let mut env = base_env();
    env.mlock_limit_bytes = RLIM_INFINITY;
    let mut mem = MockMemory::new().with_memlock_limit(RLIM_INFINITY);
    assert!(reg
        .admit_with(Fixture::with_graph(0x34).token(), &env, &mut mem)
        .is_ok());
}

#[test]
fn t20_admit_two_worker_profile_succeeds() {
    let mut reg = PlanRegistry::new();
    let mut fixture = Fixture::canonical();
    fixture.header.workers = RT_WORKERS_FALLBACK;
    let mut env = two_worker_env();
    env.rt_workers = RT_WORKERS_FALLBACK;
    let version = reg
        .admit_with(fixture.token(), &env, &mut MockMemory::new())
        .expect("two worker profile");
    assert_eq!(reg.plan_view(version).expect("view").header.workers, 2);
}

#[test]
fn t20_admit_production_entry_fails_closed_without_platform_backend() {
    // 非 Linux64 开发平台：PlatformMemory::probe() 恒 Err（fail closed，不冒充真实 mlock）。
    let mut reg = PlanRegistry::new();
    let result = reg.admit(Fixture::canonical().token(), &base_env());
    if cfg!(all(target_os = "linux", target_pointer_width = "64")) {
        // Linux64 上真实后端可用性取决于运行环境；本测试不断言其结果（板端另测）。
    } else {
        assert_eq!(result.unwrap_err(), RtStatus::ERR_UNSUPPORTED);
    }
}

// ---------------------------------------------------------------------------
// E. stage / arm / commit / complete / abort（§5.3/§7.5 RG-1）
// ---------------------------------------------------------------------------

#[test]
fn t20_stage_rejects_unknown_and_current_version() {
    let mut reg = PlanRegistry::new();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x51));
    assert_eq!(reg.stage(99).unwrap_err(), RtStatus::ERR_PLAN_STALE);
    let token = reg.stage(version).expect("stage");
    assert_eq!(token.version, version);
    assert_eq!(reg.staged_version(), Some(version));
    assert_eq!(reg.plan_state(version), Some(PlanState::Staged));
    assert_eq!(reg.stage(version).unwrap_err(), RtStatus::ERR_SWITCH_BUSY);
    assert_eq!(reg.stage(99).unwrap_err(), RtStatus::ERR_SWITCH_BUSY);
}

#[test]
fn t20_arm_writes_pending_and_arms() {
    let mut reg = PlanRegistry::new();
    let ctl = control();
    let mut ack = swap_ack();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x52));
    let token = reg.stage(version).expect("stage");
    assert!(reg.arm(token, &ctl, &mut ack, 42).is_ok());
    assert!(reg.is_armed());
    assert_eq!(ctl.switch_state.load(Ordering::Acquire), SWITCH_ARMED);
    assert_eq!(ctl.pending_plan_version.load(Ordering::Acquire), version);
    assert_eq!(
        ctl.pending_plan_ptr.load(Ordering::Acquire),
        reg.arena_ptr(version).expect("ptr")
    );
    assert_eq!(ctl.current_plan_version.load(Ordering::Acquire), 0);
    assert_eq!(ack.plan_version, version);
    assert_eq!(ack.expected_workers, RT_WORKERS);
    assert_eq!(ack.retire_epoch, 42);
    assert_eq!(ack.old_plan_ptr, 0);
    assert_eq!(reg.stats().armed, 1);
}

#[test]
fn t20_arm_rejects_bad_ctl_state_and_stale_current() {
    let mut reg = PlanRegistry::new();
    let ctl = control();
    let mut ack = swap_ack();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x53));
    let token = reg.stage(version).expect("stage");
    let busy = control();
    busy.switch_state.store(SWITCH_ARMED, Ordering::Release);
    assert_eq!(
        reg.arm(token, &busy, &mut ack, 0),
        RtStatus::ERR_SWITCH_BUSY
    );
    ctl.current_plan_version.store(9, Ordering::Release);
    assert_eq!(
        reg.arm(token, &ctl, &mut ack, 0),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    // 失败不改变本地状态（仍 staged，未 armed）。
    assert!(!reg.is_armed());
    assert_eq!(reg.staged_version(), Some(version));
}

#[test]
fn t20_commit_swaps_pointer_and_version() {
    let mut reg = PlanRegistry::new();
    let ctl = control();
    let mut ack = swap_ack();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x54));
    let token = reg.stage(version).expect("stage");
    assert!(reg.arm(token, &ctl, &mut ack, 1).is_ok());
    let pending_ptr = ctl.pending_plan_ptr.load(Ordering::Acquire);
    assert_eq!(commit_swap_at_block_boundary(&ctl), Ok(version));
    assert_eq!(ctl.switch_state.load(Ordering::Acquire), SWITCH_SWITCHED);
    assert_eq!(ctl.current_plan_version.load(Ordering::Acquire), version);
    assert_eq!(ctl.current_plan_ptr.load(Ordering::Acquire), pending_ptr);
    assert_eq!(ctl.pending_plan_version.load(Ordering::Acquire), 0);
    assert_eq!(ctl.pending_plan_ptr.load(Ordering::Acquire), 0);
    // 重复提交 = 在途切换（非 ARMED）。
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::ERR_SWITCH_BUSY
    );
}

#[test]
fn t20_commit_rejects_illegal_pending_labels() {
    let ctl = control();
    ctl.switch_state.store(SWITCH_ARMED, Ordering::Release);
    ctl.current_plan_version.store(5, Ordering::Release);
    ctl.pending_plan_version.store(5, Ordering::Release);
    ctl.pending_plan_ptr.store(0x1000, Ordering::Release);
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    ctl.pending_plan_version.store(4, Ordering::Release);
    ctl.current_plan_ptr.store(0x2000, Ordering::Release);
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    ctl.pending_plan_version.store(6, Ordering::Release);
    ctl.pending_plan_ptr.store(0, Ordering::Release);
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    ctl.pending_plan_ptr.store(0x2000, Ordering::Release);
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    ctl.pending_plan_ptr.store(0x3000, Ordering::Release);
    ctl.safety_request.store(1, Ordering::Release);
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::RT_E_WATCHDOG
    );
}

#[test]
fn t20_commit_rejects_armed_with_no_pending() {
    let ctl = control();
    ctl.switch_state.store(SWITCH_ARMED, Ordering::Release);
    assert_eq!(
        commit_swap_at_block_boundary(&ctl).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    let mut bad = control();
    bad.magic = 0;
    assert_eq!(
        commit_swap_at_block_boundary(&bad).unwrap_err(),
        RtStatus::ERR_BAD_LAYOUT
    );
}

#[test]
fn t20_complete_swap_returns_idle_and_promotes() {
    let mut reg = PlanRegistry::new();
    let mut ctl = control();
    let mut ack = swap_ack();
    let version = commit_fixture(&mut reg, &Fixture::with_graph(0x55), &mut ctl, &mut ack, 3);
    assert_eq!(ctl.switch_state.load(Ordering::Acquire), SWITCH_IDLE);
    assert_eq!(reg.current_version(), Some(version));
    assert_eq!(reg.committed(), 1);
    assert_eq!(reg.plan_state(version), Some(PlanState::Committed));
    assert_eq!(ctl.current_plan_version.load(Ordering::Acquire), version);
    assert!(reg.staged_version().is_none());
    assert!(!reg.is_armed());
    assert_eq!(reg.stats().committed, 1);
}

#[test]
fn t20_complete_swap_requires_switched_state_and_labels() {
    let mut reg = PlanRegistry::new();
    let ctl = control();
    let mut ack = swap_ack();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x56));
    let token = reg.stage(version).expect("stage");
    assert!(reg.arm(token, &ctl, &mut ack, 1).is_ok());
    // 只 arm 未提交（ARMED）。
    assert_eq!(
        reg.complete_swap(&ctl, &ack).unwrap_err(),
        RtStatus::ERR_SWITCH_BUSY
    );
    assert!(commit_swap_at_block_boundary(&ctl).is_ok());
    // swap ack 标签不符 -> 拒绝（不提前回 IDLE）。
    let mut wrong = swap_ack();
    assert!(wrong.init(99, RT_WORKERS, 0, 99, 0).is_ok());
    assert_eq!(
        reg.complete_swap(&ctl, &wrong).unwrap_err(),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert_eq!(ctl.switch_state.load(Ordering::Acquire), SWITCH_SWITCHED);
    assert_eq!(reg.complete_swap(&ctl, &ack), Ok(version));
}

#[test]
fn t20_abort_before_arm_and_disarm_after_arm() {
    let mut reg = PlanRegistry::new();
    let ctl = control();
    let mut ack = swap_ack();
    let first = admit_fixture(&mut reg, &Fixture::with_graph(0x57));
    let second = admit_fixture(&mut reg, &Fixture::with_graph(0x58));
    // 未 arm 的 abort 只回本地状态。
    let token = reg.stage(first).expect("stage");
    assert_eq!(reg.abort(token), RtStatus::OK);
    assert_eq!(reg.plan_state(first), Some(PlanState::Admitted));
    assert_eq!(reg.staged_version(), None);
    assert_eq!(reg.stats().aborted, 1);
    assert_eq!(reg.abort(token), RtStatus::ERR_PLAN_STALE);
    // arm 之后必须 abort_and_disarm（先清 pending 再回 IDLE）。
    let token = reg.stage(second).expect("stage");
    assert!(reg.arm(token, &ctl, &mut ack, 5).is_ok());
    assert_eq!(reg.abort(token), RtStatus::ERR_SWITCH_BUSY);
    assert_eq!(reg.abort_and_disarm(token, &ctl), RtStatus::OK);
    assert_eq!(ctl.switch_state.load(Ordering::Acquire), SWITCH_IDLE);
    assert_eq!(ctl.pending_plan_version.load(Ordering::Acquire), 0);
    assert_eq!(ctl.pending_plan_ptr.load(Ordering::Acquire), 0);
    assert_eq!(reg.plan_state(second), Some(PlanState::Admitted));
    assert_eq!(reg.stats().aborted, 2);
}

#[test]
fn t20_abort_and_disarm_after_commit_rejected() {
    let mut reg = PlanRegistry::new();
    let ctl = control();
    let mut ack = swap_ack();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x59));
    let token = reg.stage(version).expect("stage");
    assert!(reg.arm(token, &ctl, &mut ack, 1).is_ok());
    assert!(commit_swap_at_block_boundary(&ctl).is_ok());
    assert_eq!(
        reg.abort_and_disarm(token, &ctl),
        RtStatus::RT_E_PLAN_MISMATCH
    );
    assert!(reg.complete_swap(&ctl, &ack).is_ok());
}

#[test]
fn t20_shutdown_drops_all_plans() {
    let mut reg = PlanRegistry::new();
    let mut ctl = control();
    let mut ack = swap_ack();
    let first = commit_fixture(&mut reg, &Fixture::with_graph(0x5A), &mut ctl, &mut ack, 1);
    let second = admit_fixture(&mut reg, &Fixture::with_graph(0x5B));
    assert_eq!(reg.arena_strong_count(first), Some(1));
    assert_eq!(reg.arena_strong_count(second), Some(1));
    assert_eq!(reg.shutdown(), 2);
    assert_eq!(reg.arena_strong_count(first), None);
    assert_eq!(reg.arena_strong_count(second), None);
    assert_eq!(reg.current_version(), None);
    assert_eq!(reg.stats().unmapped, 2);
}

// ---------------------------------------------------------------------------
// F. 双图热切集成：complete_swap -> quarantine -> swap ack -> retire gate
// ---------------------------------------------------------------------------

fn quarantine_slot(reg: &PlanRegistry, version: PlanVersion) -> usize {
    match reg.quarantine().find(version) {
        Some(slot) => slot,
        None => panic!("plan version {} not quarantined", version),
    }
}

/// 把 current 从 old 切到 new（admit+stage+arm+commit+complete），返回新版本。
fn swap_current(
    reg: &mut PlanRegistry,
    ctl: &mut TspiRtControlV1,
    ack: &mut TspiSwapAckV1,
    fixture: &Fixture,
    epoch: u64,
) -> PlanVersion {
    let version = admit_fixture(reg, fixture);
    let token = reg.stage(version).expect("stage");
    assert!(reg.arm(token, ctl, ack, epoch).is_ok(), "arm");
    assert_eq!(commit_swap_at_block_boundary(ctl).expect("commit"), version);
    assert_eq!(reg.complete_swap(ctl, ack).expect("complete"), version);
    version
}

#[test]
fn t20_swap_complete_quarantines_old_graph() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x61), 11);
    assert_eq!(reg.current_version(), Some(new));
    assert_eq!(reg.plan_state(old), Some(PlanState::Superseded));
    let slot = quarantine_slot(&reg, old);
    let entry = reg.quarantine().entry(slot).expect("entry");
    assert_eq!(entry.epoch_id, 11);
    assert_eq!(entry.plan_version, old);
    assert_eq!(entry.old_plan_ptr, reg.arena_ptr(old).expect("old ptr"));
    assert_eq!(entry.remaining_mask, 0xF);
    assert_eq!(reg.quarantine().in_use(), 1);
    // 旧 arena 仍由 registry 持有（RT 只拿裸地址；无提前 unmap）。
    assert_eq!(reg.arena_strong_count(old), Some(1));
    assert_eq!(reg.stats().quarantine_entered, 1);
}

#[test]
fn t20_swap_quarantine_tracks_already_acked_workers() {
    let (mut reg, ctl, mut ack, old) = setup_current_v1();
    let version = admit_fixture(&mut reg, &Fixture::with_graph(0x62));
    let token = reg.stage(version).expect("stage");
    assert!(reg.arm(token, &ctl, &mut ack, 4).is_ok());
    assert!(commit_swap_at_block_boundary(&ctl).is_ok());
    // 提交后、complete 前，worker 0 已在新版本上 ack：quarantine 初始剩余位应扣掉它。
    assert!(ack.ack_worker_version(0, version).is_ok());
    assert!(reg.complete_swap(&ctl, &ack).is_ok());
    let slot = quarantine_slot(&reg, old);
    assert_eq!(reg.quarantine().remaining_mask(slot), Some(0b1110));
}

#[test]
fn t20_plan_state_lifecycle() {
    let (mut reg, ctl, mut ack, first) = setup_current_v1();
    assert_eq!(reg.plan_state(first), Some(PlanState::Committed));
    let second = admit_fixture(&mut reg, &Fixture::with_graph(0x63));
    assert_eq!(reg.plan_state(second), Some(PlanState::Admitted));
    let token = reg.stage(second).expect("stage");
    assert_eq!(reg.plan_state(second), Some(PlanState::Staged));
    assert!(reg.arm(token, &ctl, &mut ack, 2).is_ok());
    assert!(commit_swap_at_block_boundary(&ctl).is_ok());
    assert!(reg.complete_swap(&ctl, &ack).is_ok());
    assert_eq!(reg.plan_state(second), Some(PlanState::Committed));
    assert_eq!(reg.plan_state(first), Some(PlanState::Superseded));
}

#[test]
fn t20_swap_acked_version_updated_by_poll() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x64), 1);
    assert_eq!(reg.swap_acked_version(), None);
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    assert_eq!(reg.swap_acked_version(), None);
    let block = all_ack_block(new, RT_WORKERS);
    assert!(ack.note_block_all_ack(&block).is_ok());
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    assert_eq!(reg.swap_acked_version(), Some(new));
    assert!(reg.retire_gate(old).is_some());
}

#[test]
fn t20_retire_gate_reports_each_condition() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x65), 5);
    let gate = reg.retire_gate(old).expect("gate");
    assert_eq!(gate.version, old);
    assert!(gate.switch_idle, "complete_swap 后 switch_state 已回 IDLE");
    assert!(!gate.swap_all_ack);
    assert!(!gate.old_blocks_terminal);
    assert!(!gate.quarantine_all_ack);
    assert!(!gate.ready);
    // quarantine 记录与 new 版本标签可复算。
    let entry = reg
        .quarantine()
        .entry(quarantine_slot(&reg, old))
        .expect("entry");
    assert_eq!(entry.plan_version, old);
    assert_eq!(entry.remaining_mask, 0xF);
    assert_eq!(reg.arena_strong_count(new), Some(1));
}

#[test]
fn t20_retire_requires_swap_all_ack() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let _new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x66), 6);
    let slot = quarantine_slot(&reg, old);
    // 清 quarantine 位 + 确认 block 终态，但没有 swap all-ack（RG-2 未满足）。
    let mut worker = 0u8;
    while worker < RT_WORKERS {
        assert_eq!(
            reg.quarantine().note_ack(slot, worker, old, 6),
            RtStatus::OK
        );
        worker += 1;
    }
    assert!(reg.confirm_old_blocks_terminal(old).is_ok());
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    let gate = reg.retire_gate(old).expect("gate");
    assert!(!gate.swap_all_ack);
    assert!(gate.quarantine_all_ack);
    assert!(gate.old_blocks_terminal);
    assert!(!gate.ready);
    assert_eq!(reg.stats().unmapped, 0);
}

#[test]
fn t20_retire_requires_quarantine_all_ack() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x67), 7);
    let block = all_ack_block(new, RT_WORKERS);
    assert!(ack.note_block_all_ack(&block).is_ok());
    assert!(reg.confirm_old_blocks_terminal(old).is_ok());
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    let gate = reg.retire_gate(old).expect("gate");
    assert!(gate.swap_all_ack);
    assert!(!gate.quarantine_all_ack);
    assert_eq!(reg.stats().unmapped, 0);
}

#[test]
fn t20_retire_requires_old_blocks_terminal() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x68), 8);
    let slot = quarantine_slot(&reg, old);
    let block = all_ack_block(new, RT_WORKERS);
    assert!(note_block_switched(&ack, reg.quarantine(), slot, &block, old, 8, new).is_ok());
    assert_eq!(reg.quarantine().remaining_mask(slot), Some(0));
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    let gate = reg.retire_gate(old).expect("gate");
    assert!(gate.swap_all_ack);
    assert!(gate.quarantine_all_ack);
    assert!(
        !gate.old_blocks_terminal,
        "RG-3a 必须由 T09/T16 快照显式确认"
    );
    assert!(!gate.ready);
    assert_eq!(reg.stats().unmapped, 0);
}

#[test]
fn t20_retire_full_path_releases_old_plan() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x69), 9);
    let slot = quarantine_slot(&reg, old);
    let block = all_ack_block(new, RT_WORKERS);
    assert!(note_block_switched(&ack, reg.quarantine(), slot, &block, old, 9, new).is_ok());
    assert!(ack.is_retire_ready());
    assert!(reg.confirm_old_blocks_terminal(old).is_ok());
    assert_eq!(reg.poll_retire(&ctl, &ack), Some(old));
    assert_eq!(reg.arena_strong_count(old), None);
    assert_eq!(reg.plan_state(old), None);
    assert!(!reg.quarantine().is_occupied(slot));
    assert_eq!(reg.stats().unmapped, 1);
    assert_eq!(reg.stats().retired, 1);
    assert_eq!(reg.stats().quarantine_released, 1);
    assert_eq!(reg.arena_strong_count(new), Some(1));
    assert_eq!(reg.current_version(), Some(new));
}

#[test]
fn t20_rt_path_does_not_touch_arc_count() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x6A), 10);
    let slot = quarantine_slot(&reg, old);
    assert_eq!(reg.arena_strong_count(old), Some(1));
    let block = all_ack_block(new, RT_WORKERS);
    // 全部 RT 入口（&TspiSwapAckV1/&GraphQuarantine/&TspiBlockAckV1）都不改变强引用计数。
    assert_eq!(ack.ack_worker(0), RtStatus::OK);
    assert_eq!(
        note_worker_switched(&ack, reg.quarantine(), slot, 0, old, 10, new),
        RtStatus::OK
    );
    assert_eq!(
        note_block_switched(&ack, reg.quarantine(), slot, &block, old, 10, new),
        RtStatus::OK
    );
    assert_eq!(reg.arena_strong_count(old), Some(1));
    assert_eq!(reg.arena_strong_count(new), Some(1));
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    assert_eq!(reg.arena_strong_count(old), Some(1));
}

#[test]
fn t20_lost_worker_keeps_old_plan_forever() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x6B), 12);
    let slot = quarantine_slot(&reg, old);
    // worker 0..2 已切到新版本；worker 3 失联（永不 ack）。
    let mut worker = 0u8;
    while worker < 3 {
        assert_eq!(
            note_worker_switched(&ack, reg.quarantine(), slot, worker, old, 12, new),
            RtStatus::OK
        );
        worker += 1;
    }
    assert!(!ack.is_retire_ready());
    assert_eq!(ack.ack_mask_value(), 0b0111);
    assert_eq!(reg.quarantine().remaining_mask(slot), Some(0b1000));
    assert!(reg.confirm_old_blocks_terminal(old).is_ok());
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    assert_eq!(reg.arena_strong_count(old), Some(1));
    assert!(!reg.quarantine().try_release(slot).is_ok());
    assert_eq!(reg.stats().unmapped, 0);
    assert_eq!(reg.stats().retired, 0);
    // 到最后也永不回收（INV-L5：失联 worker 的槽保留到进程重启）。
    assert_eq!(reg.collect_retired(), None);
    assert_eq!(reg.arena_strong_count(old), Some(1));
}

#[test]
fn t20_quarantine_capacity_blocks_arm() {
    let (mut reg, ctl, mut ack, _old) = setup_current_v1();
    let mut version = 100u32;
    while version < 100 + GRAPH_QUARANTINE_SLOTS as u32 {
        assert!(reg
            .quarantine()
            .enter(1, version, 0x1000 + version as u64, RT_WORKERS)
            .is_ok());
        version += 1;
    }
    assert_eq!(reg.quarantine().free_slot_count(), 0);
    let next = admit_fixture(&mut reg, &Fixture::with_graph(0x6C));
    let token = reg.stage(next).expect("stage");
    assert_eq!(
        reg.arm(token, &ctl, &mut ack, 1),
        RtStatus::ERR_QUARANTINE_EXHAUSTED
    );
    assert!(!reg.is_armed());
}

#[test]
fn t20_collect_retired_uses_poll_mirrors() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new = swap_current(&mut reg, &mut ctl, &mut ack, &Fixture::with_graph(0x6D), 13);
    let slot = quarantine_slot(&reg, old);
    let block = all_ack_block(new, RT_WORKERS);
    assert!(note_block_switched(&ack, reg.quarantine(), slot, &block, old, 13, new).is_ok());
    // 先 poll 一次建立 RG-1/RG-2 镜像（此时 RG-3a 未确认 -> None）。
    assert_eq!(reg.poll_retire(&ctl, &ack), None);
    assert!(reg.confirm_old_blocks_terminal(old).is_ok());
    // collect_retired 用镜像评估并完成回收（冻结 API 无 ctl/ack 参数）。
    assert_eq!(reg.collect_retired(), Some(old));
    assert_eq!(reg.arena_strong_count(old), None);
}

#[test]
fn t20_retire_gate_unknown_version_returns_none() {
    let (mut reg, _ctl, _ack, _old) = setup_current_v1();
    assert!(reg.retire_gate(999).is_none());
    assert_eq!(
        reg.confirm_old_blocks_terminal(999),
        RtStatus::ERR_PLAN_STALE
    );
}

// ---------------------------------------------------------------------------
// G. crossfade 双图/双历史预算与 D 对齐（§5.3 I06/§7.4/§11.13）
// ---------------------------------------------------------------------------

fn cf_fixture(graph_seed: u8) -> Fixture {
    let mut fixture = Fixture::crossfade();
    fixture.header.graph_id = [graph_seed; 32];
    fixture.token_graph_id = [graph_seed; 32];
    fixture
}

fn cf_fixture_delay(graph_seed: u8, path_delay: u32, role_delay: u32, diag_delay: u32) -> Fixture {
    let mut fixture = Fixture::crossfade_with_delay(path_delay, role_delay, diag_delay);
    fixture.header.graph_id = [graph_seed; 32];
    fixture.token_graph_id = [graph_seed; 32];
    fixture
}

fn crossfade_budget_for(
    reg: &PlanRegistry,
    old: PlanVersion,
    fixture: &Fixture,
    frames: u32,
) -> TspiCrossfadeBudgetV1 {
    let env = base_env();
    let old_view = match reg.plan_view(old) {
        Some(view) => view,
        None => panic!("old plan missing"),
    };
    let old_memory = admission::plan_memory(&old_view, &env).expect("old memory");
    let new_memory = admission::plan_memory(&fixture.view(), &env).expect("new memory");
    let ram_lower = old_memory.graph_current_bytes + new_memory.graph_current_bytes;
    let min_cpu = t_block_ns_ceil(FS, BLOCK);
    let total = fixture.uniform_delay_q16() as u32;
    TspiCrossfadeBudgetV1::new(
        0,
        0,
        total,
        total,
        frames,
        min_cpu.saturating_mul(2),
        ram_lower.saturating_mul(2),
        min_cpu.saturating_mul(8),
        ram_lower.saturating_mul(4),
    )
}

#[test]
fn t20_crossfade_budget_validate_structure() {
    let mut budget = TspiCrossfadeBudgetV1::new(1, 2, 0, 0, BLOCK as u32, 1, 1, 1, 1);
    assert!(budget.validate().is_ok());
    budget.flags = 4;
    assert_eq!(budget.validate(), ERR_CROSSFADE_BUDGET);
    budget.flags = 0;
    budget.reserved = 1;
    assert_eq!(budget.validate(), ERR_CROSSFADE_BUDGET);
    budget.reserved = 0;
    budget.crossfade_frames = 0;
    assert_eq!(budget.validate(), ERR_CROSSFADE_BUDGET);
    budget.crossfade_frames = CROSSFADE_MAX_FRAMES + 1;
    assert_eq!(budget.validate(), ERR_CROSSFADE_BUDGET);
    budget.crossfade_frames = BLOCK as u32;
    assert!(budget.peaks_within_budget());
    budget.peak_cpu_ns = 2;
    budget.available_cpu_ns = 1;
    assert!(!budget.peaks_within_budget());
    budget.available_cpu_ns = 0;
    assert!(!budget.peaks_within_budget());
    assert!(!budget.admission_passed());
    assert!(!budget.d_aligned());
    budget.mark_admitted(3, 4);
    assert_eq!(budget.old_plan_version, 3);
    assert_eq!(budget.new_plan_version, 4);
    assert!(budget.admission_passed());
    assert!(budget.d_aligned());
}

#[test]
fn t20_normal_admit_rejects_crossfade_flagged_plan() {
    let mut reg = PlanRegistry::new();
    let status = reg
        .admit_with(
            Fixture::crossfade().token(),
            &base_env(),
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
    assert!(reg.admitted_versions().is_empty());
}

#[test]
fn t20_crossfade_rejects_plan_without_flag() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let plain = Fixture::with_graph(0x71);
    let mut budget = crossfade_budget_for(&reg, old, &plain, BLOCK as u32);
    let status = reg
        .admit_crossfade_with(old, plain.token(), &mut budget, &mut MockMemory::new())
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
    assert_eq!(reg.stats().crossfade_rejected, 1);
    assert!(!budget.admission_passed());
}

#[test]
fn t20_crossfade_accepts_aligned_budget() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x72);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let new_version = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .expect("crossfade admit");
    assert_eq!(new_version, old + 1);
    assert!(budget.admission_passed());
    assert!(budget.d_aligned());
    assert_eq!(budget.old_plan_version, old);
    assert_eq!(budget.new_plan_version, new_version);
    assert_eq!(reg.stats().crossfade_accepted, 1);
    assert_eq!(reg.plan_state(new_version), Some(PlanState::Admitted));
    // 双图同时在场：旧 plan 仍持有，未因 crossfade 提前释放。
    assert_eq!(reg.arena_strong_count(old), Some(1));
    assert_eq!(reg.arena_strong_count(new_version), Some(1));
}

#[test]
fn t20_crossfade_rejects_d_mismatch() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture_delay(0x73, 6, 0, 0);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RT_E_CROSSFADE_ALIGN);
    assert!(!budget.admission_passed());
}

#[test]
fn t20_crossfade_rejects_rate_change() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let mut new_fixture = cf_fixture(0x74);
    new_fixture.header.sample_rate = 96_000;
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RT_E_CROSSFADE_ALIGN);
}

#[test]
fn t20_crossfade_rejects_block_change() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let mut new_fixture = cf_fixture(0x75);
    new_fixture.header.block_size = 64;
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, 64);
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RT_E_CROSSFADE_ALIGN);
}

#[test]
fn t20_crossfade_rejects_channels_change() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let mut new_fixture = cf_fixture(0x76);
    new_fixture.header.n_out = 3;
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RT_E_CROSSFADE_ALIGN);
}

#[test]
fn t20_crossfade_rejects_frames_mismatch() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x77);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, (BLOCK / 2) as u32);
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RT_E_CROSSFADE_ALIGN);
}

#[test]
fn t20_crossfade_rejects_declared_d_total_mismatch() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x78);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.old_total_delay_q16 = 7;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RT_E_CROSSFADE_ALIGN);
}

#[test]
fn t20_crossfade_rejects_peak_ram_over_available() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x79);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.available_ram_bytes = budget.peak_ram_bytes / 2;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn t20_crossfade_rejects_peak_ram_below_lower_bound() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x7A);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.peak_ram_bytes = 1;
    budget.available_ram_bytes = u64::MAX / 2;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn t20_crossfade_rejects_peak_cpu_over_available() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x7B);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.available_cpu_ns = budget.peak_cpu_ns / 2;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn t20_crossfade_rejects_peak_cpu_below_block_window() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x7C);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.peak_cpu_ns = 1;
    budget.available_cpu_ns = u64::MAX / 2;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn t20_crossfade_rejects_available_zero() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x7D);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.available_ram_bytes = 0;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.available_cpu_ns = 0;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn t20_crossfade_rejects_when_old_not_current() {
    let (mut reg, mut ctl, mut ack, first) = setup_current_v1();
    let second = admit_fixture(&mut reg, &Fixture::with_graph(0x7E));
    let _ = first;
    let new_fixture = cf_fixture(0x7F);
    let mut budget = crossfade_budget_for(&reg, second, &new_fixture, BLOCK as u32);
    let status = reg
        .admit_crossfade_with(
            second,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RtStatus::ERR_PLAN_STALE);
    let _ = (&mut ctl, &mut ack);
}

#[test]
fn t20_crossfade_rejects_while_switch_busy() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let next = admit_fixture(&mut reg, &Fixture::with_graph(0x80));
    let _token = reg.stage(next).expect("stage");
    let new_fixture = cf_fixture(0x81);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, RtStatus::ERR_SWITCH_BUSY);
    let _ = (&mut ctl, &mut ack);
}

#[test]
fn t20_crossfade_rejects_unknown_budget_flags() {
    let (mut reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x82);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    budget.flags = 0x80;
    let status = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .unwrap_err();
    assert_eq!(status, ERR_CROSSFADE_BUDGET);
}

#[test]
fn t20_crossfade_validate_helper_returns_check() {
    let (reg, _ctl, _ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x83);
    let budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let old_arena = match reg.arc(old) {
        Some(arena) => arena.as_ref(),
        None => panic!("old arena"),
    };
    let new_arena = parse_plan_arena(&new_fixture.token(), 2).expect("parse new");
    let check = validate_crossfade(old_arena, &new_arena, &budget).expect("crossfade valid");
    assert_eq!(check.old_total_delay_q16, 0);
    assert_eq!(check.new_total_delay_q16, 0);
    assert_eq!(
        check.peak_ram_bytes_lower_bound,
        crossfade_peak_ram_lower_bound(old_arena, &new_arena)
    );
    assert_eq!(check.min_cpu_ns, t_block_ns_ceil(FS, BLOCK));
    assert!(check.peak_ram_bytes_lower_bound > 0);
}

#[test]
fn t20_crossfade_then_commit_and_retire_keeps_d_until_ack() {
    let (mut reg, mut ctl, mut ack, old) = setup_current_v1();
    let new_fixture = cf_fixture(0x84);
    let mut budget = crossfade_budget_for(&reg, old, &new_fixture, BLOCK as u32);
    let new = reg
        .admit_crossfade_with(
            old,
            new_fixture.token(),
            &mut budget,
            &mut MockMemory::new(),
        )
        .expect("crossfade admit");
    let token = reg.stage(new).expect("stage");
    assert!(reg.arm(token, &mut ctl, &mut ack, 21).is_ok());
    assert_eq!(commit_swap_at_block_boundary(&ctl).expect("commit"), new);
    assert_eq!(reg.complete_swap(&ctl, &ack).expect("complete"), new);
    // crossfade tail 期间两张图同时在 RAM：旧图仍持有，swap ack 未到前不得释放。
    assert_eq!(reg.arena_strong_count(old), Some(1));
    assert_eq!(reg.arena_strong_count(new), Some(1));
    assert!(reg.quarantine().find(old).is_some());
    let slot = quarantine_slot(&reg, old);
    let block = all_ack_block(new, RT_WORKERS);
    assert!(note_block_switched(&ack, reg.quarantine(), slot, &block, old, 21, new).is_ok());
    assert!(reg.confirm_old_blocks_terminal(old).is_ok());
    assert_eq!(reg.poll_retire(&ctl, &ack), Some(old));
    assert_eq!(reg.arena_strong_count(old), None);
    assert_eq!(reg.current_version(), Some(new));
}
