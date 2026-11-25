//! [NONRT] plan 注册/切换/退役 + [RT] swap-ack / graph-quarantine 原语
//! （core-v1 §4.4/§5.3/§7.3-§7.6/§11.13/§13.6）——WBS T20 写域。
//!
//! 【分区纪律】本文件是**混合模块**（同文件同时含 [RT] 与 [NONRT]），按 README §6 不写纯 RT
//! 模块标记，由代码评审 + drop-audit 类型清单覆盖：
//! - [RT] 区（commit_swap_at_block_boundary 与 TspiSwapAckV1/GraphQuarantine 方法）：只读写
//!   固定大小原子字段和"发布后只读"的 plain 字段；禁止分配/锁/syscall/日志/格式化/panic/析构
//!   （core-v1 §9.3/§10、§13.3 R-DROP-01..14）。
//! - [NONRT] 区（PlanRegistry/PlanArena）：唯一持有 plan arena 与 Arc 强引用；unmap/释放只在
//!   retire gate RG-1..RG-4（§7.5）全过后发生。RT 不可见任何 owned 句柄（§13.2）。
//!
//! 【本任务实现的契约行】
//! 1. **block-version + 4 worker ack（§7.3/§11.13 TspiSwapAckV1）**：master 在 block 边界把
//!    current_plan_ptr/version 原子替换为 pending（commit_swap_at_block_boundary）；随后只有
//!    "新 plan_version 上的 all-ack block" 才把该 block 的 worker 位图 OR 进 swap ack
//!    （TspiSwapAckV1::note_block_all_ack），4 位到齐才 retire_ready=1（RG-2）。
//! 2. **迟到 partial/graph quarantine 直到 all-ack（§7.6/INV-L5/R-DROP-06）**：旧 plan arena 在
//!    切换完成时进入 GraphQuarantine（固定 QUARANTINE_SLOTS=8 槽），剩余 worker 位图由迟到
//!    worker 显式 ack 清零；未 all-ack 前 try_release 恒失败、arena 永不回收；worker 失联则
//!    永不回收（等进程重启，安全优先）。
//! 3. **nonRT 回收（§4.2/§7.5/RG-4）**：PlanRegistry::poll_retire / collect_retired 只在
//!    RG-1（switch_state 回 IDLE）+ RG-2（swap all-ack）+ RG-3（旧版本 block 全部终态且
//!    quarantine all-ack；由 T09/T16 owner 快照确认）+ 显式 RG-4 确认后，由 control 线程把
//!    Arc 强引用降到 0 并释放（unmap 计数器 +1）。
//! 4. **无 RT 最后 Arc drop（§13.4 Lemma 2/R-DROP-05）**：[RT] 入口只接收 &TspiSwapAckV1 /
//!    &GraphQuarantine / &TspiRtControlV1 并返回 RtStatus/Option<PlanVersion> 等 Copy 值；swap
//!    ack 只携带旧 arena 裸地址 old_plan_ptr，从不 clone/drop Arc（测试以
//!    PlanRegistry::arena_strong_count 断言 RT 全程不改变强引用计数）。
//! 5. **crossfade 双 CPU/RAM 预算拒绝（§5.3 I06/§7.4/§11.13 TspiCrossfadeBudgetV1）**：仅当
//!    plan flags bit2=1、新旧 rate/N/O/B 相同、逐输出 total_delay_q16 严格相等、crossfade 帧数
//!    == B 且 <=128、双图 RAM 下界与峰值 CPU 均 <= 可用量且声明值不低于下界时才接受；预算不足
//!    -> ERR_CROSSFADE_BUDGET(123)，D 不可对齐 -> RT_E_CROSSFADE_ALIGN(309)（§9.2）。
//! 6. **plan flags 位表（§11.10；core-v1 v1.1.3 errata E4）**：bit0 stereo_strict、bit1
//!    modal_support_to_other_outputs、bit2 crossfade、bit3 extended_taps_admitted、bit4
//!    fallback_graph、bit5..7=0（reserved）；parse_plan_arena 对未知/保留位置位一律
//!    ERR_BAD_LAYOUT 拒收（PLAN_FLAG_MASK=0x1F；旧 0x3F 放过 bit5 的尾巴已按 E4 收窄）。
//!
//! 【契约缺口登记（不改他人写域，报 integrator/Lead）】
//! - TspiSwapAckV1 / TspiCrossfadeBudgetV1（§11.13）尚未进入 core::abi；本 leaf 先在
//!   rt-host::swap 定义（布局与 §11.13 逐字节一致），待 integrator 提升后本模块改为 re-export。
//! - ERR_CROSSFADE_BUDGET(123) / RT_E_CROSSFADE_ALIGN(309)（§9.2）尚未进入 core::rt；
//!   本 leaf 以本地常量提供，码值按 §9.2 冻结。
//! - core::rt 未导出 SWITCH_SWITCHED(2)/SWITCH_ABORT(3)；本 leaf 补 2/3 常量（与 §11.13
//!   switch_state 编码一致，3 与 workers::WORKER_SWITCH_ABORT 同值）。
//! - TspiPathV1/TspiDiagFirV1/TspiRoleV1 当前 ABI v1.0 尚无 delay_q16（§11.10 v1.1）；本 leaf
//!   的 total_delay_q16 按 delay_samples*65536 + delay_q16 口径实现，delay_q16 暂取 0；一旦
//!   integrator 补齐 ABI 字段，仅需改 plan_total_delay_q16 一处。
//! - coeff blob 无 header 偏移字段：本 leaf 约定 coeff blob 位于 plan 字节尾部
//!   （coeff_base = bytes.len() - total_coeff_bytes，表/元数据只能在其前），供 T12/T26 打包对齐。

pub use khaen_core::plan::{PlanToken, StageToken};
pub use khaen_core::rt::{PlanVersion, RtControl, RtStatus};

use core::mem::{align_of, needs_drop, size_of};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use khaen_core::abi::{
    plan_abi_supported, TspiBlockAckV1, TspiDiagFirV1, TspiPathV1, TspiPlanHeaderV1,
    TspiPlanIirHeaderV2, TspiRoleIirV2, TspiRoleV1, ABI_VERSION, PLAN_IIR_ENTRY_BYTES,
    PLAN_IIR_HEADER_BYTES, PLAN_IIR_MAX_SECTIONS, PLAN_IIR_SECTION_COEFFS, TSPI_PLAN_ABI_V1,
    TSPI_PLAN_IIR_MAGIC, TSPI_PLAN_IIR_VERSION, TSPI_PLAN_MAGIC,
    TSPI_RING_MAGIC,
};
use khaen_core::rt::{
    t_block_ns_ceil, PlanView, BLOCK_SIZES, MAX_DIAG_FIRS, MAX_INPUT_CHANNELS, MAX_MATRIX_PATHS,
    MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, QUARANTINE_SLOTS, RT_WORKERS, RT_WORKERS_FALLBACK,
    SAMPLE_RATES_HZ, SWITCH_ARMED, SWITCH_IDLE, TAPS_EXTENDED_MAX, TAPS_NORMAL_MAX,
};
use khaen_core::workers::{ack_mask, TspiRtControlV1};

use crate::admission::{self, AdmissionEnv, AdmissionReport, MemoryBackend, PlatformMemory};

// ---------------------------------------------------------------------------
// 常量（core-v1 §3.3/§3.4/§9.2/§11.10/§11.13；缺口项见文件头登记）
// ---------------------------------------------------------------------------

/// plan 切换状态：SWITCHED（pending 已在 block 边界提交；§11.13 switch_state=2）。
pub const SWITCH_SWITCHED: u32 = 2;
/// plan 切换状态：ABORT（切换被中止；§11.13 switch_state=3，与 WORKER_SWITCH_ABORT 同值）。
pub const SWITCH_ABORT: u32 = 3;

/// TspiSwapAckV1 字节数（core-v1 §11.13）。
pub const SWAP_ACK_BYTES: usize = 64;
/// TspiCrossfadeBudgetV1 字节数（core-v1 §11.13）。
pub const CROSSFADE_BUDGET_BYTES: usize = 64;
/// 旧图 quarantine 槽数（core-v1 §3.3/§7.6）。
pub const GRAPH_QUARANTINE_SLOTS: usize = QUARANTINE_SLOTS;
/// crossfade 最大帧数（=B 且 <=128；core-v1 §3.3 CROSSFADE_MAX_FRAMES）。
pub const CROSSFADE_MAX_FRAMES: u32 = 128;
/// plan header flags bit2=crossfade（core-v1 §11.10）。
pub const PLAN_FLAG_CROSSFADE: u8 = 1 << 2;
/// plan header flags bit3=extended_taps_admitted（core-v1 §11.10）。
pub const PLAN_FLAG_EXTENDED_TAPS_ADMITTED: u8 = 1 << 3;
/// plan header flags 已知位掩码（bit0..bit4；bit5..7=0，core-v1 v1.1.3 errata E4/§11.10）。
pub const PLAN_FLAG_MASK: u8 = 0x1F;
/// TspiCrossfadeBudgetV1.flags bit0=admission_passed（core-v1 §11.13）。
pub const CROSSFADE_FLAG_ADMISSION_PASSED: u32 = 1 << 0;
/// TspiCrossfadeBudgetV1.flags bit1=d_aligned（core-v1 §11.13）。
pub const CROSSFADE_FLAG_D_ALIGNED: u32 = 1 << 1;
/// TspiCrossfadeBudgetV1.flags 已知位掩码。
pub const CROSSFADE_FLAG_MASK: u32 = CROSSFADE_FLAG_ADMISSION_PASSED | CROSSFADE_FLAG_D_ALIGNED;
/// TspiSwapAckV1.flags bit0=retire_ready（§11.13；权威值是 retire_ready 原子）。
pub const SWAP_ACK_FLAG_RETIRE_READY: u8 = 1 << 0;
/// TspiSwapAckV1.flags 已知位掩码。
pub const SWAP_ACK_FLAG_MASK: u8 = SWAP_ACK_FLAG_RETIRE_READY;
/// crossfade 预算不足拒绝码（core-v1 §9.2 code 123；待 integrator 提升到 core::rt）。
pub const ERR_CROSSFADE_BUDGET: RtStatus = RtStatus::from_code(123);
/// crossfade D 不可对齐/跨 profile 拒绝码（core-v1 §9.2 code 309；待 integrator 提升）。
pub const RT_E_CROSSFADE_ALIGN: RtStatus = RtStatus::from_code(309);

/// 旧图 quarantine 槽状态：FREE（可分配）。
pub const GRAPH_QUARANTINE_FREE: u32 = 0;
/// 旧图 quarantine 槽状态：OCCUPIED（字段已发布，等待迟到 worker ack）。
pub const GRAPH_QUARANTINE_OCCUPIED: u32 = 1;
/// 旧图 quarantine 槽状态：INIT（已认领、字段尚未发布；外部不可见）。
pub const GRAPH_QUARANTINE_INIT: u32 = 2;

/// Q16 次采样刻度（core-v1 §3.4 DELAY_Q16_SCALE）。
pub const DELAY_Q16_SCALE: u64 = 65_536;

/// 每 worker 的 plan phase 数（core-v1 §3.3 PARALLEL_PHASES=3）。
const SWAP_ACK_PHASES: usize = 3;

/// expected workers 是否合法（2 核独立 profile / 4 核生产；§3.3）。
const fn expected_valid(expected_workers: u8) -> bool {
    expected_workers == RT_WORKERS || expected_workers == RT_WORKERS_FALLBACK
}

/// total_delay_q16 = delay_samples*65536 + delay_q16（core-v1 §12.3/§11.10 注 6）。
pub const fn total_delay_q16(delay_samples: u32, delay_q16: u32) -> u64 {
    ((delay_samples as u64) << 16) | (delay_q16 as u64)
}

// ---------------------------------------------------------------------------
// TspiSwapAckV1（64 B / align 64；core-v1 §11.13）
// ---------------------------------------------------------------------------

/// RT 可见的 swap all-worker ack（§11.13；不是 Copy，needs_drop==false）。
///
/// 生命周期：[NONRT] init（写 plain 字段 + 清零原子）-> Release 发布 switch_state=ARMED ->
/// [RT] master 在 block 边界提交后按 block 的 all-ack 位图 OR 入 ack_mask -> 4 位到齐置
/// retire_ready -> [NONRT] control 用它做 RG-2 判定 -> retire 完成后 reset 供下一次切换复用。
/// plain 字段只允许在 RD-2 发布（ARMED）之前由 NONRT 写；RT 只写 ack_mask/retire_ready 原子。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct TspiSwapAckV1 {
    /// 待 ack 的 plan（= 新版本）。
    pub plan_version: u32,
    /// expected workers（2/4；all-ack 用 ACK_MASK(expected)）。
    pub expected_workers: u8,
    /// bit0=retire_ready（NONRT 镜像；权威看 retire_ready 原子）。
    pub flags: u8,
    /// 必须为 0。
    pub reserved0: u16,
    /// worker k 完成"新版本首块"的位图（Release 发布）。
    pub ack_mask: AtomicU32,
    /// 1=所有 worker ack 已到，可由 CTL retire（Release 发布）。
    pub retire_ready: AtomicU32,
    /// 旧 plan 使用结束的 epoch。
    pub retire_epoch: u64,
    /// 当前生效版本（= plan_version）。
    pub next_plan_version: u32,
    /// 必须为 0。
    pub reserved1: u32,
    /// 旧 arena 基址（retire 目标；RT 只读该数值，不解引用 owned 类型）。
    pub old_plan_ptr: u64,
    /// 必须为 0。
    pub reserved2: u64,
    /// 必须为 0。
    pub reserved3: u64,
    /// 必须为 0。
    pub reserved4: u64,
}

impl TspiSwapAckV1 {
    /// 全零构造（admission 一次分配/测试；未 init 前 validate 拒绝）。
    pub const fn new() -> Self {
        Self {
            plan_version: 0,
            expected_workers: 0,
            flags: 0,
            reserved0: 0,
            ack_mask: AtomicU32::new(0),
            retire_ready: AtomicU32::new(0),
            retire_epoch: 0,
            next_plan_version: 0,
            reserved1: 0,
            old_plan_ptr: 0,
            reserved2: 0,
            reserved3: 0,
            reserved4: 0,
        }
    }

    /// [NONRT] 为一次切换初始化（必须在 switch_state=ARMED Release 之前完成）。
    ///
    /// old_plan_ptr=0 表示首个 plan（无旧图可 retire）。
    pub fn init(
        &mut self,
        plan_version: PlanVersion,
        expected_workers: u8,
        retire_epoch: u64,
        next_plan_version: PlanVersion,
        old_plan_ptr: u64,
    ) -> RtStatus {
        if plan_version == 0 || next_plan_version == 0 {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if plan_version != next_plan_version {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if !expected_valid(expected_workers) {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        self.plan_version = plan_version;
        self.expected_workers = expected_workers;
        self.flags = 0;
        self.reserved0 = 0;
        self.ack_mask.store(0, Ordering::Release);
        self.retire_ready.store(0, Ordering::Release);
        self.retire_epoch = retire_epoch;
        self.next_plan_version = next_plan_version;
        self.reserved1 = 0;
        self.old_plan_ptr = old_plan_ptr;
        self.reserved2 = 0;
        self.reserved3 = 0;
        self.reserved4 = 0;
        RtStatus::OK
    }

    /// [NONRT] 静态一致性校验（expected/flags/reserved/位图/retire_ready）。
    pub fn validate(&self) -> RtStatus {
        if !expected_valid(self.expected_workers) {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        if (self.flags & !SWAP_ACK_FLAG_MASK) != 0 {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        if self.reserved0 != 0
            || self.reserved1 != 0
            || self.reserved2 != 0
            || self.reserved3 != 0
            || self.reserved4 != 0
        {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        let mask = ack_mask(self.expected_workers);
        let acked = self.ack_mask.load(Ordering::Acquire);
        if (acked & !mask) != 0 {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        let ready = self.retire_ready.load(Ordering::Acquire);
        if ready > 1 {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        if ready == 1 && acked != mask {
            return RtStatus::RT_E_INVARIANT;
        }
        if (self.flags & SWAP_ACK_FLAG_RETIRE_READY) != 0 && ready != 1 {
            return RtStatus::RT_E_INVARIANT;
        }
        RtStatus::OK
    }

    /// [RT] worker k 报告"已在至少一个 block 上使用新 plan_version"。
    ///
    /// 只写 ack_mask/retire_ready 原子；全部到齐置 retire_ready=1（Release）。
    pub fn ack_worker(&self, worker: u8) -> RtStatus {
        let expected = self.expected_workers;
        if !expected_valid(expected) {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        if worker >= expected {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        let mask = ack_mask(expected);
        let bit = 1u32 << worker;
        let previous = self.ack_mask.fetch_or(bit, Ordering::AcqRel);
        if (previous & mask) == mask {
            return RtStatus::OK;
        }
        if ((previous | bit) & mask) == mask {
            self.retire_ready.store(1, Ordering::Release);
        }
        RtStatus::OK
    }

    /// [RT] 带版本标签的 worker ack（版本不符 -> RT_E_PLAN_MISMATCH，不写位）。
    pub fn ack_worker_version(&self, worker: u8, plan_version: PlanVersion) -> RtStatus {
        if plan_version != self.plan_version {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        self.ack_worker(worker)
    }

    /// [RT] 用一个"新版本上的 all-ack block"推进 swap ack（RG-2 的唯一 RT 证据）。
    ///
    /// 校验 block 的 expected/version/cutoff/三 phase 位图全部到齐；通过后把这些 worker 记入
    /// swap ack 并置 retire_ready（一个 4 worker all-ack 块即满足"每个 worker 都用过新版本"）。
    pub fn note_block_all_ack(&self, block: &TspiBlockAckV1) -> RtStatus {
        let expected = self.expected_workers;
        if !expected_valid(expected) {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        if block.expected_workers != expected {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if block.plan_version != self.plan_version {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if block.cutoff.load(Ordering::Acquire) != 0 {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        let mask = ack_mask(expected);
        let mut phase = 0usize;
        while phase < SWAP_ACK_PHASES {
            let done = match block.phase_done.get(phase) {
                Some(value) => value.load(Ordering::Acquire),
                None => return RtStatus::ERR_BAD_LAYOUT,
            };
            if (done & !mask) != 0 {
                return RtStatus::RT_E_ABI_RUNTIME;
            }
            if (done & mask) != mask {
                return RtStatus::RT_E_PARTIAL_LATE;
            }
            phase += 1;
        }
        self.ack_mask.fetch_or(mask, Ordering::AcqRel);
        self.retire_ready.store(1, Ordering::Release);
        RtStatus::OK
    }

    /// 已 ack 位图（Acquire）。
    pub fn ack_mask_value(&self) -> u32 {
        self.ack_mask.load(Ordering::Acquire)
    }

    /// 期望位图 ACK_MASK(expected)（expected 非法返回 0）。
    pub fn required_mask(&self) -> u32 {
        if expected_valid(self.expected_workers) {
            ack_mask(self.expected_workers)
        } else {
            0
        }
    }

    /// 是否 all-ack（expected 位到齐）。
    pub fn all_acked(&self) -> bool {
        let mask = self.required_mask();
        mask != 0 && (self.ack_mask_value() & mask) == mask
    }

    /// retire_ready 原子值（Acquire）。
    pub fn retire_ready_value(&self) -> u32 {
        self.retire_ready.load(Ordering::Acquire)
    }

    /// 是否可 retire（RG-2；retire_ready=1 且位图齐全）。
    pub fn is_retire_ready(&self) -> bool {
        self.retire_ready_value() == 1 && self.all_acked()
    }

    /// [NONRT] 把 retire_ready 原子镜像进 flags bit0（导出/诊断用；RT 不写 flags）。
    pub fn sync_retire_ready_flag(&mut self) {
        if self.retire_ready_value() == 1 {
            self.flags |= SWAP_ACK_FLAG_RETIRE_READY;
        } else {
            self.flags &= !SWAP_ACK_FLAG_RETIRE_READY;
        }
    }

    /// [NONRT] 清零以复用下次切换（旧 plan 已 retire 或切换已 abort 之后）。
    pub fn reset(&mut self) {
        self.plan_version = 0;
        self.expected_workers = 0;
        self.flags = 0;
        self.reserved0 = 0;
        self.ack_mask.store(0, Ordering::Release);
        self.retire_ready.store(0, Ordering::Release);
        self.retire_epoch = 0;
        self.next_plan_version = 0;
        self.reserved1 = 0;
        self.old_plan_ptr = 0;
        self.reserved2 = 0;
        self.reserved3 = 0;
        self.reserved4 = 0;
    }
}

impl Default for TspiSwapAckV1 {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TspiCrossfadeBudgetV1（64 B / align 64；core-v1 §11.13 I06）
// ---------------------------------------------------------------------------

/// crossfade 双图/双历史峰值预算（§11.13 I06；不是 Copy，needs_drop==false）。
///
/// 由 NONRT admission（本模块 validate_crossfade / admit_crossfade_with）填写后再由 RT 读取；
/// 字段在 RD-2 发布（ARMED/commit）后只读，RT 不写。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct TspiCrossfadeBudgetV1 {
    /// 旧 plan 版本。
    pub old_plan_version: u32,
    /// 新 plan 版本。
    pub new_plan_version: u32,
    /// 旧 plan 的 total_delay_q16（§11.10 注 6 口径）。
    pub old_total_delay_q16: u32,
    /// 新 plan 的 total_delay_q16；必须相等。
    pub new_total_delay_q16: u32,
    /// crossfade 帧数（=B，<=CROSSFADE_MAX_FRAMES）。
    pub crossfade_frames: u32,
    /// bit0=admission_passed、bit1=d_aligned，其余 0。
    pub flags: u32,
    /// 双图峰值 CPU（ns/块）。
    pub peak_cpu_ns: u64,
    /// 双图峰值 RAM（字节）。
    pub peak_ram_bytes: u64,
    /// admission 可用 CPU 预算（ns）。
    pub available_cpu_ns: u64,
    /// admission 可用 RAM。
    pub available_ram_bytes: u64,
    /// 必须为 0。
    pub reserved: u64,
}

impl TspiCrossfadeBudgetV1 {
    /// 构造未校验预算（flags=0、reserved=0；版本 0 表示调用者暂未知，由 registry 回填）。
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        old_plan_version: u32,
        new_plan_version: u32,
        old_total_delay_q16: u32,
        new_total_delay_q16: u32,
        crossfade_frames: u32,
        peak_cpu_ns: u64,
        peak_ram_bytes: u64,
        available_cpu_ns: u64,
        available_ram_bytes: u64,
    ) -> Self {
        Self {
            old_plan_version,
            new_plan_version,
            old_total_delay_q16,
            new_total_delay_q16,
            crossfade_frames,
            flags: 0,
            peak_cpu_ns,
            peak_ram_bytes,
            available_cpu_ns,
            available_ram_bytes,
            reserved: 0,
        }
    }

    /// [NONRT] 结构自检：flags 未知位/reserved/帧数越界 -> ERR_CROSSFADE_BUDGET。
    pub fn validate(&self) -> RtStatus {
        if (self.flags & !CROSSFADE_FLAG_MASK) != 0 || self.reserved != 0 {
            return ERR_CROSSFADE_BUDGET;
        }
        if self.crossfade_frames == 0 || self.crossfade_frames > CROSSFADE_MAX_FRAMES {
            return ERR_CROSSFADE_BUDGET;
        }
        RtStatus::OK
    }

    /// admission 是否已通过（flags bit0）。
    pub fn admission_passed(&self) -> bool {
        (self.flags & CROSSFADE_FLAG_ADMISSION_PASSED) != 0
    }

    /// D 是否已对齐（flags bit1）。
    pub fn d_aligned(&self) -> bool {
        (self.flags & CROSSFADE_FLAG_D_ALIGNED) != 0
    }

    /// 峰值是否都在可用量内（含可用量>0）。
    pub fn peaks_within_budget(&self) -> bool {
        self.available_cpu_ns != 0
            && self.available_ram_bytes != 0
            && self.peak_cpu_ns <= self.available_cpu_ns
            && self.peak_ram_bytes <= self.available_ram_bytes
    }

    /// [NONRT] 写入 admission 结论（版本号回填到实际分配值）。
    pub fn mark_admitted(&mut self, old_plan_version: u32, new_plan_version: u32) {
        self.old_plan_version = old_plan_version;
        self.new_plan_version = new_plan_version;
        self.flags = CROSSFADE_FLAG_ADMISSION_PASSED | CROSSFADE_FLAG_D_ALIGNED;
    }

    fn snapshot(&self) -> CrossfadeSnapshot {
        CrossfadeSnapshot {
            old_plan_version: self.old_plan_version,
            new_plan_version: self.new_plan_version,
            old_total_delay_q16: self.old_total_delay_q16 as u64,
            new_total_delay_q16: self.new_total_delay_q16 as u64,
            crossfade_frames: self.crossfade_frames,
            flags: self.flags,
            peak_cpu_ns: self.peak_cpu_ns,
            peak_ram_bytes: self.peak_ram_bytes,
            available_cpu_ns: self.available_cpu_ns,
            available_ram_bytes: self.available_ram_bytes,
        }
    }
}

impl Default for TspiCrossfadeBudgetV1 {
    fn default() -> Self {
        Self::new(0, 0, 0, 0, 0, 0, 0, 0, 0)
    }
}

/// 预算字段快照（[NONRT] 内部用；Copy，避免给 ABI 类型加 Copy/Clone）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CrossfadeSnapshot {
    old_plan_version: u32,
    new_plan_version: u32,
    old_total_delay_q16: u64,
    new_total_delay_q16: u64,
    crossfade_frames: u32,
    flags: u32,
    peak_cpu_ns: u64,
    peak_ram_bytes: u64,
    available_cpu_ns: u64,
    available_ram_bytes: u64,
}

/// crossfade 准入的复算结果（[NONRT]；供控制面导出/测试断言）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrossfadeCheck {
    /// 旧 plan 的 total_delay_q16（逐输出最大值口径）。
    pub old_total_delay_q16: u64,
    /// 新 plan 的 total_delay_q16。
    pub new_total_delay_q16: u64,
    /// 双图 RAM 可复算下界（旧 current 图 + 新 current 图）。
    pub peak_ram_bytes_lower_bound: u64,
    /// 双图峰值 CPU 可复算下界（>= 一个 block 窗口 t_block）。
    pub min_cpu_ns: u64,
}

/// 双图 RAM 可复算下界：旧 plan 当前图 + 新 plan 当前图（不含共享 RtArena，故为下界）。
pub fn crossfade_peak_ram_lower_bound(old: &PlanArena, new: &PlanArena) -> u64 {
    old.memory
        .graph_current_bytes
        .saturating_add(new.memory.graph_current_bytes)
}

// ---------------------------------------------------------------------------
// 逐输出 total_delay_q16（crossfade D 对齐口径；core-v1 §5.3/§11.10 注 6）
// ---------------------------------------------------------------------------

/// 逐输出 total_delay_q16 = max(路径 D) + 启用 role 的 D + diag FIR 的 D。
///
/// 注：ABI v1.0 的 path/diag/role 尚无 delay_q16 字段（§11.10 v1.1），此处分量为 0；见文件头
/// 契约缺口登记。越界输出/表项返回 ERR_PATH_TABLE/ERR_ROLES。
pub fn plan_total_delay_q16(plan: &PlanArena) -> Result<[u64; MAX_OUTPUT_CHANNELS], RtStatus> {
    let out_count = plan.header.n_out as usize;
    if out_count == 0 || out_count > MAX_OUTPUT_CHANNELS {
        return Err(RtStatus::ERR_CHANNELS);
    }
    let mut totals = [0u64; MAX_OUTPUT_CHANNELS];
    let mut index = 0usize;
    while index < plan.paths.len() {
        let path = match plan.paths.get(index) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_PATH_TABLE),
        };
        let out = path.out_index as usize;
        if out >= out_count {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        let delay = total_delay_q16(path.delay_samples, 0);
        if delay > totals[out] {
            totals[out] = delay;
        }
        index += 1;
    }
    index = 0usize;
    while index < plan.roles.len() {
        let role = match plan.roles.get(index) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_ROLES),
        };
        index += 1;
        if (role.flags & 1) == 0 || role.out_index == u16::MAX {
            continue;
        }
        let out = role.out_index as usize;
        if out >= out_count {
            return Err(RtStatus::ERR_ROLES);
        }
        totals[out] = match totals[out].checked_add(total_delay_q16(role.delay_samples, 0)) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_ROLES),
        };
    }
    index = 0usize;
    while index < plan.diags.len() {
        let diag = match plan.diags.get(index) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_PATH_TABLE),
        };
        let out = diag.out_index as usize;
        if out >= out_count {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        totals[out] = match totals[out].checked_add(total_delay_q16(diag.delay_samples, 0)) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_BAD_LAYOUT),
        };
        index += 1;
    }
    Ok(totals)
}

fn max_output_delay(profile: &[u64; MAX_OUTPUT_CHANNELS], out_count: usize) -> u64 {
    let mut max = 0u64;
    let mut index = 0usize;
    while index < out_count && index < MAX_OUTPUT_CHANNELS {
        if profile[index] > max {
            max = profile[index];
        }
        index += 1;
    }
    max
}

/// crossfade 预算/对齐校验（[NONRT]；§5.3/§7.4/§11.13 I06）。
///
/// 失败码：结构/预算不足 -> ERR_CROSSFADE_BUDGET(123)；D 不可对齐或跨 rate/N/O/B ->
/// RT_E_CROSSFADE_ALIGN(309)。
pub fn validate_crossfade(
    old: &PlanArena,
    new: &PlanArena,
    budget: &TspiCrossfadeBudgetV1,
) -> Result<CrossfadeCheck, RtStatus> {
    let status = budget.validate();
    if !status.is_ok() {
        return Err(status);
    }
    validate_crossfade_snapshot(old, new, &budget.snapshot())
}

fn validate_crossfade_snapshot(
    old: &PlanArena,
    new: &PlanArena,
    budget: &CrossfadeSnapshot,
) -> Result<CrossfadeCheck, RtStatus> {
    if (budget.flags & !CROSSFADE_FLAG_MASK) != 0
        || budget.crossfade_frames == 0
        || budget.crossfade_frames > CROSSFADE_MAX_FRAMES
    {
        return Err(ERR_CROSSFADE_BUDGET);
    }
    let old_header = &old.header;
    let new_header = &new.header;
    // (1) 跨 rate/N/O/B 变化一律禁止 crossfade（必须停流新 epoch；§5.3）。
    if old_header.sample_rate != new_header.sample_rate
        || old_header.block_size != new_header.block_size
        || old_header.n_in != new_header.n_in
        || old_header.n_out != new_header.n_out
    {
        return Err(RT_E_CROSSFADE_ALIGN);
    }
    // (2) 新 plan 必须显式声明 crossfade（flags bit2；§11.10）。
    if (new_header.flags & PLAN_FLAG_CROSSFADE) == 0 {
        return Err(ERR_CROSSFADE_BUDGET);
    }
    // (3) 版本标签（0=调用者未知，由 registry 回填后二次确认）。
    if budget.old_plan_version != 0 && budget.old_plan_version != old.version {
        return Err(RT_E_CROSSFADE_ALIGN);
    }
    if budget.new_plan_version != 0 && budget.new_plan_version != new.version {
        return Err(RT_E_CROSSFADE_ALIGN);
    }
    // (4) 逐输出 total_delay_q16 严格相等（D 对齐；不等只能硬切+安全静音）。
    let old_profile = plan_total_delay_q16(old)?;
    let new_profile = plan_total_delay_q16(new)?;
    let out_count = new_header.n_out as usize;
    let mut out = 0usize;
    while out < MAX_OUTPUT_CHANNELS {
        if old_profile[out] != new_profile[out] {
            return Err(RT_E_CROSSFADE_ALIGN);
        }
        out += 1;
    }
    let old_max = max_output_delay(&old_profile, out_count);
    let new_max = max_output_delay(&new_profile, out_count);
    if old_max > u32::MAX as u64 || new_max > u32::MAX as u64 {
        return Err(ERR_CROSSFADE_ALIGN_AS_BUDGET);
    }
    // (5) 声明的 total_delay_q16 必须与 plan 复算一致（防调用者低报/高报）。
    if budget.old_total_delay_q16 != 0 && budget.old_total_delay_q16 != old_max {
        return Err(RT_E_CROSSFADE_ALIGN);
    }
    if budget.new_total_delay_q16 != 0 && budget.new_total_delay_q16 != new_max {
        return Err(RT_E_CROSSFADE_ALIGN);
    }
    // (6) crossfade 帧数 == B 且 <=128（更长必须新 profile 重新准入；§3.3）。
    if budget.crossfade_frames != new_header.block_size as u32 {
        return Err(RT_E_CROSSFADE_ALIGN);
    }
    // (7) 双图 RAM/CPU 峰值：声明值不得低于可复算下界，且必须 <= 可用量（fail-safe）。
    let ram_lower = crossfade_peak_ram_lower_bound(old, new);
    let min_cpu = t_block_ns_ceil(new_header.sample_rate, new_header.block_size);
    if budget.available_ram_bytes == 0 || budget.available_cpu_ns == 0 {
        return Err(ERR_CROSSFADE_BUDGET);
    }
    if budget.peak_ram_bytes < ram_lower || budget.peak_ram_bytes > budget.available_ram_bytes {
        return Err(ERR_CROSSFADE_BUDGET);
    }
    if budget.peak_cpu_ns < min_cpu || budget.peak_cpu_ns > budget.available_cpu_ns {
        return Err(ERR_CROSSFADE_BUDGET);
    }
    Ok(CrossfadeCheck {
        old_total_delay_q16: old_max,
        new_total_delay_q16: new_max,
        peak_ram_bytes_lower_bound: ram_lower,
        min_cpu_ns: min_cpu,
    })
}

/// 超 u32 的 total_delay_q16 拒绝为预算错误（§11.13 字段是 u32）。
const ERR_CROSSFADE_ALIGN_AS_BUDGET: RtStatus = ERR_CROSSFADE_BUDGET;

// ---------------------------------------------------------------------------
// GraphQuarantine（固定 8 槽；core-v1 §7.6/INV-L5/R-DROP-06）
// ---------------------------------------------------------------------------

/// 一个 quarantine 槽的快照（诊断/测试用；Copy）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphQuarantineEntry {
    /// 旧图所属 epoch。
    pub epoch_id: u64,
    /// 旧 plan 版本。
    pub plan_version: u32,
    /// 旧 arena 基址。
    pub old_plan_ptr: u64,
    /// expected workers。
    pub expected_workers: u8,
    /// 仍可能使用旧图的 worker 位图（0 = 可释放）。
    pub remaining_mask: u32,
    /// 槽是否被占用。
    pub occupied: bool,
}

/// [RT]/[NONRT] 旧图 quarantine 池（§7.6）。
///
/// 生命周期：master/control 在切换完成时 enter（epoch/version/ptr/expected+remaining）；
/// 迟到 worker 用带标签的 note_ack 清自己的位；只有 remaining==0 时 NONRT try_release 才能
/// 释放槽。worker 失联 -> 该槽永不回收（INV-L5；等进程重启）。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct GraphQuarantine {
    epoch_id: [AtomicU64; GRAPH_QUARANTINE_SLOTS],
    plan_version: [AtomicU32; GRAPH_QUARANTINE_SLOTS],
    old_plan_ptr: [AtomicU64; GRAPH_QUARANTINE_SLOTS],
    expected_workers: [AtomicU32; GRAPH_QUARANTINE_SLOTS],
    remaining: [AtomicU32; GRAPH_QUARANTINE_SLOTS],
    state: [AtomicU32; GRAPH_QUARANTINE_SLOTS],
}

impl GraphQuarantine {
    /// 全 FREE 构造（admission 一次分配；无堆分配）。
    pub const fn new() -> Self {
        Self {
            epoch_id: [const { AtomicU64::new(0) }; GRAPH_QUARANTINE_SLOTS],
            plan_version: [const { AtomicU32::new(0) }; GRAPH_QUARANTINE_SLOTS],
            old_plan_ptr: [const { AtomicU64::new(0) }; GRAPH_QUARANTINE_SLOTS],
            expected_workers: [const { AtomicU32::new(0) }; GRAPH_QUARANTINE_SLOTS],
            remaining: [const { AtomicU32::new(0) }; GRAPH_QUARANTINE_SLOTS],
            state: [const { AtomicU32::new(GRAPH_QUARANTINE_FREE) }; GRAPH_QUARANTINE_SLOTS],
        }
    }

    /// [RT] 把一张旧图放入 quarantine；池满 -> RT_E_QUARANTINE_FULL(306，§9.2)。
    ///
    /// 认领（FREE->INIT CAS）后写标签与剩余位图，最后 Release 发布 OCCUPIED。
    pub fn enter(
        &self,
        epoch_id: u64,
        plan_version: PlanVersion,
        old_plan_ptr: u64,
        expected_workers: u8,
    ) -> Result<usize, RtStatus> {
        if plan_version == 0 {
            return Err(RtStatus::RT_E_PLAN_MISMATCH);
        }
        if !expected_valid(expected_workers) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut index = 0usize;
        while index < GRAPH_QUARANTINE_SLOTS {
            let slot = match self.state.get(index) {
                Some(value) => value,
                None => return Err(RtStatus::ERR_BAD_LAYOUT),
            };
            if slot.load(Ordering::Acquire) == GRAPH_QUARANTINE_FREE
                && slot
                    .compare_exchange(
                        GRAPH_QUARANTINE_FREE,
                        GRAPH_QUARANTINE_INIT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                if let Some(value) = self.epoch_id.get(index) {
                    value.store(epoch_id, Ordering::Relaxed);
                } else {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                if let Some(value) = self.plan_version.get(index) {
                    value.store(plan_version, Ordering::Relaxed);
                } else {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                if let Some(value) = self.old_plan_ptr.get(index) {
                    value.store(old_plan_ptr, Ordering::Relaxed);
                } else {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                if let Some(value) = self.expected_workers.get(index) {
                    value.store(expected_workers as u32, Ordering::Relaxed);
                } else {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                if let Some(value) = self.remaining.get(index) {
                    value.store(ack_mask(expected_workers), Ordering::Relaxed);
                } else {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                slot.store(GRAPH_QUARANTINE_OCCUPIED, Ordering::Release);
                return Ok(index);
            }
            index += 1;
        }
        Err(RtStatus::RT_E_QUARANTINE_FULL)
    }

    /// [RT]/[NONRT] 迟到 worker 确认"旧版本不再被本 worker 使用"（标签必须匹配；幂等）。
    pub fn note_ack(
        &self,
        index: usize,
        worker: u8,
        plan_version: PlanVersion,
        epoch_id: u64,
    ) -> RtStatus {
        if worker >= RT_WORKERS {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        let state = match self.state.get(index) {
            Some(value) => value,
            None => return RtStatus::ERR_BAD_LAYOUT,
        };
        if state.load(Ordering::Acquire) != GRAPH_QUARANTINE_OCCUPIED {
            return RtStatus::RT_E_SLOT_STATE;
        }
        let version = match self.plan_version.get(index) {
            Some(value) => value,
            None => return RtStatus::ERR_BAD_LAYOUT,
        };
        let epoch = match self.epoch_id.get(index) {
            Some(value) => value,
            None => return RtStatus::ERR_BAD_LAYOUT,
        };
        if version.load(Ordering::Acquire) != plan_version {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        if epoch.load(Ordering::Acquire) != epoch_id {
            return RtStatus::RT_E_EPOCH_MISMATCH;
        }
        let expected = match self.expected_workers.get(index) {
            Some(value) => value,
            None => return RtStatus::ERR_BAD_LAYOUT,
        };
        if worker as u32 >= expected.load(Ordering::Acquire) {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        if let Some(value) = self.remaining.get(index) {
            value.fetch_and(!(1u32 << worker), Ordering::AcqRel);
        } else {
            return RtStatus::ERR_BAD_LAYOUT;
        }
        RtStatus::OK
    }

    /// 槽剩余位图（未占用 -> None）。
    pub fn remaining_mask(&self, index: usize) -> Option<u32> {
        match self.state.get(index) {
            Some(value) => {
                if value.load(Ordering::Acquire) != GRAPH_QUARANTINE_OCCUPIED {
                    return None;
                }
                match self.remaining.get(index) {
                    Some(remaining) => Some(remaining.load(Ordering::Acquire)),
                    None => None,
                }
            }
            None => None,
        }
    }

    /// 槽快照（未占用 -> None）。
    pub fn entry(&self, index: usize) -> Option<GraphQuarantineEntry> {
        match self.state.get(index) {
            Some(value) => {
                if value.load(Ordering::Acquire) != GRAPH_QUARANTINE_OCCUPIED {
                    return None;
                }
                let epoch_id = self
                    .epoch_id
                    .get(index)
                    .map(|v| v.load(Ordering::Acquire))?;
                let plan_version = self
                    .plan_version
                    .get(index)
                    .map(|v| v.load(Ordering::Acquire))?;
                let old_plan_ptr = self
                    .old_plan_ptr
                    .get(index)
                    .map(|v| v.load(Ordering::Acquire))?;
                let expected_workers = self
                    .expected_workers
                    .get(index)
                    .map(|v| v.load(Ordering::Acquire))?;
                let remaining_mask = self
                    .remaining
                    .get(index)
                    .map(|v| v.load(Ordering::Acquire))?;
                Some(GraphQuarantineEntry {
                    epoch_id,
                    plan_version,
                    old_plan_ptr,
                    expected_workers: expected_workers as u8,
                    remaining_mask,
                    occupied: true,
                })
            }
            None => None,
        }
    }

    /// 槽是否占用。
    pub fn is_occupied(&self, index: usize) -> bool {
        match self.state.get(index) {
            Some(value) => value.load(Ordering::Acquire) == GRAPH_QUARANTINE_OCCUPIED,
            None => false,
        }
    }

    /// 占用槽数。
    pub fn in_use(&self) -> u32 {
        let mut count = 0u32;
        let mut index = 0usize;
        while index < GRAPH_QUARANTINE_SLOTS {
            if self.is_occupied(index) {
                count += 1;
            }
            index += 1;
        }
        count
    }

    /// FREE 槽数（NONRT 切图前容量检查）。
    pub fn free_slot_count(&self) -> u32 {
        let mut count = 0u32;
        let mut index = 0usize;
        while index < GRAPH_QUARANTINE_SLOTS {
            if let Some(value) = self.state.get(index) {
                if value.load(Ordering::Acquire) == GRAPH_QUARANTINE_FREE {
                    count += 1;
                }
            }
            index += 1;
        }
        count
    }

    /// 按 plan 版本查找槽（诊断/测试）。
    pub fn find(&self, plan_version: PlanVersion) -> Option<usize> {
        let mut index = 0usize;
        while index < GRAPH_QUARANTINE_SLOTS {
            if self.is_occupied(index) {
                if let Some(value) = self.plan_version.get(index) {
                    if value.load(Ordering::Acquire) == plan_version {
                        return Some(index);
                    }
                }
            }
            index += 1;
        }
        None
    }

    /// [NONRT] 释放一个 remaining==0 的槽（未 all-ack -> RT_E_SLOT_STATE；RT 不得调用）。
    pub fn try_release(&self, index: usize) -> Result<PlanVersion, RtStatus> {
        let state = match self.state.get(index) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_BAD_LAYOUT),
        };
        if state.load(Ordering::Acquire) != GRAPH_QUARANTINE_OCCUPIED {
            return Err(RtStatus::RT_E_SLOT_STATE);
        }
        let remaining = match self.remaining.get(index) {
            Some(value) => value.load(Ordering::Acquire),
            None => return Err(RtStatus::ERR_BAD_LAYOUT),
        };
        if remaining != 0 {
            return Err(RtStatus::RT_E_SLOT_STATE);
        }
        let version = match self.plan_version.get(index) {
            Some(value) => value.load(Ordering::Acquire),
            None => return Err(RtStatus::ERR_BAD_LAYOUT),
        };
        if state
            .compare_exchange(
                GRAPH_QUARANTINE_OCCUPIED,
                GRAPH_QUARANTINE_FREE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            Ok(version)
        } else {
            Err(RtStatus::RT_E_SLOT_STATE)
        }
    }

    /// [NONRT] 释放所有 remaining==0 的槽（返回释放数）。
    pub fn release_all_acked(&self) -> u32 {
        let mut released = 0u32;
        let mut index = 0usize;
        while index < GRAPH_QUARANTINE_SLOTS {
            if self.remaining_mask(index) == Some(0) && self.try_release(index).is_ok() {
                released += 1;
            }
            index += 1;
        }
        released
    }

    /// [NONRT] 停机时强制清零（只允许在 Stopped：进程退出前，RT 线程已停）。
    pub fn release_all_force(&self) -> u32 {
        let mut released = 0u32;
        let mut index = 0usize;
        while index < GRAPH_QUARANTINE_SLOTS {
            if let Some(value) = self.state.get(index) {
                if value.load(Ordering::Acquire) == GRAPH_QUARANTINE_OCCUPIED {
                    value.store(GRAPH_QUARANTINE_FREE, Ordering::Release);
                    released += 1;
                }
            }
            index += 1;
        }
        released
    }
}

impl Default for GraphQuarantine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PlanArena / plan 解析（[NONRT]；Drop 只在 CTL，RT 只拿裸地址）
// ---------------------------------------------------------------------------

/// plan arena 的内存尺寸快照（T18 admission::plan_memory 的结果子集；供 crossfade 预算下界）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanArenaMemory {
    /// 单张图（current）字节。
    pub graph_current_bytes: u64,
    /// 单张图（pending/fallback/crossfade 旧图）字节。
    pub graph_pending_bytes: u64,
    /// RtArena 字节。
    pub arena_bytes: u64,
    /// 总预算字节。
    pub total_bytes: u64,
    /// 已 mlock 字节（admission 报告）。
    pub mlocked_bytes: u64,
    /// RT 线程栈字节（admission 报告）。
    pub stack_bytes: u64,
}

/// [NONRT] 一个已准入 plan arena。
///
/// 唯一所有者是 PlanRegistry 里的 Arc 强引用；[RT] 只通过 ptr_u64 拿到与 TspiPlanHeaderV1
/// 同址的裸地址（core-v1 §13.4 Lemma 1/2），绝不持有 Arc/引用所有权。本类型有隐式字段析构，
/// 只允许在 CTL（NONRT）发生（R-DROP-04/05）。
#[derive(Debug)]
pub struct PlanArena {
    version: PlanVersion,
    graph_id: [u8; 32],
    header: TspiPlanHeaderV1,
    paths: Vec<TspiPathV1>,
    diags: Vec<TspiDiagFirV1>,
    roles: Vec<TspiRoleV1>,
    /// plan ABI v2 per-role IIR 表项（空 = 无 PEQ；v1 plan 恒空）。
    iir: Vec<TspiRoleIirV2>,
    coeffs: Vec<u8>,
    raw_bytes: usize,
    admission: Option<AdmissionReport>,
    memory: PlanArenaMemory,
}

impl PlanArena {
    /// 本 arena 的 plan 版本（registry 分配，header.plan_version 已同步）。
    pub fn version(&self) -> PlanVersion {
        self.version
    }

    /// 规范化 graph_id（32 B；不参与 plan_version，§11.10）。
    pub fn graph_id(&self) -> [u8; 32] {
        self.graph_id
    }

    /// plan 头（含 registry 分配的 plan_version）。
    pub fn header(&self) -> &TspiPlanHeaderV1 {
        &self.header
    }

    /// path 表。
    pub fn paths(&self) -> &[TspiPathV1] {
        &self.paths
    }

    /// diag FIR 表。
    pub fn diags(&self) -> &[TspiDiagFirV1] {
        &self.diags
    }

    /// role 表。
    pub fn roles(&self) -> &[TspiRoleV1] {
        &self.roles
    }

    /// plan ABI v2 per-role IIR 表项（空 = 无 PEQ；GAP-T66-3）。
    pub fn iir(&self) -> &[TspiRoleIirV2] {
        &self.iir
    }

    /// 系数 blob（与 header.total_coeff_bytes 等长）。
    pub fn coeffs(&self) -> &[u8] {
        &self.coeffs
    }

    /// 原始 plan 字节数（诊断）。
    pub fn raw_bytes(&self) -> usize {
        self.raw_bytes
    }

    /// admission 成功报告（未准入前为 None）。
    pub fn admission(&self) -> Option<&AdmissionReport> {
        self.admission.as_ref()
    }

    /// 内存尺寸快照。
    pub fn memory(&self) -> PlanArenaMemory {
        self.memory
    }

    /// 只读视图（core-v1 §12.3 PlanView；借用本 arena）。
    pub fn view(&self) -> PlanView<'_> {
        PlanView {
            header: &self.header,
            paths: &self.paths,
            diags: &self.diags,
            roles: &self.roles,
            coeffs: &self.coeffs,
        }
    }

    /// plan arena 基址（= header 地址；RT 只把它当数值，不解引用 owned 类型）。
    pub fn ptr_u64(&self) -> u64 {
        &self.header as *const TspiPlanHeaderV1 as u64
    }

    /// plan 是否声明 crossfade（flags bit2）。
    pub fn has_crossfade_flag(&self) -> bool {
        (self.header.flags & PLAN_FLAG_CROSSFADE) != 0
    }

    /// expected workers（2/4）。
    pub fn expected_workers(&self) -> u8 {
        self.header.workers
    }
}

/// 读 POD（LE）结构：不做对齐要求，调用者保证字节已初始化且 T 为纯整数字段 POD。
fn read_pod<T: Copy>(bytes: &[u8], offset: usize) -> Result<T, RtStatus> {
    let size = size_of::<T>();
    let end = offset.checked_add(size).ok_or(RtStatus::ERR_BAD_LAYOUT)?;
    if end > bytes.len() {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    // SAFETY: T 为 abi.rs 的 POD（全部整数字段、无指针/无 Drop）；read_unaligned 不要求对齐；
    // 字节已初始化（plan 字节来自 PlanBuf），任意 bit pattern 对整数字段均合法。
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr().add(offset) as *const T) })
}

/// 表区间（offset/count/entry）；count==0 返回 None；越界/未对齐 -> ERR_BAD_LAYOUT。
fn table_region(
    offset: u32,
    count: usize,
    entry_bytes: usize,
    limit: usize,
) -> Result<Option<(usize, usize)>, RtStatus> {
    if count == 0 {
        return Ok(None);
    }
    if offset == 0 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let start = offset as usize;
    if start % 8 != 0 || start < 128 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let len = count
        .checked_mul(entry_bytes)
        .ok_or(RtStatus::ERR_BAD_LAYOUT)?;
    let end = start.checked_add(len).ok_or(RtStatus::ERR_BAD_LAYOUT)?;
    if end > limit {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    Ok(Some((start, len)))
}

fn check_coeff_range(offset: u64, len: u32, taps: u32, total_coeff: usize) -> Result<(), RtStatus> {
    let expect = taps.checked_mul(4).ok_or(RtStatus::ERR_TAPS)?;
    if len != expect {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    let start = offset as usize;
    if start % 64 != 0 {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    let end = start
        .checked_add(len as usize)
        .ok_or(RtStatus::ERR_PATH_TABLE)?;
    if end > total_coeff {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    Ok(())
}

/// [NONRT] plan ABI v2：解析并校验 per-role IIR(PEQ) 表（GAP-T66-3）。
///
/// 前置：header magic/ABI/表区/role 表已校验。返回空 Vec = 无 IIR 表（v2 无 PEQ 等价 v1
/// 语义，但 role.peq_count>0 仍拒收）。硬门：表头 magic/version/entry_bytes/count、表项
/// role 绑定/阶数/段数/系数区间、按 role_id 严格升序、table_crc32c（覆盖表头+entries+
/// 按 entry 顺序的系数 blob 原始字节）、稳定性（二阶实系数极点均在单位圆内：|a2|<1 且
/// |a1|<1+a2）、全部系数有限。
fn parse_plan_iir(
    bytes: &[u8],
    header: &TspiPlanHeaderV1,
    coeff_base: usize,
    total_coeff: usize,
    roles: &[TspiRoleV1],
) -> Result<Vec<TspiRoleIirV2>, RtStatus> {
    let offset = header.reserved3[0] as usize;
    let count = header.reserved3[1] as usize;
    if offset == 0 || count == 0 {
        if roles.iter().any(|role| role.peq_count != 0) {
            return Err(RtStatus::ERR_ROLES);
        }
        return Ok(Vec::new());
    }
    if count > MAX_OUTPUT_ROLES {
        return Err(RtStatus::ERR_CAPACITY);
    }
    let header_bytes = size_of::<TspiPlanIirHeaderV2>();
    if header_bytes != PLAN_IIR_HEADER_BYTES {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let entries_bytes = count
        .checked_mul(PLAN_IIR_ENTRY_BYTES)
        .ok_or(RtStatus::ERR_BAD_LAYOUT)?;
    let table_end = offset
        .checked_add(header_bytes)
        .and_then(|value| value.checked_add(entries_bytes))
        .ok_or(RtStatus::ERR_BAD_LAYOUT)?;
    if offset < 128 || offset % 8 != 0 || table_end > coeff_base {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let iir_header: TspiPlanIirHeaderV2 = read_pod(bytes, offset)?;
    if iir_header.magic != TSPI_PLAN_IIR_MAGIC
        || iir_header.version != TSPI_PLAN_IIR_VERSION
        || iir_header.entry_bytes as usize != PLAN_IIR_ENTRY_BYTES
        || iir_header.entry_count as usize != count
        || iir_header.reserved0 != 0
    {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let mut entries: Vec<TspiRoleIirV2> = Vec::with_capacity(count);
    let mut previous_role: Option<u16> = None;
    let mut index = 0usize;
    while index < count {
        let entry_offset = offset + header_bytes + index * PLAN_IIR_ENTRY_BYTES;
        let entry: TspiRoleIirV2 = read_pod(bytes, entry_offset)?;
        if entry.order != 2
            || entry.flags != 0
            || entry.reserved0 != 0
            || entry.reserved1 != 0
            || entry.reserved2 != 0
        {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if entry.section_count == 0 || entry.section_count as usize > PLAN_IIR_MAX_SECTIONS {
            return Err(RtStatus::ERR_ROLES);
        }
        let role = roles
            .iter()
            .find(|role| role.role_id == entry.role_id)
            .ok_or(RtStatus::ERR_ROLES)?;
        if role.peq_count != entry.section_count {
            return Err(RtStatus::ERR_ROLES);
        }
        let expected_len = entry.section_count as u64 * 4 * PLAN_IIR_SECTION_COEFFS as u64;
        if entry.coeff_len_bytes as u64 != expected_len {
            return Err(RtStatus::ERR_ROLES);
        }
        let coeff_start = entry.coeff_offset_bytes as usize;
        if coeff_start % 64 != 0
            || coeff_start
                .checked_add(entry.coeff_len_bytes as usize)
                .ok_or(RtStatus::ERR_ROLES)?
                > total_coeff
        {
            return Err(RtStatus::ERR_ROLES);
        }
        if let Some(previous) = previous_role {
            if entry.role_id <= previous {
                return Err(RtStatus::ERR_ROLES);
            }
        }
        previous_role = Some(entry.role_id);
        entries.push(entry);
        index += 1;
    }
    // 每个 peq_count>0 的 role 必须有且只有一条 IIR 表项（parse 阶段按 role_id 唯一保证后者）。
    for role in roles {
        if role.peq_count != 0
            && !entries.iter().any(|entry| entry.role_id == role.role_id)
        {
            return Err(RtStatus::ERR_ROLES);
        }
    }
    // IIR 表 CRC：表头(crc 置 0) + entries + 按 entry 顺序的系数 blob 原始字节。
    let mut crc_bytes: Vec<u8> = Vec::with_capacity(table_end - offset + total_coeff);
    crc_bytes.extend_from_slice(&bytes[offset..offset + header_bytes]);
    crc_bytes[header_bytes - 4..header_bytes].fill(0);
    crc_bytes.extend_from_slice(&bytes[offset + header_bytes..table_end]);
    for entry in &entries {
        let start = coeff_base + entry.coeff_offset_bytes as usize;
        let end = start + entry.coeff_len_bytes as usize;
        crc_bytes.extend_from_slice(&bytes[start..end]);
    }
    if khaen_core::nodes::crc32c(&crc_bytes) != iir_header.table_crc32c {
        return Err(RtStatus::ERR_HASH);
    }
    // 系数硬门：有限 + biquad 稳定（极点单位圆内）。
    for entry in &entries {
        let start = coeff_base + entry.coeff_offset_bytes as usize;
        let bytes_region = &bytes[start..start + entry.coeff_len_bytes as usize];
        let mut section = 0usize;
        while section < entry.section_count as usize {
            let base = section * 4 * PLAN_IIR_SECTION_COEFFS;
            let mut coeffs = [0.0f32; PLAN_IIR_SECTION_COEFFS];
            let mut coeff_index = 0usize;
            while coeff_index < PLAN_IIR_SECTION_COEFFS {
                let at = base + coeff_index * 4;
                coeffs[coeff_index] = f32::from_le_bytes([
                    bytes_region[at],
                    bytes_region[at + 1],
                    bytes_region[at + 2],
                    bytes_region[at + 3],
                ]);
                if !coeffs[coeff_index].is_finite() {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                coeff_index += 1;
            }
            let (a1, a2) = (coeffs[3], coeffs[4]);
            if !(a2.abs() < 1.0 && a1.abs() < 1.0 + a2) {
                return Err(RtStatus::ERR_ROLES);
            }
            section += 1;
        }
    }
    Ok(entries)
}

/// 结构化解析 + 校验 plan 字节（[NONRT]；T12/T26 打包约定见文件头）。
fn parse_plan_arena(token: &PlanToken, version: PlanVersion) -> Result<PlanArena, RtStatus> {
    if cfg!(not(target_endian = "little")) {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let bytes = token.plan().as_bytes();
    if bytes.len() < 128 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let mut header: TspiPlanHeaderV1 = read_pod(bytes, 0)?;
    if header.magic != TSPI_PLAN_MAGIC {
        return Err(RtStatus::ERR_MAGIC);
    }
    // plan ABI 版本（GAP-T66-3）：v1 旧 plan 与 v2 per-role IIR plan 都接受；
    // 其它版本一律 ERR_ABI_VERSION（不"尽力解析"、不把 v2 当 v1 运行）。
    if !plan_abi_supported(header.abi_version) {
        return Err(RtStatus::ERR_ABI_VERSION);
    }
    if header.header_size != 128 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if header.reserved0 != 0 || header.reserved1 != 0 || header.reserved2 != 0 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if header.abi_version == TSPI_PLAN_ABI_V1 {
        if header.reserved3 != [0u32; 3] {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
    } else {
        // v2：reserved3[0]=iir_table_offset、reserved3[1]=iir_entry_count、reserved3[2]=0。
        if header.reserved3[2] != 0 {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if (header.reserved3[0] == 0) != (header.reserved3[1] == 0) {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if header.reserved3[1] as usize > MAX_OUTPUT_ROLES {
            return Err(RtStatus::ERR_CAPACITY);
        }
        if header.reserved3[0] != 0
            && (header.reserved3[0] < 128 || header.reserved3[0] % 8 != 0)
        {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
    }
    // plan flags 位表（core-v1 v1.1.3 errata E4/§11.10）：bit0..4 有效、bit5..7 必须 0；
    // 未知/保留位一律拒收（旧 0x3F 掩码会静默接受 bit5，已按 E4 收窄）。
    if (header.flags & !PLAN_FLAG_MASK) != 0 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if !SAMPLE_RATES_HZ.contains(&header.sample_rate) {
        return Err(RtStatus::ERR_RATE);
    }
    if !BLOCK_SIZES.contains(&header.block_size) {
        return Err(RtStatus::ERR_BLOCK_SIZE);
    }
    if header.precision != 0 {
        return Err(RtStatus::ERR_PRECISION);
    }
    if !expected_valid(header.workers) {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    if header.n_in == 0
        || header.n_out == 0
        || header.n_in as usize > MAX_INPUT_CHANNELS
        || header.n_out as usize > MAX_OUTPUT_CHANNELS
    {
        return Err(RtStatus::ERR_CHANNELS);
    }
    if header.path_count as usize > MAX_MATRIX_PATHS {
        return Err(RtStatus::ERR_CAPACITY);
    }
    if header.diag_fir_count as usize > MAX_DIAG_FIRS {
        return Err(RtStatus::ERR_CAPACITY);
    }
    if header.role_table_count as usize > MAX_OUTPUT_ROLES {
        return Err(RtStatus::ERR_ROLES);
    }
    if header.max_taps > TAPS_EXTENDED_MAX {
        return Err(RtStatus::ERR_TAPS);
    }
    if header.max_taps > TAPS_NORMAL_MAX && (header.flags & PLAN_FLAG_EXTENDED_TAPS_ADMITTED) == 0 {
        return Err(RtStatus::ERR_TAPS);
    }
    let block = header.block_size as u32;
    if header.in_stride < block
        || header.in_stride % 16 != 0
        || header.out_stride < block
        || header.out_stride % 16 != 0
    {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    if header.graph_id != token.graph_id() {
        return Err(RtStatus::ERR_HASH);
    }

    // 系数 blob 位于 plan 字节尾部（见文件头契约缺口登记）。
    let total_coeff = header.total_coeff_bytes as usize;
    if total_coeff > bytes.len() {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let coeff_base = bytes.len() - total_coeff;
    if coeff_base < 128 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let path_region = table_region(
        header.path_table_offset,
        header.path_count as usize,
        size_of::<TspiPathV1>(),
        coeff_base,
    )?;
    let diag_region = table_region(
        header.diag_table_offset,
        header.diag_fir_count as usize,
        size_of::<TspiDiagFirV1>(),
        coeff_base,
    )?;
    let role_region = table_region(
        header.role_table_offset,
        header.role_table_count as usize,
        size_of::<TspiRoleV1>(),
        coeff_base,
    )?;
    if header.affinity_count > RT_WORKERS as u16 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let _affinity_region = table_region(
        header.affinity_table_offset,
        header.affinity_count as usize,
        32,
        coeff_base,
    )?;
    if header.ctl_meta_len != 0 {
        let start = header.ctl_meta_offset as usize;
        let end = start
            .checked_add(header.ctl_meta_len as usize)
            .ok_or(RtStatus::ERR_BAD_LAYOUT)?;
        if start < 128 || end > coeff_base {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
    }

    // path 表：按 (out_index,in_index) 严格升序、无重复（§11.10 表顺序）。
    let mut paths: Vec<TspiPathV1> = Vec::with_capacity(header.path_count as usize);
    if let Some((start, _len)) = path_region {
        let count = header.path_count as usize;
        let mut index = 0usize;
        let mut previous: Option<(u16, u16)> = None;
        while index < count {
            let offset = start + index * size_of::<TspiPathV1>();
            let path: TspiPathV1 = read_pod(bytes, offset)?;
            if path.in_index >= header.n_in || path.out_index >= header.n_out {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
            if path.taps == 0 || path.taps > header.max_taps {
                return Err(RtStatus::ERR_TAPS);
            }
            if path.polarity != 1 && path.polarity != -1 {
                return Err(RtStatus::ERR_BAD_LAYOUT);
            }
            if (path.flags & !0x0Fu8) != 0 || path.reserved0 != 0 || path.reserved1 != 0 {
                return Err(RtStatus::ERR_BAD_LAYOUT);
            }
            check_coeff_range(
                path.coeff_offset_bytes,
                path.coeff_len_bytes,
                path.taps,
                total_coeff,
            )?;
            let key = (path.out_index, path.in_index);
            if let Some(prev) = previous {
                if key <= prev {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
            }
            previous = Some(key);
            paths.push(path);
            index += 1;
        }
    }

    // diag FIR 表：按 out_index 严格升序、无重复。
    let mut diags: Vec<TspiDiagFirV1> = Vec::with_capacity(header.diag_fir_count as usize);
    if let Some((start, _len)) = diag_region {
        let count = header.diag_fir_count as usize;
        let mut index = 0usize;
        let mut previous: Option<u16> = None;
        while index < count {
            let offset = start + index * size_of::<TspiDiagFirV1>();
            let diag: TspiDiagFirV1 = read_pod(bytes, offset)?;
            if diag.out_index >= header.n_out {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
            if diag.taps == 0 || diag.taps > header.max_taps {
                return Err(RtStatus::ERR_TAPS);
            }
            if (diag.flags & !0x1Fu16) != 0 || diag.reserved0 != 0 || diag.reserved1 != 0 {
                return Err(RtStatus::ERR_BAD_LAYOUT);
            }
            check_coeff_range(
                diag.coeff_offset_bytes,
                diag.coeff_len_bytes,
                diag.taps,
                total_coeff,
            )?;
            if let Some(prev) = previous {
                if diag.out_index <= prev {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
            }
            previous = Some(diag.out_index);
            diags.push(diag);
            index += 1;
        }
    }

    // role 表：按 role_id 严格升序、无重复。
    let mut roles: Vec<TspiRoleV1> = Vec::with_capacity(header.role_table_count as usize);
    if let Some((start, _len)) = role_region {
        let count = header.role_table_count as usize;
        let mut index = 0usize;
        let mut previous: Option<u16> = None;
        while index < count {
            let offset = start + index * size_of::<TspiRoleV1>();
            let role: TspiRoleV1 = read_pod(bytes, offset)?;
            if role.role_id as usize >= MAX_OUTPUT_ROLES {
                return Err(RtStatus::ERR_ROLES);
            }
            if role.out_index != u16::MAX && role.out_index >= header.n_out {
                return Err(RtStatus::ERR_ROLES);
            }
            if (role.flags & !0x03u16) != 0
                || role.reserved0 != 0
                || role.reserved1 != 0
                || role.reserved2 != 0
                || role.reserved3 != 0
                || role.reserved4 != 0
            {
                return Err(RtStatus::ERR_BAD_LAYOUT);
            }
            if role.polarity != 1 && role.polarity != -1 {
                return Err(RtStatus::ERR_ROLES);
            }
            if role.peq_count > 32 {
                return Err(RtStatus::ERR_ROLES);
            }
            // 角色级延迟上限 200 ms（§11.10 role.delay_samples）。
            if role.delay_samples as u64 > (header.sample_rate as u64) / 5 {
                return Err(RtStatus::ERR_ROLES);
            }
            if let Some(prev) = previous {
                if role.role_id <= prev {
                    return Err(RtStatus::ERR_ROLES);
                }
            }
            previous = Some(role.role_id);
            roles.push(role);
            index += 1;
        }
    }

    // GAP-T66-3：v1 plan 没有 IIR 系数区，role.peq_count>0 必须拒收（禁止静默丢弃 PEQ）；
    // v2 plan 的 IIR 表在 role 表之后解析（role 绑定/系数区间/CRC）。
    if header.abi_version == TSPI_PLAN_ABI_V1 && roles.iter().any(|role| role.peq_count != 0) {
        return Err(RtStatus::ERR_ROLES);
    }
    let iir = parse_plan_iir(bytes, &header, coeff_base, total_coeff, &roles)?;

    let coeffs = bytes[coeff_base..].to_vec();
    header.plan_version = version;
    Ok(PlanArena {
        version,
        graph_id: header.graph_id,
        header,
        paths,
        diags,
        roles,
        iir,
        coeffs,
        raw_bytes: bytes.len(),
        admission: None,
        memory: PlanArenaMemory::default(),
    })
}

// ---------------------------------------------------------------------------
// [RT] block 边界 plan 提交（INV-L1：只有 master 写 current_plan_ptr/version）
// ---------------------------------------------------------------------------

/// [RT] master 在 block 边界提交已 ARMED 的 pending plan（§4.4/§7.5 RG-1）。
///
/// 前置：switch_state=ARMED 且 pending 已由 NONRT 发布（Release）；本函数只把
/// current_plan_ptr/version 原子替换为 pending 并置 switch_state=SWITCHED（Release）。
/// 成功返回新 plan_version；IDLE/SWITCHED/ABORT、pending 标签非法或版本倒退 ->
/// ERR_SWITCH_BUSY / RT_E_PLAN_MISMATCH，绝不部分切换。
pub fn commit_swap_at_block_boundary(ctl: &TspiRtControlV1) -> Result<PlanVersion, RtStatus> {
    if ctl.magic != TSPI_RING_MAGIC || ctl.abi_version != ABI_VERSION || ctl.size != 128 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if ctl.safety_request.load(Ordering::Acquire) != 0 {
        return Err(RtStatus::RT_E_WATCHDOG);
    }
    if ctl.switch_state.load(Ordering::Acquire) != SWITCH_ARMED {
        return Err(RtStatus::ERR_SWITCH_BUSY);
    }
    let pending_version = ctl.pending_plan_version.load(Ordering::Acquire);
    let pending_ptr = ctl.pending_plan_ptr.load(Ordering::Acquire);
    let current_version = ctl.current_plan_version.load(Ordering::Acquire);
    let current_ptr = ctl.current_plan_ptr.load(Ordering::Acquire);
    if pending_version == 0 || pending_ptr == 0 {
        return Err(RtStatus::RT_E_PLAN_MISMATCH);
    }
    // INV-L7：plan_version 单调递增（回滚必须重新准入成更高版本，禁止版本倒退）。
    if pending_version <= current_version {
        return Err(RtStatus::RT_E_PLAN_MISMATCH);
    }
    if pending_ptr == current_ptr {
        return Err(RtStatus::RT_E_PLAN_MISMATCH);
    }
    // 发布顺序：先 version 后 ptr（Release）；读者 Acquire ptr 后再读 version。
    ctl.current_plan_version
        .store(pending_version, Ordering::Relaxed);
    ctl.current_plan_ptr.store(pending_ptr, Ordering::Release);
    ctl.pending_plan_version.store(0, Ordering::Relaxed);
    ctl.pending_plan_ptr.store(0, Ordering::Relaxed);
    ctl.switch_state.store(SWITCH_SWITCHED, Ordering::Release);
    Ok(pending_version)
}
// ---------------------------------------------------------------------------
// [RT] worker 切版本 + 旧图 quarantine 释放（block-version 与 quarantine 的联动入口）
// ---------------------------------------------------------------------------

/// [RT] worker 报告"已切到新 plan_version 且不再使用旧图"。
///
/// 同时推进两处原子状态：swap ack（RG-2 位）与旧图 quarantine 的剩余位（R-DROP-06）。
/// 任一标签不符即返回对应错误，且不部分推进（先 swap ack 后 quarantine 位）。
pub fn note_worker_switched(
    swap_ack: &TspiSwapAckV1,
    quarantine: &GraphQuarantine,
    quarantine_slot: usize,
    worker: u8,
    old_plan_version: PlanVersion,
    old_epoch: u64,
    new_plan_version: PlanVersion,
) -> RtStatus {
    let status = swap_ack.ack_worker_version(worker, new_plan_version);
    if !status.is_ok() {
        return status;
    }
    quarantine.note_ack(quarantine_slot, worker, old_plan_version, old_epoch)
}

/// [RT] master 用一个"新版本 all-ack block"推进 swap ack，并释放 4 worker 的旧图 quarantine 位。
///
/// 语义：block 在新 plan_version 上三 phase 全部 all-ack 且 cutoff=0 => 每个 worker 都已离开
/// 旧图；因此逐 worker 清 quarantine 剩余位（工人失联则位永不清，槽永不释放）。
pub fn note_block_switched(
    swap_ack: &TspiSwapAckV1,
    quarantine: &GraphQuarantine,
    quarantine_slot: usize,
    block: &TspiBlockAckV1,
    old_plan_version: PlanVersion,
    old_epoch: u64,
    new_plan_version: PlanVersion,
) -> RtStatus {
    if block.plan_version != new_plan_version {
        return RtStatus::RT_E_PLAN_MISMATCH;
    }
    let status = swap_ack.note_block_all_ack(block);
    if !status.is_ok() {
        return status;
    }
    let mut worker = 0u8;
    while worker < swap_ack.expected_workers {
        let released = quarantine.note_ack(quarantine_slot, worker, old_plan_version, old_epoch);
        if !released.is_ok() {
            return released;
        }
        worker += 1;
    }
    RtStatus::OK
}

// ---------------------------------------------------------------------------
// PlanRegistry（[NONRT] 唯一所有者；回收只在这里）
// ---------------------------------------------------------------------------

/// plan 在 registry 内的生命周期状态（§5.3 PlanLifecycle 子集；NONRT 诊断）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanState {
    /// 已准入（PlanAdmitted），尚未 staged。
    Admitted,
    /// 已写入 pending（PlanStaged；ctl.switch_state=ARMED 或待 arm）。
    Staged,
    /// 已生效（PlanCommitted）。
    Committed,
    /// 已被新版本取代，等待 retire gate（PlanSuperseded）。
    Superseded,
}

/// 切换/回收统计（[NONRT] 诊断；Copy）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapStats {
    /// 成功准入次数。
    pub admitted: u32,
    /// stage 次数。
    pub staged: u32,
    /// arm（写 pending + ARMED）次数。
    pub armed: u32,
    /// block 边界提交次数。
    pub committed: u32,
    /// abort 次数。
    pub aborted: u32,
    /// retire gate 全过后回收次数。
    pub retired: u32,
    /// 已释放（unmap/drop）plan 图数量（旧图回收 mock 计数器）。
    pub unmapped: u32,
    /// crossfade 准入通过次数。
    pub crossfade_accepted: u32,
    /// crossfade 拒绝次数。
    pub crossfade_rejected: u32,
    /// 旧图进入 quarantine 次数。
    pub quarantine_entered: u32,
    /// quarantine 槽释放次数。
    pub quarantine_released: u32,
    /// retire gate 被条件阻塞的评估次数。
    pub retire_blocked: u32,
}

/// retire gate 逐条件报告（§7.5 RG-1..RG-3；诊断/测试）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetireGateReport {
    /// 待回收旧版本。
    pub version: PlanVersion,
    /// RG-1：switch_state 已回 IDLE。
    pub switch_idle: bool,
    /// RG-2：4 worker 已 all-ack 新版本（swap ack）。
    pub swap_all_ack: bool,
    /// RG-3a：旧版本 block 已全部终态（T09/T16 快照确认）。
    pub old_blocks_terminal: bool,
    /// RG-3b：旧图 quarantine 槽已 all-ack。
    pub quarantine_all_ack: bool,
    /// 全部满足（RG-4 允许 control drop/unmap）。
    pub ready: bool,
}

#[derive(Debug)]
struct PendingRetire {
    version: PlanVersion,
    quarantine_slot: Option<usize>,
    old_blocks_terminal: bool,
}

#[derive(Debug)]
struct PlanEntry {
    arena: Arc<PlanArena>,
    state: PlanState,
}

/// [NONRT] plan 注册/切换/退役（core-v1 §5.3/§7.5；唯一 Drop/unmap 发生地）。
///
/// 使用流程（control 线程）：
/// 1. admit/admit_with（或 admit_crossfade）-> 分配版本；
/// 2. stage -> arm（写 pending + switch_state=ARMED）；
/// 3. [RT] master 在 block 边界 commit_swap_at_block_boundary；
/// 4. control complete_swap（回 IDLE、旧图入 quarantine）；
/// 5. [RT] master 用新版本 all-ack block 推进 swap ack（note_block_all_ack）；
/// 6. control confirm_old_blocks_terminal + poll_retire/collect_retired（RG-1..RG-4）。
#[derive(Debug)]
pub struct PlanRegistry {
    committed: u32,
    next_version: PlanVersion,
    plans: Vec<PlanEntry>,
    current: Option<PlanVersion>,
    staged: Option<PlanVersion>,
    armed: bool,
    env: Option<AdmissionEnv>,
    superseded: Vec<PendingRetire>,
    mirror_switch_idle: bool,
    swap_acked_version: Option<PlanVersion>,
    quarantine: GraphQuarantine,
    stats: SwapStats,
}

impl PlanRegistry {
    /// 空注册表（next_version 从 1 开始；plan_version=0 保留给"无 plan"）。
    pub fn new() -> Self {
        Self {
            committed: 0,
            next_version: 1,
            plans: Vec::new(),
            current: None,
            staged: None,
            armed: false,
            env: None,
            superseded: Vec::new(),
            mirror_switch_idle: false,
            swap_acked_version: None,
            quarantine: GraphQuarantine::new(),
            stats: SwapStats::default(),
        }
    }

    /// 已成功 commit 的 plan 次数（诊断用；T06 冻结 API）。
    pub fn committed(&self) -> u32 {
        self.committed
    }

    /// 切换/回收统计。
    pub fn stats(&self) -> SwapStats {
        self.stats
    }

    /// 旧图 quarantine 池（T26 需要与 RT 共享同一实例）。
    pub fn quarantine(&self) -> &GraphQuarantine {
        &self.quarantine
    }

    /// 当前生效 plan 版本。
    pub fn current_version(&self) -> Option<PlanVersion> {
        self.current
    }

    /// 已 staged 的 plan 版本。
    pub fn staged_version(&self) -> Option<PlanVersion> {
        self.staged
    }

    /// 是否已发布 ARMED。
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// 是否有在途切换（staged 或 armed）。
    pub fn is_switch_busy(&self) -> bool {
        self.armed || self.staged.is_some()
    }

    /// 最新一次 swap all-ack 的版本（RG-2 证据；无则 None）。
    pub fn swap_acked_version(&self) -> Option<PlanVersion> {
        self.swap_acked_version
    }

    /// 所有已准入且未回收的版本（升序；NONRT 可分配）。
    pub fn admitted_versions(&self) -> Vec<PlanVersion> {
        let mut versions = Vec::with_capacity(self.plans.len());
        let mut index = 0usize;
        while index < self.plans.len() {
            if let Some(entry) = self.plans.get(index) {
                versions.push(entry.arena.version);
            }
            index += 1;
        }
        versions
    }

    /// 指定版本 arena 的裸地址（RT 只能拿数值）。
    pub fn arena_ptr(&self, version: PlanVersion) -> Option<u64> {
        self.arc(version).map(|arena| arena.ptr_u64())
    }

    /// 指定版本 arena 的 Arc 强引用计数（应为 1；RT 不得改变；测试断言用）。
    pub fn arena_strong_count(&self, version: PlanVersion) -> Option<usize> {
        self.arc(version).map(Arc::strong_count)
    }

    /// 指定版本 plan 的只读视图。
    pub fn plan_view(&self, version: PlanVersion) -> Option<PlanView<'_>> {
        self.arc(version).map(|arena| arena.view())
    }

    /// 指定版本 plan 的 ABI v2 per-role IIR 表项（v1/无 PEQ 为空切片；GAP-T66-3）。
    pub fn iir_entries(&self, version: PlanVersion) -> Option<&[TspiRoleIirV2]> {
        self.arc(version).map(|arena| arena.iir())
    }

    /// 指定版本生命周期状态。
    pub fn plan_state(&self, version: PlanVersion) -> Option<PlanState> {
        self.plan_index(version)
            .and_then(|index| self.plans.get(index).map(|entry| entry.state))
    }

    // -----------------------------------------------------------------------
    // admit
    // -----------------------------------------------------------------------

    /// 准入并登记 plan（非 RT；生产入口用真实平台内存后端，非 Linux64 平台 fail closed）。
    pub fn admit(&mut self, token: PlanToken, env: &AdmissionEnv) -> Result<PlanVersion, RtStatus> {
        let mut backend = PlatformMemory::probe()?;
        self.admit_with(token, env, &mut backend)
    }

    /// 准入并登记 plan（可注入内存后端；T18 admission 的内存/锁页/预触判定）。
    ///
    /// 失败不消耗版本号、不登记半成品（不产生可被 RT 看到的 plan）。
    pub fn admit_with<B: MemoryBackend>(
        &mut self,
        token: PlanToken,
        env: &AdmissionEnv,
        backend: &mut B,
    ) -> Result<PlanVersion, RtStatus> {
        if self.is_switch_busy() {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        let version = self.reserve_version()?;
        let mut arena = parse_plan_arena(&token, version)?;
        // crossfade 必须走 admit_crossfade（§11.10 flags bit2 规则）。
        if arena.has_crossfade_flag() {
            return Err(ERR_CROSSFADE_BUDGET);
        }
        self.ensure_new_graph(&arena.graph_id)?;
        let memory_plan = admission::plan_memory(&arena.view(), env)?;
        let report = admission::admit_with(&arena.view(), env, backend)?;
        arena.admission = Some(report);
        arena.memory = PlanArenaMemory {
            graph_current_bytes: memory_plan.graph_current_bytes,
            graph_pending_bytes: memory_plan.graph_pending_bytes,
            arena_bytes: memory_plan.arena_bytes,
            total_bytes: memory_plan.total_bytes,
            mlocked_bytes: report.mlocked_bytes,
            stack_bytes: report.stack_bytes,
        };
        self.plans.push(PlanEntry {
            arena: Arc::new(arena),
            state: PlanState::Admitted,
        });
        self.next_version = version.wrapping_add(1);
        self.env = Some(*env);
        self.stats.admitted += 1;
        Ok(version)
    }

    // -----------------------------------------------------------------------
    // crossfade admission（§5.3 I06）
    // -----------------------------------------------------------------------

    /// crossfade 准入（冻结签名；只校验不改 budget；生产平台后端）。
    pub fn admit_crossfade(
        &mut self,
        old: PlanVersion,
        new: PlanToken,
        budget: &TspiCrossfadeBudgetV1,
    ) -> Result<PlanVersion, RtStatus> {
        let status = budget.validate();
        if !status.is_ok() {
            self.stats.crossfade_rejected += 1;
            return Err(status);
        }
        let mut backend = match PlatformMemory::probe() {
            Ok(value) => value,
            Err(err) => {
                self.stats.crossfade_rejected += 1;
                return Err(err);
            }
        };
        let snapshot = budget.snapshot();
        match self.crossfade_admit_inner(old, new, &snapshot, &mut backend) {
            Ok((version, _check)) => Ok(version),
            Err(err) => {
                self.stats.crossfade_rejected += 1;
                Err(err)
            }
        }
    }

    /// crossfade 准入（可注入后端；成功时把 admission 结论写回 budget flags/版本）。
    pub fn admit_crossfade_with<B: MemoryBackend>(
        &mut self,
        old: PlanVersion,
        new: PlanToken,
        budget: &mut TspiCrossfadeBudgetV1,
        backend: &mut B,
    ) -> Result<PlanVersion, RtStatus> {
        let status = budget.validate();
        if !status.is_ok() {
            self.stats.crossfade_rejected += 1;
            return Err(status);
        }
        let snapshot = budget.snapshot();
        match self.crossfade_admit_inner(old, new, &snapshot, backend) {
            Ok((version, _check)) => {
                budget.mark_admitted(old, version);
                Ok(version)
            }
            Err(err) => {
                self.stats.crossfade_rejected += 1;
                Err(err)
            }
        }
    }

    fn crossfade_admit_inner<B: MemoryBackend>(
        &mut self,
        old: PlanVersion,
        new: PlanToken,
        budget: &CrossfadeSnapshot,
        backend: &mut B,
    ) -> Result<(PlanVersion, CrossfadeCheck), RtStatus> {
        if self.is_switch_busy() {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if self.current != Some(old) {
            return Err(RtStatus::ERR_PLAN_STALE);
        }
        let env = match self.env {
            Some(value) => value,
            None => return Err(RtStatus::ERR_PLAN_STALE),
        };
        let version = self.reserve_version()?;
        let mut arena = parse_plan_arena(&new, version)?;
        if !arena.has_crossfade_flag() {
            return Err(ERR_CROSSFADE_BUDGET);
        }
        self.ensure_new_graph(&arena.graph_id)?;
        let check = {
            let old_arena = match self.arc(old) {
                Some(value) => value,
                None => return Err(RtStatus::ERR_PLAN_STALE),
            };
            validate_crossfade_snapshot(old_arena, &arena, budget)?
        };
        let memory_plan = admission::plan_memory(&arena.view(), &env)?;
        let report = admission::admit_with(&arena.view(), &env, backend)?;
        arena.admission = Some(report);
        arena.memory = PlanArenaMemory {
            graph_current_bytes: memory_plan.graph_current_bytes,
            graph_pending_bytes: memory_plan.graph_pending_bytes,
            arena_bytes: memory_plan.arena_bytes,
            total_bytes: memory_plan.total_bytes,
            mlocked_bytes: report.mlocked_bytes,
            stack_bytes: report.stack_bytes,
        };
        self.plans.push(PlanEntry {
            arena: Arc::new(arena),
            state: PlanState::Admitted,
        });
        self.next_version = version.wrapping_add(1);
        self.env = Some(env);
        self.stats.admitted += 1;
        self.stats.crossfade_accepted += 1;
        Ok((version, check))
    }

    // -----------------------------------------------------------------------
    // stage / arm / commit / abort
    // -----------------------------------------------------------------------

    /// 把已准入 plan 标记为 staged（§5.3 PlanStaged 的本地半程）。
    ///
    /// 冻结签名无 ctl 参数：RT 可见的 pending/ARMED 发布由 arm() 完成；本方法只做本地
    /// 状态迁移与互斥（在途切换 -> ERR_SWITCH_BUSY）。
    pub fn stage(&mut self, version: PlanVersion) -> Result<StageToken, RtStatus> {
        if self.is_switch_busy() {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if self.current == Some(version) {
            return Err(RtStatus::ERR_PLAN_STALE);
        }
        let index = match self.plan_index(version) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_PLAN_STALE),
        };
        if let Some(entry) = self.plans.get_mut(index) {
            if entry.state != PlanState::Admitted {
                return Err(RtStatus::ERR_PLAN_STALE);
            }
            entry.state = PlanState::Staged;
        } else {
            return Err(RtStatus::ERR_PLAN_STALE);
        }
        self.staged = Some(version);
        self.stats.staged += 1;
        Ok(StageToken { version })
    }

    /// [NONRT] 发布 pending + switch_state=ARMED（§4.4 步骤 / §7.5 RG-1 前态）。
    ///
    /// 前置：stage 已成功、ctl.switch_state=IDLE、旧图 quarantine 有空槽（否则
    /// ERR_QUARANTINE_EXHAUSTED，拒绝新切图；§7.6）。成功后 RT master 才能在 block 边界提交。
    pub fn arm(
        &mut self,
        token: StageToken,
        ctl: &TspiRtControlV1,
        swap_ack: &mut TspiSwapAckV1,
        epoch_id: u64,
    ) -> RtStatus {
        if self.staged != Some(token.version) {
            return RtStatus::ERR_PLAN_STALE;
        }
        if self.armed {
            return RtStatus::ERR_SWITCH_BUSY;
        }
        if ctl.magic != TSPI_RING_MAGIC || ctl.abi_version != ABI_VERSION || ctl.size != 128 {
            return RtStatus::ERR_BAD_LAYOUT;
        }
        if ctl.switch_state.load(Ordering::Acquire) != SWITCH_IDLE {
            return RtStatus::ERR_SWITCH_BUSY;
        }
        let current_version = self.current.unwrap_or(0);
        if ctl.current_plan_version.load(Ordering::Acquire) != current_version {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if self.current.is_some() && self.quarantine.free_slot_count() == 0 {
            return RtStatus::ERR_QUARANTINE_EXHAUSTED;
        }
        let (version, expected, new_ptr) = match self.plan_index(token.version) {
            Some(index) => match self.plans.get(index) {
                Some(entry) => (
                    entry.arena.version,
                    entry.arena.expected_workers(),
                    entry.arena.ptr_u64(),
                ),
                None => return RtStatus::ERR_PLAN_STALE,
            },
            None => return RtStatus::ERR_PLAN_STALE,
        };
        let old_ptr = match self.current {
            Some(value) => match self.arc(value) {
                Some(arena) => arena.ptr_u64(),
                None => 0,
            },
            None => 0,
        };
        let status = swap_ack.init(version, expected, epoch_id, version, old_ptr);
        if !status.is_ok() {
            return status;
        }
        ctl.pending_plan_version.store(version, Ordering::Relaxed);
        ctl.pending_plan_ptr.store(new_ptr, Ordering::Relaxed);
        ctl.switch_state.store(SWITCH_ARMED, Ordering::Release);
        self.armed = true;
        self.stats.armed += 1;
        RtStatus::OK
    }

    /// [NONRT] 提交后收尾（RG-1：switch_state 回 IDLE；旧图入 quarantine）。
    ///
    /// 前置：RT master 已用 commit_swap_at_block_boundary 把 ctl 置 SWITCHED 且
    /// current_plan_version == staged。成功后旧版本进入 Superseded + GraphQuarantine，
    /// 之后只有 retire gate 全过才能释放。
    pub fn complete_swap(
        &mut self,
        ctl: &TspiRtControlV1,
        swap_ack: &TspiSwapAckV1,
    ) -> Result<PlanVersion, RtStatus> {
        let version = match self.staged {
            Some(value) => value,
            None => return Err(RtStatus::ERR_SWITCH_BUSY),
        };
        if !self.armed {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if ctl.magic != TSPI_RING_MAGIC || ctl.abi_version != ABI_VERSION || ctl.size != 128 {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if ctl.switch_state.load(Ordering::Acquire) != SWITCH_SWITCHED {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if ctl.current_plan_version.load(Ordering::Acquire) != version {
            return Err(RtStatus::RT_E_PLAN_MISMATCH);
        }
        if ctl.current_plan_ptr.load(Ordering::Acquire) == 0 {
            return Err(RtStatus::RT_E_PLAN_MISMATCH);
        }
        if swap_ack.plan_version != version {
            return Err(RtStatus::RT_E_PLAN_MISMATCH);
        }
        let expected = match self.plan_index(version) {
            Some(index) => match self.plans.get(index) {
                Some(entry) => entry.arena.expected_workers(),
                None => return Err(RtStatus::ERR_PLAN_STALE),
            },
            None => return Err(RtStatus::ERR_PLAN_STALE),
        };
        if swap_ack.expected_workers != expected {
            return Err(RtStatus::RT_E_PLAN_MISMATCH);
        }

        // RG-1：切回 IDLE（唯一写者是 control；master 已发布 SWITCHED）。
        ctl.switch_state.store(SWITCH_IDLE, Ordering::Release);

        let old = self.current.replace(version);
        if let Some(old_version) = old {
            if old_version != version {
                let old_ptr = match self.arc(old_version) {
                    Some(arena) => arena.ptr_u64(),
                    None => 0,
                };
                let old_expected = match self.arc(old_version) {
                    Some(arena) => arena.expected_workers(),
                    None => expected,
                };
                let full_mask = ack_mask(old_expected);
                let acked = swap_ack.ack_mask_value() & full_mask;
                match self.quarantine.enter(
                    swap_ack.retire_epoch,
                    old_version,
                    old_ptr,
                    old_expected,
                ) {
                    Ok(slot) => {
                        // 提交前已用新版本 all-ack 的 worker 直接视为"已不用旧图"。
                        let mut worker = 0u8;
                        while worker < old_expected {
                            if (acked & (1u32 << worker)) != 0
                                && self
                                    .quarantine
                                    .note_ack(slot, worker, old_version, swap_ack.retire_epoch)
                                    .is_ok()
                            {
                                // 已清位；失败保留位图（fail closed，无法提前释放）。
                            }
                            worker += 1;
                        }
                        self.stats.quarantine_entered += 1;
                        if let Some(index) = self.plan_index(old_version) {
                            if let Some(entry) = self.plans.get_mut(index) {
                                entry.state = PlanState::Superseded;
                            }
                        }
                        self.superseded.push(PendingRetire {
                            version: old_version,
                            quarantine_slot: Some(slot),
                            old_blocks_terminal: false,
                        });
                    }
                    Err(err) => {
                        // arm 已做过容量检查；此处失败属不变量破坏：不标记 Superseded、
                        // 不释放旧图（安全泄漏），把错误交给 control 走 SafetyMute。
                        self.stats.retire_blocked += 1;
                        return Err(err);
                    }
                }
            }
        }

        if let Some(index) = self.plan_index(version) {
            if let Some(entry) = self.plans.get_mut(index) {
                entry.state = PlanState::Committed;
            }
        }
        self.staged = None;
        self.armed = false;
        self.mirror_switch_idle = true;
        self.committed = self.committed.wrapping_add(1);
        self.stats.committed += 1;
        Ok(version)
    }

    /// 中止在途切换（未 arm 时；arm 之后必须 abort_and_disarm）。
    pub fn abort(&mut self, token: StageToken) -> RtStatus {
        if self.armed {
            return RtStatus::ERR_SWITCH_BUSY;
        }
        if self.staged != Some(token.version) {
            return RtStatus::ERR_PLAN_STALE;
        }
        self.abort_local(token.version);
        RtStatus::OK
    }

    /// [NONRT] 中止已 arm 的切换并把 switch_state 回 IDLE（旧 plan 保持生效）。
    pub fn abort_and_disarm(&mut self, token: StageToken, ctl: &TspiRtControlV1) -> RtStatus {
        if self.staged != Some(token.version) {
            return RtStatus::ERR_PLAN_STALE;
        }
        if ctl.magic != TSPI_RING_MAGIC || ctl.abi_version != ABI_VERSION || ctl.size != 128 {
            return RtStatus::ERR_BAD_LAYOUT;
        }
        let state = ctl.switch_state.load(Ordering::Acquire);
        if state == SWITCH_SWITCHED {
            // 已提交：只能 complete_swap 或走非 RT 重新准入的 rollback，不能 abort。
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if state == SWITCH_ARMED {
            if ctl.pending_plan_version.load(Ordering::Acquire) != token.version {
                return RtStatus::RT_E_PLAN_MISMATCH;
            }
            ctl.pending_plan_version.store(0, Ordering::Relaxed);
            ctl.pending_plan_ptr.store(0, Ordering::Relaxed);
            ctl.switch_state.store(SWITCH_IDLE, Ordering::Release);
        }
        self.armed = false;
        self.abort_local(token.version);
        RtStatus::OK
    }

    fn abort_local(&mut self, version: PlanVersion) {
        self.staged = None;
        if let Some(index) = self.plan_index(version) {
            if let Some(entry) = self.plans.get_mut(index) {
                entry.state = PlanState::Admitted;
            }
        }
        self.stats.aborted += 1;
    }

    // -----------------------------------------------------------------------
    // retire gate（§7.5 RG-1..RG-4）
    // -----------------------------------------------------------------------

    /// [NONRT] 旧版本 block 全部终态确认（RG-3a；由 T09/T16 槽快照/playback ring owner 提供）。
    ///
    /// fail closed：未显式确认的旧版本永不回收。
    pub fn confirm_old_blocks_terminal(&mut self, version: PlanVersion) -> RtStatus {
        let mut index = 0usize;
        while index < self.superseded.len() {
            let matched = match self.superseded.get(index) {
                Some(pending) => pending.version == version,
                None => false,
            };
            if matched {
                if let Some(pending) = self.superseded.get_mut(index) {
                    pending.old_blocks_terminal = true;
                }
                return RtStatus::OK;
            }
            index += 1;
        }
        RtStatus::ERR_PLAN_STALE
    }

    /// 指定版本 retire gate 逐条件报告（诊断/测试；不在 superseded 返回 None）。
    pub fn retire_gate(&self, version: PlanVersion) -> Option<RetireGateReport> {
        let mut index = 0usize;
        while index < self.superseded.len() {
            if let Some(pending) = self.superseded.get(index) {
                if pending.version == version {
                    return Some(self.gate_for(pending));
                }
            }
            index += 1;
        }
        None
    }

    /// [NONRT] 用 RT 可见状态刷新 retire 镜像并评估回收（§12.3 poll_retire）。
    ///
    /// 只有返回 Some 才表示 control 可/已 drop+unmap 旧 arena（本函数已执行 Arc drop）。
    pub fn poll_retire(
        &mut self,
        ctl: &TspiRtControlV1,
        swap_ack: &TspiSwapAckV1,
    ) -> Option<PlanVersion> {
        if ctl.magic == TSPI_RING_MAGIC && ctl.abi_version == ABI_VERSION && ctl.size == 128 {
            self.mirror_switch_idle = ctl.switch_state.load(Ordering::Acquire) == SWITCH_IDLE;
        } else {
            self.mirror_switch_idle = false;
        }
        let expected = swap_ack.expected_workers;
        if expected_valid(expected) {
            let mask = ack_mask(expected);
            let acked = swap_ack.ack_mask_value();
            if swap_ack.retire_ready_value() == 1
                && (acked & mask) == mask
                && swap_ack.plan_version != 0
                && self
                    .swap_acked_version
                    .map_or(true, |value| swap_ack.plan_version > value)
            {
                self.swap_acked_version = Some(swap_ack.plan_version);
            }
        }
        self.evaluate_retire()
    }

    /// 轮询 retire gate；只有 Some 才允许 drop/unmap 旧 arena（冻结签名）。
    ///
    /// 以 complete_swap/poll_retire 维护的镜像为准；在 collect_retired 之前至少调用一次
    /// poll_retire（或 complete_swap）以建立 RG-1/RG-2 镜像。
    pub fn collect_retired(&mut self) -> Option<PlanVersion> {
        self.evaluate_retire()
    }

    /// [NONRT] 停机（HostState=Stopped）：强制释放全部 plan/quarantine。
    ///
    /// 只允许在 RT 线程已停、设备已关之后调用（§5.1 Stopped）。
    pub fn shutdown(&mut self) -> u32 {
        self.stats.quarantine_released += self.quarantine.release_all_force();
        self.superseded.clear();
        let count = self.plans.len() as u32;
        self.plans.clear();
        self.stats.unmapped += count;
        self.stats.retired += count;
        self.staged = None;
        self.armed = false;
        self.current = None;
        self.mirror_switch_idle = false;
        self.swap_acked_version = None;
        count
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    fn evaluate_retire(&mut self) -> Option<PlanVersion> {
        let mut index = 0usize;
        while index < self.superseded.len() {
            let ready = match self.superseded.get(index) {
                Some(pending) => self.gate_for(pending).ready,
                None => break,
            };
            if !ready {
                self.stats.retire_blocked += 1;
                index += 1;
                continue;
            }
            let pending = self.superseded.remove(index);
            if let Some(slot) = pending.quarantine_slot {
                if self.quarantine.try_release(slot).is_ok() {
                    self.stats.quarantine_released += 1;
                }
            }
            let position = self
                .plans
                .iter()
                .position(|entry| entry.arena.version == pending.version);
            if let Some(position) = position {
                let entry = self.plans.remove(position);
                // NONRT unmap/drop：唯一强引用在此归零（RT 从未持有 Arc；R-DROP-05）。
                drop(entry);
                self.stats.unmapped += 1;
                self.stats.retired += 1;
                return Some(pending.version);
            }
            // 不变量破坏：superseded 里的版本不在 plans（不应发生）；继续评估其它。
        }
        None
    }

    fn gate_for(&self, pending: &PendingRetire) -> RetireGateReport {
        let swap_all_ack = match self.swap_acked_version {
            Some(value) => value > pending.version,
            None => false,
        };
        let quarantine_all_ack = match pending.quarantine_slot {
            // fail closed：没有 quarantine 记录 -> 不认为可释放。
            Some(slot) => self.quarantine.remaining_mask(slot) == Some(0),
            None => false,
        };
        RetireGateReport {
            version: pending.version,
            switch_idle: self.mirror_switch_idle,
            swap_all_ack,
            old_blocks_terminal: pending.old_blocks_terminal,
            quarantine_all_ack,
            ready: self.mirror_switch_idle
                && swap_all_ack
                && pending.old_blocks_terminal
                && quarantine_all_ack,
        }
    }

    fn reserve_version(&self) -> Result<PlanVersion, RtStatus> {
        if self.next_version == 0 || self.next_version == u32::MAX {
            // PLAN_VERSION_WRAP_BOUND：2^32 次 commit 才可能触发；回绕即 fault（INV-L7）。
            return Err(RtStatus::RT_E_INVARIANT);
        }
        Ok(self.next_version)
    }

    fn plan_index(&self, version: PlanVersion) -> Option<usize> {
        self.plans
            .iter()
            .position(|entry| entry.arena.version == version)
    }

    fn arc(&self, version: PlanVersion) -> Option<&Arc<PlanArena>> {
        self.plan_index(version)
            .and_then(|index| self.plans.get(index).map(|entry| &entry.arena))
    }

    fn ensure_new_graph(&self, graph_id: &[u8; 32]) -> Result<(), RtStatus> {
        let mut index = 0usize;
        while index < self.plans.len() {
            if let Some(entry) = self.plans.get(index) {
                if entry.arena.graph_id == *graph_id {
                    // §11.10 注 5：内容未变的 commit 是 stale（graph_id 相同 = 无变更）。
                    return Err(RtStatus::ERR_PLAN_STALE);
                }
            }
            index += 1;
        }
        Ok(())
    }
}

impl Default for PlanRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 编译期断言（§13.2/§11.13：RT 可见类型 needs_drop==false + 固定布局）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(SWAP_ACK_BYTES == 64);
    assert!(CROSSFADE_BUDGET_BYTES == 64);
    assert!(size_of::<TspiSwapAckV1>() == SWAP_ACK_BYTES);
    assert!(align_of::<TspiSwapAckV1>() == 64);
    assert!(!needs_drop::<TspiSwapAckV1>());
    assert!(size_of::<TspiCrossfadeBudgetV1>() == CROSSFADE_BUDGET_BYTES);
    assert!(align_of::<TspiCrossfadeBudgetV1>() == 64);
    assert!(!needs_drop::<TspiCrossfadeBudgetV1>());
    assert!(!needs_drop::<GraphQuarantine>());
    assert!(!needs_drop::<PlanVersion>());
    assert!(!needs_drop::<RtStatus>());
    assert!(!needs_drop::<PlanView<'static>>());
    assert!(GRAPH_QUARANTINE_SLOTS == 8);
};

#[cfg(test)]
mod swap_tests {
    include!("../tests/T20/swap_models.rs");
}
