//! [RT] 四 worker 静态 phase DAG / 固定 reduce / 有界 B1-B3 / epoch —— WBS T09 写域。
//!
//! RT-MODULE: DSP 纯 worker/master 路径禁止析构/分配/锁/syscall/日志/panic；
//! 见 core-v1 §10（F-01..F-44）、§13（R-DROP-01..14）与 §7（四 worker 静态 DAG / worker-ack）。
//! 本文件 [RT] 入口只读写调用方预分配固定缓冲与原子 ack；全部任务分配由 [NONRT]
//! WorkerDagPlan::build 一次完成，运行期不 work-steal、不动态均衡、不创建线程。
//!
//! 实现范围与语义（core-v1 §7.1-§7.7；WBS T09；依赖 T07 direct）：
//! - 静态任务表：B1 输出角色 tile、B2 输入列（k）tap tile、B3 输出角色 tile；
//!   每个 tile 在 plan 中固定 owner（u_owner / k_lane+lane_worker / p_owner），
//!   RT 只执行自己的 tile，禁止 steal。
//! - 私有 partial：每个 reduce lane 每个输出角色一条固定 f32 缓冲（结构体 64 B 对齐、
//!   lane 间不共享 cache line）；B2 各 lane 独立累加，B3 由输出角色 owner 按 lane 0..3
//!   固定顺序 reduce（不跳序、不用原子浮点/无序归约）。
//! - 固定 reduce/累加顺序与 worker 数无关：同一 plan 在 W=1/2/4 下浮点输出逐位一致
//!   （tests/T09 用 f32::to_bits 真实断言；W=1 只是 benchmark/对拍口径，不授予生产资格）。
//! - 有界 B1-B3：ack 位图 + cutoff；worker 在 phase 边界做有界 spin（spin_max_iters），
//!   超界/迟到使整块 invalid，master 不发布半图；迟到 worker 的旧 ack 槽标签必须被拒绝。
//! - epoch：block/ack 标签 (epoch_id, block_seq, plan_version) 不一致即丢弃；
//!   2 核降级/重准入是独立 plan + 新 epoch，禁止在旧 epoch 上用 2 个 worker 继续半图。
//!
//! 边界（本 leaf 不改公共根；integrator/T26 适配）：
//! - core::rt::RtControl 是 T06 scaffold 的简化结构，缺 core-v1 v1.1 §11.13 的
//!   block_start_seq 配方；本文件按 §11.13 定义 leaf-local KhaenRtControlV1
//!   （128 B / align 64，字段顺序与规范一致，tests/T09 断言 offset），待 integrator
//!   提升到 core::abi 后删除本地定义（与 T12 "plan 字节打包不在写域" 同类处理）。
//! - 生产 RT 线程入口 run_worker/run_master（core-v1 §12.3）需要 T16 SPSC、T18
//!   admission、T26 主机 arena/block 源；本 leaf 提供同语义的 [RT] worker/master 步进
//!   函数（worker_begin_block / master_start_block / worker_phase_b1..b3 / wait_phase_done
//!   / ack_phase），T26 组合成线程循环。不提供空转 ! 循环冒充已实现线程。
//! - partial 缓冲按"一块一份"提供；quarantine 槽与 partial 槽的绑定/不回收属于 T20/T21
//!   的 host arena 设计；本文件提供 quota/状态模型 WorkerQuarantine + WorkerAckSlotPool
//!   供其复用（§7.3/§7.6 的 ack 槽生命周期与 all-ack 前不复用）。
//! - FFT 频点 tile（partitioned 路径）不在本 leaf：WBS T56（deps T09/T12/T55）负责全图
//!   hybrid DAG 的 FFT/G/P/reduce 分派；本文件的 phase/tile owner/reduce 结构是它的基座。
//!
//! 与 T07 direct 的关系：本文件是 direct 参考语义（rt-stack/reference/SPEC.md §2）的
//! 4 worker 分块执行；线性卷积历史沿用 direct.rs 的 x_past/u_past/v_past block 约定。
//! 由于 B2 把 k 列分到 reduce lane 后按 lane 顺序求和，加法顺序与 T07 单线程 direct
//! 不同，因此与 T07 f32 的判据是 T07 冻结的 f32 工程门（相对 L2 <= 1e-5 等）；而
//! W=1/2/4 之间必须逐位一致（同一 plan 的固定 tile/reduce 顺序）。

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::abi::{
    KhaenBlockAckV1, KhaenBlockDescV1, KhaenWorkerStatsV1, ABI_VERSION, KHAEN_RING_MAGIC,
};
use crate::direct::{
    DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_BLOCK, DIRECT_MAX_ROLES,
    DIRECT_MAX_TAPS, DIRECT_NO_SOURCE,
};
use crate::rt::{
    RtStatus, WorkerId, PARALLEL_PHASES, QUARANTINE_SLOTS, RT_WORKERS, RT_WORKERS_FALLBACK,
};

// ---------------------------------------------------------------------------
// 固定容量/常量（core-v1 §3.3/§3.4；见文件尾静态断言）
// ---------------------------------------------------------------------------

/// 生产 worker 数（core-v1 §3.3 RT_WORKERS）。
pub const WORKER_COUNT_MAX: usize = RT_WORKERS as usize;
/// 2 核降级 profile 的 worker 数（独立 plan/epoch，不是失核后继续半图）。
pub const WORKER_COUNT_FALLBACK: usize = RT_WORKERS_FALLBACK as usize;
/// phase 数 B1/B2/B3（core-v1 §3.3 PARALLEL_PHASES）。
pub const WORKER_PHASE_COUNT: usize = PARALLEL_PHASES as usize;
/// reduce lane 数（= worker 上限；lane 是静态算术分组，与运行期 worker 数解耦）。
pub const WORKER_REDUCE_LANES: usize = WORKER_COUNT_MAX;
/// ack 槽数（core-v1 §3.4 ACK_SLOTS；与 playback ring 对齐）。
pub const WORKER_ACK_SLOTS: usize = 4;
/// quarantine 槽数（core-v1 §3.3 QUARANTINE_SLOTS）。
pub const WORKER_QUARANTINE_SLOTS: usize = QUARANTINE_SLOTS;
/// 单 block 样本上限（= direct DIRECT_MAX_BLOCK = 128）。
pub const WORKER_BLOCK_MAX: usize = DIRECT_MAX_BLOCK;
/// 每 phase tile 上限（= 输出角色上限 22；direct path 的 tile 以角色/输入列为单位）。
pub const WORKER_ROLES_MAX: usize = DIRECT_MAX_ROLES;
/// partial 每条（lane, role）的样本 stride（固定 128，与 block 大小解耦）。
pub const WORKER_PARTIAL_ROLE_STRIDE: usize = WORKER_BLOCK_MAX;
/// partial 每个 lane 的样本 stride（22 角色 x 128 样本）。
pub const WORKER_PARTIAL_LANE_STRIDE: usize = WORKER_ROLES_MAX * WORKER_PARTIAL_ROLE_STRIDE;
/// partial 总样本数（4 lane x 22 角色 x 128 样本 = 11264 f32 = 45056 B）。
pub const WORKER_PARTIAL_SAMPLES: usize = WORKER_REDUCE_LANES * WORKER_PARTIAL_LANE_STRIDE;
/// 默认有界 spin 上界（迭代次数；T18 admission 按目标核标定后覆盖，plan 不携带未标定值）。
pub const WORKER_DEFAULT_SPIN_MAX_ITERS: u32 = 1_000_000;
/// 默认 cutoff drain 上界（cutoff 置位后允许迟到 worker 收尾的有界迭代数）。
pub const WORKER_DEFAULT_CUTOFF_DRAIN_ITERS: u32 = 4096;
/// ack 槽状态：FREE（可初始化）。
pub const WORKER_ACK_SLOT_FREE: u32 = 0;
/// ack 槽状态：INIT（master 已写标签，worker 可写位）。
pub const WORKER_ACK_SLOT_INIT: u32 = 1;
/// ack 槽状态：WAIT（已发布 block_start，等待 all-ack）。
pub const WORKER_ACK_SLOT_WAIT: u32 = 2;
/// ack 槽状态：ALL_ACK（expected 位全部到齐且 cutoff=0）。
pub const WORKER_ACK_SLOT_ALL_ACK: u32 = 3;
/// ack 槽状态：RECYCLABLE（playback 已读完 / quarantine 已 all-ack，nonRT 可回收）。
pub const WORKER_ACK_SLOT_RECYCLABLE: u32 = 4;
/// ack 槽状态非法值上界（>= 此值 = fault）。
pub const WORKER_ACK_SLOT_STATE_MAX: u32 = 5;
/// ack.flags bit0：本 ack 槽有效（master 初始化时置位）。
pub const WORKER_ACK_FLAG_VALID: u8 = 1;
/// ack.flags bit1：late_seen（本 leaf 记录在 worker 本地 stats.flags，见 §7.3 说明）。
pub const WORKER_ACK_FLAG_LATE_SEEN: u8 = 2;
/// ack.flags 允许位掩码（其余位必须 0，否则 RT_E_ABI_RUNTIME）。
pub const WORKER_ACK_FLAG_MASK: u8 = WORKER_ACK_FLAG_VALID | WORKER_ACK_FLAG_LATE_SEEN;
/// KhaenWorkerStatsV1.flags bit0：alive。
pub const WORKER_STAT_FLAG_ALIVE: u64 = 1;
/// KhaenWorkerStatsV1.flags bit1：last_block_late（迟到 worker 记录自身，见 §7.3）。
pub const WORKER_STAT_FLAG_LAST_BLOCK_LATE: u64 = 2;
/// KhaenWorkerStatsV1.flags bit2：cutoff_seen。
pub const WORKER_STAT_FLAG_CUTOFF_SEEN: u64 = 4;
/// quarantine 槽：空闲。
pub const WORKER_QUARANTINE_FREE: u32 = 0;
/// quarantine 槽：占用且等待迟到 worker all-ack。
pub const WORKER_QUARANTINE_OCCUPIED: u32 = 1;
/// plan 切换状态：ABORT（降级/重准入请求；master 不得再启动新 block）。core-v1 §11.13
/// switch_state 枚举 0=IDLE/1=ARMED/2=SWITCHED/3=ABORT；rt.rs 只冻结 IDLE/ARMED，本地补 ABORT。
pub const WORKER_SWITCH_ABORT: u32 = 3;
/// core-v1 §9.2 code 308（RT_E_ACK_SLOT_EXHAUSTED）：T06 rt.rs scaffold 尚无该常量，
/// 本 leaf 用同值 leaf-local 常量。
///
/// INTEGRATION-TODO（T09-review I05；T06/integrator）：在 core-v1 §9.2 常量表中补齐
/// 308（及 309 RT_E_CROSSFADE_ALIGN），在 core::rt::RtStatus 增加规范常量后删除本
/// leaf 定义并替换引用；T26 集成前必须完成（否则错误码来源分裂）。
pub const RT_E_ACK_SLOT_EXHAUSTED: RtStatus = RtStatus::from_code(308);
/// RtControl.flags bit1：two_worker profile（= plan.expected_workers==2）。
pub const WORKER_CONTROL_FLAG_TWO_WORKER: u32 = 0x2;
/// RtControl.flags bit0：rt_mode（仅诊断/准入）。
pub const WORKER_CONTROL_FLAG_RT_MODE: u32 = 0x1;

// ---------------------------------------------------------------------------
// §11.13 RtControl 的 leaf-local 定义（128 B / align 64；待 integrator 提升到 core::abi）
// ---------------------------------------------------------------------------

/// [RT] 跨 master/worker 的 RT 控制块（core-v1 v1.1.1 §11.13，128 B / align 64）。
///
/// 说明：T06 scaffold 的 core::rt::RtControl 缺 block_start_seq 配方与多数字段；本 leaf
/// 按规范定一份 leaf-local 版本供 T09 协议测试与 T26 适配（字段顺序/偏移与 §11.13 一致）。
/// 全部字段为原子/普通 POD，无 Drop；地址在 admission 后稳定（INV-L3）。
///
/// INTEGRATION-TODO（T09-review I05；T06/integrator + T02 确认）：
/// 1. 把本类型提升到 core::abi（§11.13 唯一来源），workers.rs/T26 改为 import 公共类型；
/// 2. 删除本 leaf-local 定义与 tests/T09 的重复 offset 断言（保留 abi-layout 的权威断言）；
/// 3. T02 确认 flags 用 AtomicU32（§11.13 原文 plain u32；本 leaf 为了在共享引用下发布
///    two_worker profile 改为原子，大小/偏移不变）；
/// 4. 同时提升 308/309 常量（见 RT_E_ACK_SLOT_EXHAUSTED 的 INTEGRATION-TODO）。
/// 在完成 1–4 前，本类型只保证布局/语义与规范一致，不代表 abi 已冻结。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct KhaenRtControlV1 {
    /// = KHAEN_RING_MAGIC（复用 ring 魔数区分结构）。
    pub magic: u32,
    /// = ABI_VERSION。
    pub abi_version: u16,
    /// = 128。
    pub size: u16,
    /// master 每 block 描述写入后 Release fetch_add；worker Acquire 读（§6.4 I05）。
    pub block_start_seq: AtomicU64,
    /// 本 block epoch_id。
    pub block_start_epoch: AtomicU64,
    /// 本 block frame_index。
    pub block_start_frame: AtomicU64,
    /// 本 block plan_version。
    pub block_start_plan_version: AtomicU32,
    /// 本 block 的 payload/ack 槽号。
    pub block_start_slot: AtomicU32,
    /// 当前 plan arena 基址（0=null）。
    pub current_plan_ptr: AtomicU64,
    /// 当前生效 plan 版本（与 ptr 同步发布）。
    pub current_plan_version: AtomicU32,
    /// ARMED 时新 plan 版本。
    pub pending_plan_version: AtomicU32,
    /// ARMED 时新 plan 基址。
    pub pending_plan_ptr: AtomicU64,
    /// 0=IDLE、1=ARMED、2=SWITCHED、3=ABORT。
    pub switch_state: AtomicU32,
    /// 0=none、1=mute、2=fault（只置位，安全请求）。
    pub safety_request: AtomicU32,
    /// 最后一个 RtStatus 原因。
    pub safety_reason: u32,
    /// bit0=rt_mode、bit1=two_worker，其余 0。
    ///
    /// 注：core-v1 §11.13 原文为 plain u32；本 leaf 用 AtomicU32 以便 control 在共享
    /// 引用（&KhaenRtControlV1）下发布/读取 two_worker profile，大小/偏移不变（4 B @76），
    /// 待 T02 编辑者确认后由 integrator 同步到 core::abi（登记为 leaf 契约偏差）。
    pub flags: AtomicU32,
    /// master 每 block Release fetch_add；watchdog 只读。
    pub heartbeat_seq: AtomicU64,
    /// admission 标定的 spin 上界。
    pub spin_max_iters: AtomicU32,
    /// admission 标定的 cutoff drain 上界。
    pub cutoff_drain_iters: AtomicU32,
    /// tick -> ns 换算（admission 标定）。
    pub tick_freq_hz: u32,
    /// 每 block 预算内 tick 读数上限（<=3）。
    pub tick_read_budget: u32,
    /// capture ring 在 RtArena 的偏移（本 leaf 未使用）。
    pub capture_ring_offset: u64,
    /// playback ring 在 RtArena 的偏移（本 leaf 未使用）。
    pub playback_ring_offset: u64,
    /// 必须为 0。
    pub reserved: u64,
}

impl KhaenRtControlV1 {
    /// 构造全零/规范初值控制块（admission 前/测试）。
    pub const fn new() -> Self {
        Self {
            magic: KHAEN_RING_MAGIC,
            abi_version: ABI_VERSION,
            size: 128,
            block_start_seq: AtomicU64::new(0),
            block_start_epoch: AtomicU64::new(0),
            block_start_frame: AtomicU64::new(0),
            block_start_plan_version: AtomicU32::new(0),
            block_start_slot: AtomicU32::new(0),
            current_plan_ptr: AtomicU64::new(0),
            current_plan_version: AtomicU32::new(0),
            pending_plan_version: AtomicU32::new(0),
            pending_plan_ptr: AtomicU64::new(0),
            switch_state: AtomicU32::new(0),
            safety_request: AtomicU32::new(0),
            safety_reason: 0,
            flags: AtomicU32::new(0),
            heartbeat_seq: AtomicU64::new(0),
            spin_max_iters: AtomicU32::new(WORKER_DEFAULT_SPIN_MAX_ITERS),
            cutoff_drain_iters: AtomicU32::new(WORKER_DEFAULT_CUTOFF_DRAIN_ITERS),
            tick_freq_hz: 0,
            tick_read_budget: 0,
            capture_ring_offset: 0,
            playback_ring_offset: 0,
            reserved: 0,
        }
    }
}

impl Default for KhaenRtControlV1 {
    fn default() -> Self {
        Self::new()
    }
}

/// [RT] 一个 block 的启动身份（§6.4 block_start 配方的四种原子字段快照，POD/Copy）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerBlockStart {
    /// 本 block epoch_id。
    pub epoch_id: u64,
    /// 本 block frame_index。
    pub frame_index: u64,
    /// 本 block plan_version。
    pub plan_version: u32,
    /// 本 block 的 payload/ack 槽号。
    pub slot: u32,
}

impl WorkerBlockStart {
    /// 全零占位（仅测试/初始化）。
    pub const ZERO: Self = Self {
        epoch_id: 0,
        frame_index: 0,
        plan_version: 0,
        slot: 0,
    };
}

fn control_valid(ctl: &KhaenRtControlV1) -> RtStatus {
    if ctl.magic != KHAEN_RING_MAGIC || ctl.abi_version != ABI_VERSION || ctl.size != 128 {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    RtStatus::OK
}

/// [RT] master 发布 block_start：先写普通原子字段，最后 Release fetch_add 序号（§6.4/I05）。
///
/// 返回 RtStatus；调用者（master）必须在 worker 可读前完成全部描述写。
pub fn publish_block_start(
    ctl: &KhaenRtControlV1,
    epoch_id: u64,
    frame_index: u64,
    plan_version: u32,
    slot: u32,
) -> RtStatus {
    let status = control_valid(ctl);
    if !status.is_ok() {
        return status;
    }
    ctl.block_start_epoch.store(epoch_id, Ordering::Relaxed);
    ctl.block_start_frame.store(frame_index, Ordering::Relaxed);
    ctl.block_start_plan_version
        .store(plan_version, Ordering::Relaxed);
    ctl.block_start_slot.store(slot, Ordering::Relaxed);
    ctl.block_start_seq.fetch_add(1, Ordering::Release);
    RtStatus::OK
}

/// [RT] 读取当前 block_start 身份字段（必须在观察到序号递增后调用；字段为 Relaxed 读）。
pub fn read_block_start(ctl: &KhaenRtControlV1) -> WorkerBlockStart {
    WorkerBlockStart {
        epoch_id: ctl.block_start_epoch.load(Ordering::Relaxed),
        frame_index: ctl.block_start_frame.load(Ordering::Relaxed),
        plan_version: ctl.block_start_plan_version.load(Ordering::Relaxed),
        slot: ctl.block_start_slot.load(Ordering::Relaxed),
    }
}

/// [RT] worker 有界等待新 block_start_seq（Acquire 读；spins_used 记录实际迭代）。
///
/// 返回新序号；序号回退 = RT_E_INVARIANT，safety_request 置位 = RT_E_WATCHDOG，
/// spin 上界耗尽 = RT_E_CUTOFF（有界等待，绝不无界）。
pub fn wait_block_start(
    ctl: &KhaenRtControlV1,
    last_seq: u64,
    spin_max_iters: u32,
) -> Result<u64, RtStatus> {
    let status = control_valid(ctl);
    if !status.is_ok() {
        return Err(status);
    }
    if ctl.safety_request.load(Ordering::Acquire) != 0 {
        return Err(RtStatus::RT_E_WATCHDOG);
    }
    let mut spin = 0u32;
    loop {
        let seq = ctl.block_start_seq.load(Ordering::Acquire);
        if seq != last_seq {
            if seq < last_seq {
                return Err(RtStatus::RT_E_INVARIANT);
            }
            return Ok(seq);
        }
        if spin >= spin_max_iters {
            return Err(RtStatus::RT_E_CUTOFF);
        }
        spin = spin.wrapping_add(1);
        core::hint::spin_loop();
    }
}

/// [RT] worker 消费一个 block_start：要求序号恰好 +1（禁止跳变/重放，§6.4）。
pub fn consume_block_start(
    ctl: &KhaenRtControlV1,
    last_seq: u64,
    spin_max_iters: u32,
) -> Result<(u64, WorkerBlockStart), RtStatus> {
    let seq = wait_block_start(ctl, last_seq, spin_max_iters)?;
    let expected = match last_seq.checked_add(1) {
        Some(value) => value,
        None => return Err(RtStatus::RT_E_INVARIANT),
    };
    if seq != expected {
        return Err(RtStatus::RT_E_INVARIANT);
    }
    Ok((seq, read_block_start(ctl)))
}

// ---------------------------------------------------------------------------
// 静态 phase DAG plan（[NONRT] 构建一次，RT 只读；无分配/无 Drop）
// ---------------------------------------------------------------------------

/// [NONRT]/[RT] 四 worker 静态 phase DAG 的计划（Copy/POD；由 CTL 构建，经引用进入 RT）。
///
/// 任务表按 phase 固定：
/// - B1：task i = 输出角色 i 的 u 任务（输入侧 A FIR），owner = u_owner[i]；
/// - B2：task i = 输入列 k=i 的 tap tile（G_full[j][k] * u_k 全部输出行），
///   归属 lane = k_lane[i]，执行 worker = lane_worker[lane]；
/// - B3：task i = 输出角色 i 的 reduce + P FIR + scatter tile，owner = p_owner[i]。
///
/// 每个 tile 在 plan 中恰有一个 owner（无 work stealing）；worker 数只决定"谁执行哪些
/// 固定 tile"，不改变 tile 内容或 reduce 顺序，因此 W=1/2/4 的浮点结果逐位一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerDagPlan {
    /// 参考语义图拓扑（A_map/a/g/p/scatter/D，见 direct::DirectTopology）。
    pub topology: DirectTopology,
    /// direct 固定缓冲布局（x_past/u_past/v_past/u_cur/v_cur 偏移）。
    pub layout: DirectLayout,
    /// 本 plan 的 block 大小（生产准入 64/128；更小值只用于测试流式覆盖）。
    pub block_size: u16,
    /// 本 plan 的 worker 数（1/2/4；1 仅 benchmark/对拍，不授予生产资格）。
    pub expected_workers: u8,
    /// plan 版本（进程内单调；进 ack 标签，不参与图 hash）。
    pub plan_version: u32,
    /// reduce lane 数（固定 4，见 WORKER_REDUCE_LANES）。
    pub reduce_lanes: u8,
    /// B1 tile owner：输出角色 -> worker。
    pub u_owner: [u8; WORKER_ROLES_MAX],
    /// B2 tile lane：输入列 k -> reduce lane。
    pub k_lane: [u8; WORKER_ROLES_MAX],
    /// reduce lane -> 执行 worker。
    pub lane_worker: [u8; WORKER_REDUCE_LANES],
    /// B3 tile owner：输出角色 -> worker。
    pub p_owner: [u8; WORKER_ROLES_MAX],
    /// lane 是否分配到输入列（1=active；inactive lane 不参与 reduce，保证 W 无关）。
    pub lane_active: [u8; WORKER_REDUCE_LANES],
}

impl WorkerDagPlan {
    /// [NONRT] 构建 plan：校验拓扑/容量，生成静态 tile->worker/lane 映射；无堆分配。
    pub fn build(
        topology: &DirectTopology,
        block_size: u16,
        expected_workers: u8,
        plan_version: u32,
    ) -> Result<Self, RtStatus> {
        let status = topology.validate();
        if !status.is_ok() {
            return Err(status);
        }
        let layout = match DirectLayout::build(topology) {
            Ok(value) => value,
            Err(err) => return Err(err),
        };
        if block_size == 0 || block_size as usize > DIRECT_MAX_BLOCK {
            return Err(RtStatus::ERR_BLOCK_SIZE);
        }
        if !valid_worker_count(expected_workers) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let n_role = topology.n_role as usize;
        let workers = expected_workers as usize;
        let mut u_owner = [0u8; WORKER_ROLES_MAX];
        let mut k_lane = [0u8; WORKER_ROLES_MAX];
        let mut p_owner = [0u8; WORKER_ROLES_MAX];
        let mut lane_worker = [0u8; WORKER_REDUCE_LANES];
        let mut lane_active = [0u8; WORKER_REDUCE_LANES];
        let mut k = 0usize;
        while k < n_role {
            let lane = lane_of_k(k, n_role);
            k_lane[k] = lane;
            lane_active[lane as usize] = 1;
            k += 1;
        }
        let mut lane = 0usize;
        while lane < WORKER_REDUCE_LANES {
            lane_worker[lane] = (lane % workers) as u8;
            lane += 1;
        }
        let mut j = 0usize;
        while j < n_role {
            u_owner[j] = (j % workers) as u8;
            p_owner[j] = (j % workers) as u8;
            j += 1;
        }
        let plan = Self {
            topology: *topology,
            layout,
            block_size,
            expected_workers,
            plan_version,
            reduce_lanes: WORKER_REDUCE_LANES as u8,
            u_owner,
            k_lane,
            lane_worker,
            p_owner,
            lane_active,
        };
        let status = plan.validate();
        if !status.is_ok() {
            return Err(status);
        }
        Ok(plan)
    }

    /// [RT] plan 自校验（RT 只读结构；任何不一致返回固定错误码，不 panic）。
    pub fn validate(&self) -> RtStatus {
        let status = self.topology.validate();
        if !status.is_ok() {
            return status;
        }
        match DirectLayout::build(&self.topology) {
            Ok(expected) => {
                if expected != self.layout {
                    return RtStatus::ERR_BAD_LAYOUT;
                }
            }
            Err(err) => return err,
        }
        if self.block_size == 0 || self.block_size as usize > DIRECT_MAX_BLOCK {
            return RtStatus::ERR_BLOCK_SIZE;
        }
        if !valid_worker_count(self.expected_workers) {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        if self.reduce_lanes as usize != WORKER_REDUCE_LANES {
            return RtStatus::ERR_BAD_LAYOUT;
        }
        let n_role = self.topology.n_role as usize;
        let workers = self.expected_workers as usize;
        let mut lane_owner = [0u8; WORKER_ROLES_MAX];
        let mut k = 0usize;
        while k < n_role {
            let expected_lane = lane_of_k(k, n_role);
            if self.k_lane[k] != expected_lane || self.lane_active[expected_lane as usize] != 1 {
                return RtStatus::ERR_BAD_LAYOUT;
            }
            lane_owner[k] = expected_lane;
            k += 1;
        }
        let mut lane = 0usize;
        while lane < WORKER_REDUCE_LANES {
            if self.lane_worker[lane] as usize >= workers {
                return RtStatus::ERR_BAD_LAYOUT;
            }
            if self.lane_worker[lane] as usize != lane % workers {
                return RtStatus::ERR_BAD_LAYOUT;
            }
            let mut has_k = false;
            let mut probe = 0usize;
            while probe < n_role {
                if lane_owner[probe] as usize == lane {
                    has_k = true;
                }
                probe += 1;
            }
            let expected_active = if has_k { 1 } else { 0 };
            if self.lane_active[lane] != expected_active {
                return RtStatus::ERR_BAD_LAYOUT;
            }
            lane += 1;
        }
        let mut j = 0usize;
        while j < n_role {
            if self.u_owner[j] as usize >= workers || self.p_owner[j] as usize >= workers {
                return RtStatus::ERR_BAD_LAYOUT;
            }
            j += 1;
        }
        RtStatus::OK
    }

    /// 每 phase 的 tile 数（direct path：每个输出角色一个 B1/B3 tile，每个输入列一个 B2 tile）。
    pub const fn phase_task_count(&self, phase: u8) -> u8 {
        if phase >= WORKER_PHASE_COUNT as u8 {
            0
        } else {
            self.topology.n_role as u8
        }
    }

    /// 本 plan 全部 phase 的 tile 总数。
    pub const fn total_tasks(&self) -> u16 {
        3 * self.topology.n_role
    }

    /// tile owner（phase, task）-> worker；越界返回 None（RT 以错误码处理，不 panic）。
    pub fn task_owner(&self, phase: u8, task: u8) -> Option<WorkerId> {
        if phase >= WORKER_PHASE_COUNT as u8 || task as u16 >= self.topology.n_role {
            return None;
        }
        let index = task as usize;
        let owner = if phase == 1 {
            let lane = self.k_lane[index] as usize;
            self.lane_worker[lane]
        } else if phase == 2 {
            self.p_owner[index]
        } else {
            self.u_owner[index]
        };
        Some(owner)
    }

    /// 某 worker 在 phase 内的 tile 数（无 stealing；W=1 时为全部 tile）。
    pub fn worker_task_count(&self, phase: u8, worker: WorkerId) -> u8 {
        if phase >= WORKER_PHASE_COUNT as u8 || worker >= self.expected_workers {
            return 0;
        }
        let tasks = self.topology.n_role as u8;
        let mut count = 0u8;
        let mut task = 0u8;
        while task < tasks {
            if self.task_owner(phase, task) == Some(worker) {
                count = count + 1;
            }
            task = task + 1;
        }
        count
    }

    /// phase 并行度不足时如实记录的串行比例（Q16）：max(0, tasks - W) / tasks。
    ///
    /// 不 spin 凑忙；worker 数超过 tile 数时只如实空出 worker（见 worker_phase_idle_slots）。
    pub fn serial_fraction_q16(&self, phase: u8) -> u32 {
        let tasks = self.phase_task_count(phase) as u64;
        if tasks == 0 {
            return 0;
        }
        let workers = self.expected_workers as u64;
        if tasks <= workers {
            return 0;
        }
        (((tasks - workers) * 65536) / tasks) as u32
    }

    /// phase 内实际并行 worker 数（= min(tasks, W)）。
    pub fn phase_parallel_workers(&self, phase: u8) -> u8 {
        let tasks = self.phase_task_count(phase);
        if tasks < self.expected_workers {
            tasks
        } else {
            self.expected_workers
        }
    }

    /// 是否 2 核降级 profile（独立 plan，不是失核后继续半图）。
    pub const fn is_two_core_profile(&self) -> bool {
        self.expected_workers == RT_WORKERS_FALLBACK
    }
}

fn valid_worker_count(expected_workers: u8) -> bool {
    expected_workers == 1
        || expected_workers == WORKER_COUNT_FALLBACK as u8
        || expected_workers == WORKER_COUNT_MAX as u8
}

fn lane_of_k(k: usize, n_role: usize) -> u8 {
    if n_role == 0 {
        return 0;
    }
    ((k * WORKER_REDUCE_LANES) / n_role) as u8
}

// ---------------------------------------------------------------------------
// 私有 partial（每 lane 每输出角色固定缓冲；64 B 对齐 + lane 间 cache line 隔离）
// ---------------------------------------------------------------------------

/// [RT] 四 worker 私有 partial 缓冲：4 reduce lane x 22 角色 x 128 样本 f32。
///
/// 结构体按 64 B 对齐；每 lane 间隔 11264 B（=176 个 64 B cache line 的整数倍），
/// 每角色间隔 512 B，因此相邻 worker/lane 不会写同一 cache line（F-34）。
/// 一块一份；quarantine 槽与本缓冲的绑定由 host arena（T20/T21）负责，all-ack 前禁用。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct WorkerPartials {
    data: [f32; WORKER_PARTIAL_SAMPLES],
}

impl WorkerPartials {
    /// 全零构造（admission 时一次分配；无堆分配）。
    pub const fn new() -> Self {
        Self {
            data: [0.0f32; WORKER_PARTIAL_SAMPLES],
        }
    }

    /// 清零（每 block 开始前由 master 在发布 block_start 之前完成）。
    pub fn zero(&mut self) {
        let mut index = 0usize;
        while index < WORKER_PARTIAL_SAMPLES {
            self.data[index] = 0.0;
            index += 1;
        }
    }

    /// 只读（lane, role）的 128 样本 partial 切片；越界返回 None。
    pub fn lane_role(&self, lane: usize, role: usize) -> Option<&[f32]> {
        if lane >= WORKER_REDUCE_LANES || role >= WORKER_ROLES_MAX {
            return None;
        }
        let offset = lane * WORKER_PARTIAL_LANE_STRIDE + role * WORKER_PARTIAL_ROLE_STRIDE;
        self.data.get(offset..offset + WORKER_PARTIAL_ROLE_STRIDE)
    }

    /// 可写（lane, role）的 128 样本 partial 切片；越界返回 None。
    pub fn lane_role_mut(&mut self, lane: usize, role: usize) -> Option<&mut [f32]> {
        if lane >= WORKER_REDUCE_LANES || role >= WORKER_ROLES_MAX {
            return None;
        }
        let offset = lane * WORKER_PARTIAL_LANE_STRIDE + role * WORKER_PARTIAL_ROLE_STRIDE;
        self.data
            .get_mut(offset..offset + WORKER_PARTIAL_ROLE_STRIDE)
    }

    /// 原始样本视图（测试/证据用；RT 内部用 lane_role*）。
    pub fn as_samples(&self) -> &[f32] {
        &self.data
    }
}

impl Default for WorkerPartials {
    fn default() -> Self {
        Self::new()
    }
}

/// [NONRT] 清零 direct 状态缓冲（新 epoch/首块前；只写固定缓冲，无分配）。
pub fn worker_state_zero(state: &mut [f32]) {
    let mut index = 0usize;
    while index < state.len() {
        state[index] = 0.0;
        index += 1;
    }
}

// ---------------------------------------------------------------------------
// [RT] B1/B2/B3 phase 执行（每个 worker 只执行本 worker 的固定 tile；无 stealing）
// ---------------------------------------------------------------------------

fn phase_validate_core(
    plan: &WorkerDagPlan,
    coeffs: &DirectCoeffsF32<'_>,
    state_len: usize,
    valid_frames: usize,
    worker: WorkerId,
) -> RtStatus {
    let status = plan.validate();
    if !status.is_ok() {
        return status;
    }
    let topo = &plan.topology;
    let n_src = topo.n_src as usize;
    let n_role = topo.n_role as usize;
    let l_a = topo.l_a as usize;
    let l_g = topo.l_g as usize;
    let l_p = topo.l_p as usize;
    let m = topo.m as usize;
    let a_len = match n_src.checked_mul(l_a) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    let g_len = match m.checked_mul(m) {
        Some(value) => match value.checked_mul(l_g) {
            Some(total) => total,
            None => return RtStatus::ERR_CAPACITY,
        },
        None => return RtStatus::ERR_CAPACITY,
    };
    let p_len = match n_role.checked_mul(l_p) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    if coeffs.a_fir.len() != a_len || coeffs.g.len() != g_len || coeffs.p.len() != p_len {
        return RtStatus::ERR_PATH_TABLE;
    }
    if state_len < plan.layout.state_samples() {
        return RtStatus::ERR_CAPACITY;
    }
    if valid_frames > plan.block_size as usize {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    if worker >= plan.expected_workers {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    RtStatus::OK
}

fn build_part_pos(plan: &WorkerDagPlan) -> [u16; WORKER_ROLES_MAX] {
    let topo = &plan.topology;
    let mut part_pos = [DIRECT_NO_SOURCE; WORKER_ROLES_MAX];
    let m = topo.m as usize;
    let mut index = 0usize;
    while index < m {
        part_pos[topo.participants[index] as usize] = index as u16;
        index += 1;
    }
    part_pos
}

/// [RT] B1：本 worker 负责的输出角色 u 任务（输入侧 A FIR）+ x_past 历史更新。
///
/// 只写本 worker 的 u_cur[role] 与其源通道 x_past（A_map 单射保证同一源只被一个 tile 写）。
pub fn worker_phase_b1(
    plan: &WorkerDagPlan,
    worker: WorkerId,
    coeffs: &DirectCoeffsF32<'_>,
    state: &mut [f32],
    input: &[f32],
    in_stride: usize,
    valid_frames: usize,
) -> RtStatus {
    let status = phase_validate_core(plan, coeffs, state.len(), valid_frames, worker);
    if !status.is_ok() {
        return status;
    }
    let topo = &plan.topology;
    let layout = &plan.layout;
    let n_src = topo.n_src as usize;
    let n_role = topo.n_role as usize;
    let l_a = topo.l_a as usize;
    let block = WORKER_BLOCK_MAX;
    let u_cur_off = layout.u_cur_off as usize;
    let x_past_off = layout.x_past_off as usize;
    let x_past_len = if l_a > 0 { l_a - 1 } else { 0 };
    if in_stride < plan.block_size as usize {
        return RtStatus::ERR_STRIDE_ALIGN;
    }
    let need_in = match n_src.checked_mul(in_stride) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    if input.len() < need_in {
        return RtStatus::ERR_CAPACITY;
    }
    let mut task = 0usize;
    while task < n_role {
        if plan.u_owner[task] == worker {
            let u_base = u_cur_off + task * block;
            let src = topo.src_of_role[task];
            if src == DIRECT_NO_SOURCE {
                let mut n = 0usize;
                while n < valid_frames {
                    state[u_base + n] = 0.0;
                    n += 1;
                }
            } else {
                let si = src as usize;
                let in_base = si * in_stride;
                let a_base = si * l_a;
                let xp_base = x_past_off + si * x_past_len;
                let mut n = 0usize;
                while n < valid_frames {
                    state[u_base + n] = 0.0;
                    n += 1;
                }
                let mut tap = 0usize;
                while tap < l_a {
                    let coef = coeffs.a_fir[a_base + tap];
                    let mut n = 0usize;
                    while n < valid_frames {
                        let value = if n >= tap {
                            input[in_base + n - tap]
                        } else {
                            state[xp_base + (tap - 1 - n)]
                        };
                        state[u_base + n] = state[u_base + n] + coef * value;
                        n += 1;
                    }
                    tap += 1;
                }
                // 历史约定与 direct.rs 相同：x_past[s][t] = x_s[block_start-1-t]；
                // 下降序保证读取的是更新前的旧历史（valid<block 时不推进零填充）。
                let mut t = x_past_len;
                while t > 0 {
                    t -= 1;
                    let value = if t < valid_frames {
                        input[in_base + (valid_frames - 1 - t)]
                    } else {
                        state[xp_base + (t - valid_frames)]
                    };
                    state[xp_base + t] = value;
                }
            }
        }
        task += 1;
    }
    RtStatus::OK
}

/// [RT] B2：本 worker 负责 lane 的输入列 tap tile（G_full[j][k] * u_k 全部输出行）。
///
/// 每个 reduce lane 独立累加到自己的私有 partial；同一 lane 内 k 升序、t 升序累加。
/// 需要 B1 全部 all-ack 后才能读 u_cur/u_past（phase 依赖，由调用方 barrier 保证）。
pub fn worker_phase_b2(
    plan: &WorkerDagPlan,
    worker: WorkerId,
    coeffs: &DirectCoeffsF32<'_>,
    state: &[f32],
    partials: &mut WorkerPartials,
    valid_frames: usize,
) -> RtStatus {
    let status = phase_validate_core(plan, coeffs, state.len(), valid_frames, worker);
    if !status.is_ok() {
        return status;
    }
    let topo = &plan.topology;
    let layout = &plan.layout;
    let n_role = topo.n_role as usize;
    let l_g = topo.l_g as usize;
    let m = topo.m as usize;
    let d = topo.d_samples as usize;
    let block = WORKER_BLOCK_MAX;
    let u_cur_off = layout.u_cur_off as usize;
    let u_past_off = layout.u_past_off as usize;
    let u_past_len = if l_g > 0 { l_g - 1 } else { 0 };
    let part_pos = build_part_pos(plan);
    // 卷积历史合并缓冲：hist[0..n_past] = u_past 的时间正序（u[block_start-n_past..]），
    // hist[n_past..] = 当前块 u[0..V]。于是 cell 卷积对每个 tap 都是
    //   part[n] += coef * hist[n_past - tap + n]   （n=0..V 连续升序）
    // 单段连续访存 + 可自动向量化（NEON FMA）；每样本累加顺序仍为 tap 升序，
    // 与旧实现 f32 逐位一致（只改变循环嵌套/访存形态，不改变任何加法顺序）。
    let mut hist = [0.0f32; DIRECT_MAX_TAPS - 1 + DIRECT_MAX_BLOCK];
    let mut task = 0usize;
    while task < n_role {
        let lane = plan.k_lane[task] as usize;
        if lane < WORKER_REDUCE_LANES && plan.lane_worker[lane] == worker {
            let k = task;
            let k_pos = part_pos[k];
            let u_cur_k = u_cur_off + k * block;
            let u_past_k = u_past_off + k * u_past_len;
            // 本 worker 该 k lane 的全部 cell 共用同一输入历史：每 k 只构造一次。
            let mut i = 0usize;
            while i < u_past_len {
                hist[i] = state[u_past_k + (u_past_len - 1 - i)];
                i += 1;
            }
            let mut n = 0usize;
            while n < valid_frames {
                hist[u_past_len + n] = state[u_cur_k + n];
                n += 1;
            }
            let hist = &hist[..u_past_len + valid_frames];
            let mut j = 0usize;
            while j < n_role {
                let j_pos = part_pos[j];
                let has_g = j_pos != DIRECT_NO_SOURCE && k_pos != DIRECT_NO_SOURCE;
                let is_diag = j == k;
                if !has_g && !is_diag {
                    // 该 cell 既无 G 系数也无 delta_D 基线：旧实现对全 tap 累加 ±0 仍是 0，
                    // 跳过与逐位等价（part 在块首已清零）。
                    j += 1;
                    continue;
                }
                // 对角 cell 的 delta_D 基线折进 tap==d 的系数（与旧实现同一 tap 位置/同一
                // 每样本累加顺序，因此 f32 逐位一致），不再需要 4096 次无效乘加。
                let delta_tap = if is_diag { d } else { usize::MAX };
                let g_base = if has_g {
                    (j_pos as usize * m + k_pos as usize) * l_g
                } else {
                    0usize
                };
                let part = match partials.lane_role_mut(lane, j) {
                    Some(slice) => &mut slice[..valid_frames],
                    None => return RtStatus::ERR_BAD_LAYOUT,
                };
                let mut tap = 0usize;
                while tap < l_g {
                    let mut coef = 0.0f32;
                    if has_g {
                        coef = coeffs.g[g_base + tap];
                    }
                    if tap == delta_tap {
                        coef = coef + 1.0;
                    }
                    if coef == 0.0 {
                        tap += 1;
                        continue;
                    }
                    let seg = &hist[u_past_len - tap..u_past_len - tap + valid_frames];
                    let mut n = 0usize;
                    while n < valid_frames {
                        part[n] = part[n] + coef * seg[n];
                        n += 1;
                    }
                    tap += 1;
                }
                j += 1;
            }
        }
        task += 1;
    }
    RtStatus::OK
}

/// [RT] B3：本 worker 负责输出角色的固定 reduce + 对角 P FIR + scatter + 有限性检查。
///
/// reduce 顺序固定为 lane 0..3（inactive lane 跳过，结果与 worker 数无关）；
/// 同时更新 u_past/v_past 历史（u_past 必须在 B2 全部完成后写，见 phase 依赖）。
pub fn worker_phase_b3(
    plan: &WorkerDagPlan,
    worker: WorkerId,
    coeffs: &DirectCoeffsF32<'_>,
    state: &mut [f32],
    partials: &WorkerPartials,
    output: &mut [f32],
    out_stride: usize,
    valid_frames: usize,
    bad_output: &AtomicU32,
) -> RtStatus {
    let status = phase_validate_core(plan, coeffs, state.len(), valid_frames, worker);
    if !status.is_ok() {
        return status;
    }
    let topo = &plan.topology;
    let layout = &plan.layout;
    let n_role = topo.n_role as usize;
    let containers = topo.container_channels as usize;
    let l_g = topo.l_g as usize;
    let l_p = topo.l_p as usize;
    let block = WORKER_BLOCK_MAX;
    let block_size = plan.block_size as usize;
    let u_cur_off = layout.u_cur_off as usize;
    let u_past_off = layout.u_past_off as usize;
    let v_cur_off = layout.v_cur_off as usize;
    let v_past_off = layout.v_past_off as usize;
    let u_past_len = if l_g > 0 { l_g - 1 } else { 0 };
    let v_past_len = if l_p > 0 { l_p - 1 } else { 0 };
    if out_stride < block_size {
        return RtStatus::ERR_STRIDE_ALIGN;
    }
    let need_out = match containers.checked_mul(out_stride) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    if output.len() < need_out {
        return RtStatus::ERR_CAPACITY;
    }
    let mut task = 0usize;
    while task < n_role {
        if plan.p_owner[task] != worker {
            task += 1;
            continue;
        }
        let j = task;
        // 1) 固定 reduce：lane 0..3 顺序；inactive lane 不存在 partial（与 W 无关）。
        let mut lane_slices: [Option<&[f32]>; WORKER_REDUCE_LANES] = [None; WORKER_REDUCE_LANES];
        let mut lane = 0usize;
        while lane < WORKER_REDUCE_LANES {
            if plan.lane_active[lane] != 0 {
                match partials.lane_role(lane, j) {
                    Some(slice) => lane_slices[lane] = Some(slice),
                    None => return RtStatus::ERR_BAD_LAYOUT,
                }
            }
            lane += 1;
        }
        let vc_base = v_cur_off + j * block;
        let mut n = 0usize;
        while n < valid_frames {
            let mut acc = 0.0f32;
            let mut lane_index = 0usize;
            while lane_index < WORKER_REDUCE_LANES {
                if let Some(slice) = lane_slices[lane_index] {
                    acc = acc + slice[n];
                }
                lane_index += 1;
            }
            state[vc_base + n] = acc;
            n += 1;
        }
        // 2) 对角 P FIR（vc/v_past；每样本 tap 升序，与 direct 相同）。
        let mut y = [0.0f32; WORKER_BLOCK_MAX];
        let p_base = j * l_p;
        let vp_base = v_past_off + j * v_past_len;
        let mut tap = 0usize;
        while tap < l_p {
            let coef = coeffs.p[p_base + tap];
            let mut n = 0usize;
            while n < valid_frames {
                let value = if n >= tap {
                    state[vc_base + n - tap]
                } else {
                    state[vp_base + (tap - 1 - n)]
                };
                y[n] = y[n] + coef * value;
                n += 1;
            }
            tap += 1;
        }
        // 3) 历史更新：u_past[j] 用 u_cur[j]；v_past[j] 用刚算出的 v_cur[j]（下降序）。
        let u_cur_base = u_cur_off + j * block;
        let up_base = u_past_off + j * u_past_len;
        let mut t = u_past_len;
        while t > 0 {
            t -= 1;
            let value = if t < valid_frames {
                state[u_cur_base + (valid_frames - 1 - t)]
            } else {
                state[up_base + (t - valid_frames)]
            };
            state[up_base + t] = value;
        }
        let mut t = v_past_len;
        while t > 0 {
            t -= 1;
            let value = if t < valid_frames {
                state[vc_base + (valid_frames - 1 - t)]
            } else {
                state[vp_base + (t - valid_frames)]
            };
            state[vp_base + t] = value;
        }
        // 4) scatter 输出（block_size 整行写；[valid,B) 与未映射容器保持严格 0）。
        let out_base = topo.scatter[j] as usize * out_stride;
        let mut nonfinite = false;
        let mut n = 0usize;
        while n < block_size {
            let value = if n < valid_frames { y[n] } else { 0.0 };
            output[out_base + n] = value;
            if !value.is_finite() {
                nonfinite = true;
            }
            n += 1;
        }
        if nonfinite {
            bad_output.store(1, Ordering::Release);
        }
        task += 1;
    }
    RtStatus::OK
}

// ---------------------------------------------------------------------------
// worker-ack 协议（core-v1 §7.3 / §11.6；expected 参数化，禁止硬写 0xF）
// ---------------------------------------------------------------------------

/// ACK_MASK(expected) = (1 << expected) - 1（core-v1 §3.4；expected 1/2/4）。
///
/// expected >= 32 时饱和为 u32::MAX（调用路径只用于 1/2/4；避免移位 UB）。
pub const fn ack_mask(expected_workers: u8) -> u32 {
    if expected_workers >= 32 {
        u32::MAX
    } else {
        (1u32 << expected_workers) - 1
    }
}

fn expected_valid(expected_workers: u8) -> bool {
    expected_workers == 1
        || expected_workers == WORKER_COUNT_FALLBACK as u8
        || expected_workers == WORKER_COUNT_MAX as u8
}

/// [RT] ack 槽静态检查（expected/flags/位图越界位；不改槽）。
pub fn ack_slot_validate(ack: &KhaenBlockAckV1) -> RtStatus {
    if !expected_valid(ack.expected_workers) {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    if (ack.flags & !WORKER_ACK_FLAG_MASK) != 0 {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    let mask = ack_mask(ack.expected_workers);
    let mut phase = 0usize;
    while phase < WORKER_PHASE_COUNT {
        let done = ack.phase_done[phase].load(Ordering::Acquire);
        if (done & !mask) != 0 {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        phase += 1;
    }
    RtStatus::OK
}

/// [RT] worker k 完成 phase：校验 expected/phase/worker 后 Release fetch_or 位图。
///
/// 本入口不做 epoch/block 标签比对（调用者已在 worker_begin_block / ack_phase_labeled
/// 完成标签校验）；标签可能过期的路径必须用 ack_phase_labeled。
pub fn ack_phase(ack: &KhaenBlockAckV1, worker: WorkerId, phase: u8) -> RtStatus {
    if phase >= WORKER_PHASE_COUNT as u8 {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    let expected = ack.expected_workers;
    if !expected_valid(expected) {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    if worker >= WORKER_COUNT_MAX as u8 || worker >= expected {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    if (ack.flags & !WORKER_ACK_FLAG_MASK) != 0 {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    let bit = 1u32 << worker;
    ack.phase_done[phase as usize].fetch_or(bit, Ordering::Release);
    RtStatus::OK
}

/// [RT] 带标签的 ack：epoch/block_seq/plan_version 与槽标签不一致时拒绝写位（I08）。
///
/// 返回 RT_E_EPOCH_MISMATCH（epoch 不符）或 RT_E_PARTIAL_LATE（block/plan 不符，迟到 worker）。
/// 迟到统计由 worker 本地 stats（KhaenWorkerStatsV1.flags bit1 / late_blocks）记录，
/// 不修改当前 ack 槽的任何位（§7.3 "不匹配不得修改当前槽"）。
pub fn ack_phase_labeled(
    ack: &KhaenBlockAckV1,
    worker: WorkerId,
    phase: u8,
    epoch_id: u64,
    block_seq: u64,
    plan_version: u32,
) -> RtStatus {
    if ack.epoch_id != epoch_id {
        return RtStatus::RT_E_EPOCH_MISMATCH;
    }
    if ack.block_seq != block_seq || ack.plan_version != plan_version {
        return RtStatus::RT_E_PARTIAL_LATE;
    }
    ack_phase(ack, worker, phase)
}

/// [RT] master 有界等待单个 phase 的 all-ack（cutoff 置位立即返回迟到）。
pub fn wait_phase_done(
    ack: &KhaenBlockAckV1,
    phase: u8,
    expected_workers: u8,
    spin_max_iters: u32,
) -> RtStatus {
    if phase >= WORKER_PHASE_COUNT as u8 {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    if !expected_valid(expected_workers) {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    if ack.expected_workers != expected_workers {
        return RtStatus::RT_E_PLAN_MISMATCH;
    }
    let mask = ack_mask(expected_workers);
    let mut spin = 0u32;
    loop {
        if ack.cutoff.load(Ordering::Acquire) != 0 {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        let done = ack.phase_done[phase as usize].load(Ordering::Acquire);
        if (done & !mask) != 0 {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        if (done & mask) == mask {
            return RtStatus::OK;
        }
        if spin >= spin_max_iters {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        spin = spin.wrapping_add(1);
        core::hint::spin_loop();
    }
}

/// [RT] master 有界等待 B1/B2/B3 全部 all-ack（期望 expected 位；禁用 0xF 硬编码）。
pub fn wait_phases(ack: &KhaenBlockAckV1, expected_workers: u8, spin_max_iters: u32) -> RtStatus {
    if !expected_valid(expected_workers) {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    if ack.expected_workers != expected_workers {
        return RtStatus::RT_E_PLAN_MISMATCH;
    }
    let mask = ack_mask(expected_workers);
    let mut spin = 0u32;
    loop {
        if ack.cutoff.load(Ordering::Acquire) != 0 {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        let mut all = true;
        let mut phase = 0usize;
        while phase < WORKER_PHASE_COUNT {
            let done = ack.phase_done[phase].load(Ordering::Acquire);
            if (done & !mask) != 0 {
                return RtStatus::RT_E_ABI_RUNTIME;
            }
            if (done & mask) != mask {
                all = false;
            }
            phase += 1;
        }
        if all {
            return RtStatus::OK;
        }
        if spin >= spin_max_iters {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        spin = spin.wrapping_add(1);
        core::hint::spin_loop();
    }
}

/// [RT] master 置 cutoff（整块 invalid；worker 必须丢弃输出/partial 结果）。
pub fn set_cutoff(ack: &KhaenBlockAckV1) -> RtStatus {
    if !expected_valid(ack.expected_workers) {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    ack.cutoff.store(1, Ordering::Release);
    RtStatus::OK
}

/// [RT] ack 是否 all-ack（expected 位到齐且 cutoff=0）。
pub fn ack_all(ack: &KhaenBlockAckV1) -> bool {
    if !expected_valid(ack.expected_workers) {
        return false;
    }
    let mask = ack_mask(ack.expected_workers);
    if ack.cutoff.load(Ordering::Acquire) != 0 {
        return false;
    }
    let mut phase = 0usize;
    while phase < WORKER_PHASE_COUNT {
        if (ack.phase_done[phase].load(Ordering::Acquire) & mask) != mask {
            return false;
        }
        phase += 1;
    }
    true
}

/// [RT] worker 记录迟到（本地 stats；不写 ack 槽）：late_blocks+1、flags=last_block_late。
pub fn worker_record_late(stats: &mut KhaenWorkerStatsV1) {
    stats.late_blocks = stats.late_blocks.wrapping_add(1);
    stats.flags |= WORKER_STAT_FLAG_LAST_BLOCK_LATE;
}

/// [RT] worker 记录 cutoff（本地 stats）：invalid_blocks+1、flags |= cutoff_seen。
pub fn worker_record_cutoff(stats: &mut KhaenWorkerStatsV1) {
    stats.invalid_blocks = stats.invalid_blocks.wrapping_add(1);
    stats.flags |= WORKER_STAT_FLAG_CUTOFF_SEEN;
}

/// [RT] worker 完成一个 block：blocks_done+1、清 last_block_late 位。
pub fn worker_record_block_done(stats: &mut KhaenWorkerStatsV1) {
    stats.blocks_done = stats.blocks_done.wrapping_add(1);
    stats.flags &= !WORKER_STAT_FLAG_LAST_BLOCK_LATE;
}

// ---------------------------------------------------------------------------
// ack 槽池（§7.3/§7.6：4 槽、生命周期、耗尽 -> SafetyMute、all-ack 前不复用）
// ---------------------------------------------------------------------------

/// [RT] 4 个 block ack 槽 + 每槽生命周期状态（master 独占写；worker 只读/写位图）。
///
/// 槽状态机：FREE -> INIT -> WAIT -> ALL_ACK -> RECYCLABLE -> FREE；
/// RECYCLABLE 只能由 nonRT（playback 读完/quarantine all-ack 后）置位，RT 禁止提前复位。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct WorkerAckSlotPool {
    /// 4 个 ack 槽（固定容量；槽被占用且未终态时禁止复用）。
    pub slots: [KhaenBlockAckV1; WORKER_ACK_SLOTS],
    state: [AtomicU32; WORKER_ACK_SLOTS],
}

impl WorkerAckSlotPool {
    /// 全 FREE 构造（admission 一次分配；无堆分配）。
    pub const fn new() -> Self {
        Self {
            slots: [const { KhaenBlockAckV1::new(0, 0, 0, 0) }; WORKER_ACK_SLOTS],
            state: [const { AtomicU32::new(WORKER_ACK_SLOT_FREE) }; WORKER_ACK_SLOTS],
        }
    }

    /// [RT] master 申请一个 ack 槽并写入标签；无 FREE 槽 -> RT_E_ACK_SLOT_EXHAUSTED。
    ///
    /// 返回值是槽号；成功后槽状态 INIT，master 写 block_start 后再 mark_published。
    pub fn try_acquire(
        &mut self,
        block_seq: u64,
        epoch_id: u64,
        plan_version: u32,
        expected_workers: u8,
        deadline_mono_ns: u64,
    ) -> Result<usize, RtStatus> {
        if !expected_valid(expected_workers) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut index = 0usize;
        while index < WORKER_ACK_SLOTS {
            let state = &self.state[index];
            if state.load(Ordering::Acquire) == WORKER_ACK_SLOT_FREE {
                let acquired = state.compare_exchange(
                    WORKER_ACK_SLOT_FREE,
                    WORKER_ACK_SLOT_INIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if acquired.is_ok() {
                    let slot = &mut self.slots[index];
                    slot.block_seq = block_seq;
                    slot.epoch_id = epoch_id;
                    slot.plan_version = plan_version;
                    slot.expected_workers = expected_workers;
                    slot.flags = WORKER_ACK_FLAG_VALID;
                    slot.deadline_mono_ns = deadline_mono_ns;
                    slot.compute_ticks = 0;
                    slot.reserved0 = 0;
                    slot.reserved1 = 0;
                    let mut phase = 0usize;
                    while phase < WORKER_PHASE_COUNT {
                        slot.phase_done[phase].store(0, Ordering::Release);
                        phase += 1;
                    }
                    slot.cutoff.store(0, Ordering::Release);
                    return Ok(index);
                }
            }
            index += 1;
        }
        Err(RT_E_ACK_SLOT_EXHAUSTED)
    }

    /// [RT] master 发布 block_start 后把槽置 WAIT。
    pub fn mark_published(&self, index: usize) -> RtStatus {
        match self.state.get(index) {
            Some(state) => match state.compare_exchange(
                WORKER_ACK_SLOT_INIT,
                WORKER_ACK_SLOT_WAIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => RtStatus::OK,
                Err(_) => RtStatus::RT_E_SLOT_STATE,
            },
            None => RtStatus::ERR_BAD_LAYOUT,
        }
    }

    /// [RT] master 在 all-ack 后把槽置 ALL_ACK（位图未齐/cutoff -> RT_E_SLOT_STATE）。
    pub fn mark_all_ack(&self, index: usize) -> RtStatus {
        let slot = match self.slots.get(index) {
            Some(value) => value,
            None => return RtStatus::ERR_BAD_LAYOUT,
        };
        if !ack_all(slot) {
            return RtStatus::RT_E_SLOT_STATE;
        }
        match self.state.get(index) {
            Some(state) => match state.compare_exchange(
                WORKER_ACK_SLOT_WAIT,
                WORKER_ACK_SLOT_ALL_ACK,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => RtStatus::OK,
                Err(_) => RtStatus::RT_E_SLOT_STATE,
            },
            None => RtStatus::ERR_BAD_LAYOUT,
        }
    }

    /// [NONRT] playback 读完 / quarantine all-ack 后：ALL_ACK -> RECYCLABLE。
    pub fn release_recyclable(&self, index: usize) -> RtStatus {
        match self.state.get(index) {
            Some(state) => match state.compare_exchange(
                WORKER_ACK_SLOT_ALL_ACK,
                WORKER_ACK_SLOT_RECYCLABLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => RtStatus::OK,
                Err(_) => RtStatus::RT_E_SLOT_STATE,
            },
            None => RtStatus::ERR_BAD_LAYOUT,
        }
    }

    /// [NONRT] 回收：RECYCLABLE -> FREE（RT 禁止调用）。
    pub fn recycle_free(&self, index: usize) -> RtStatus {
        match self.state.get(index) {
            Some(state) => match state.compare_exchange(
                WORKER_ACK_SLOT_RECYCLABLE,
                WORKER_ACK_SLOT_FREE,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => RtStatus::OK,
                Err(_) => RtStatus::RT_E_SLOT_STATE,
            },
            None => RtStatus::ERR_BAD_LAYOUT,
        }
    }

    /// 槽引用（worker 只读/写原子位图）。
    pub fn slot(&self, index: usize) -> Option<&KhaenBlockAckV1> {
        self.slots.get(index)
    }

    /// 槽状态（FREE/INIT/WAIT/ALL_ACK/RECYCLABLE）；越界返回 u32::MAX。
    pub fn state(&self, index: usize) -> u32 {
        match self.state.get(index) {
            Some(value) => value.load(Ordering::Acquire),
            None => u32::MAX,
        }
    }

    /// 非 FREE 槽数（耗尽判定/降级 drain 判定）。
    pub fn occupied(&self) -> u32 {
        let mut count = 0u32;
        let mut index = 0usize;
        while index < WORKER_ACK_SLOTS {
            if self.state[index].load(Ordering::Acquire) != WORKER_ACK_SLOT_FREE {
                count += 1;
            }
            index += 1;
        }
        count
    }
}

impl Default for WorkerAckSlotPool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// quarantine 槽模型（§7.6：迟到 worker all-ack 前禁止复用/释放；失联永不回收）
// ---------------------------------------------------------------------------

/// [RT] quarantine 槽池（固定 8 槽；迟到 worker all-ack 前不得复用/释放）。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct WorkerQuarantine {
    epoch_id: [AtomicU64; WORKER_QUARANTINE_SLOTS],
    block_seq: [AtomicU64; WORKER_QUARANTINE_SLOTS],
    plan_version: [AtomicU32; WORKER_QUARANTINE_SLOTS],
    expected_workers: [AtomicU32; WORKER_QUARANTINE_SLOTS],
    remaining: [AtomicU32; WORKER_QUARANTINE_SLOTS],
    state: [AtomicU32; WORKER_QUARANTINE_SLOTS],
}

impl WorkerQuarantine {
    /// 全 FREE 构造（admission 一次分配；无堆分配）。
    pub const fn new() -> Self {
        Self {
            epoch_id: [const { AtomicU64::new(0) }; WORKER_QUARANTINE_SLOTS],
            block_seq: [const { AtomicU64::new(0) }; WORKER_QUARANTINE_SLOTS],
            plan_version: [const { AtomicU32::new(0) }; WORKER_QUARANTINE_SLOTS],
            expected_workers: [const { AtomicU32::new(0) }; WORKER_QUARANTINE_SLOTS],
            remaining: [const { AtomicU32::new(0) }; WORKER_QUARANTINE_SLOTS],
            state: [const { AtomicU32::new(WORKER_QUARANTINE_FREE) }; WORKER_QUARANTINE_SLOTS],
        }
    }

    /// [RT] master 把 invalid 块放入 quarantine；池满 -> ERR_QUARANTINE_EXHAUSTED。
    pub fn enter(
        &self,
        epoch_id: u64,
        block_seq: u64,
        plan_version: u32,
        expected_workers: u8,
    ) -> Result<usize, RtStatus> {
        if !expected_valid(expected_workers) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut index = 0usize;
        while index < WORKER_QUARANTINE_SLOTS {
            let state = &self.state[index];
            if state.load(Ordering::Acquire) == WORKER_QUARANTINE_FREE {
                let acquired = state.compare_exchange(
                    WORKER_QUARANTINE_FREE,
                    WORKER_QUARANTINE_OCCUPIED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if acquired.is_ok() {
                    self.epoch_id[index].store(epoch_id, Ordering::Release);
                    self.block_seq[index].store(block_seq, Ordering::Release);
                    self.plan_version[index].store(plan_version, Ordering::Release);
                    self.expected_workers[index].store(expected_workers as u32, Ordering::Release);
                    self.remaining[index].store(ack_mask(expected_workers), Ordering::Release);
                    return Ok(index);
                }
            }
            index += 1;
        }
        Err(RtStatus::ERR_QUARANTINE_EXHAUSTED)
    }

    /// [RT]/[NONRT] 迟到 worker 确认"不再使用该块"（标签必须匹配；重复 ack 幂等）。
    pub fn note_ack(
        &self,
        index: usize,
        worker: WorkerId,
        epoch_id: u64,
        block_seq: u64,
        plan_version: u32,
    ) -> RtStatus {
        if index >= WORKER_QUARANTINE_SLOTS || worker >= WORKER_COUNT_MAX as u8 {
            return RtStatus::ERR_BAD_LAYOUT;
        }
        if self.state[index].load(Ordering::Acquire) != WORKER_QUARANTINE_OCCUPIED {
            return RtStatus::RT_E_SLOT_STATE;
        }
        if self.epoch_id[index].load(Ordering::Acquire) != epoch_id
            || self.block_seq[index].load(Ordering::Acquire) != block_seq
            || self.plan_version[index].load(Ordering::Acquire) != plan_version
        {
            return RtStatus::RT_E_PARTIAL_LATE;
        }
        let expected = self.expected_workers[index].load(Ordering::Acquire);
        if worker as u32 >= expected {
            return RtStatus::ERR_ADMISSION_SCHED;
        }
        self.remaining[index].fetch_and(!(1u32 << worker), Ordering::AcqRel);
        RtStatus::OK
    }

    /// [NONRT] all-ack 后由 control 释放槽（RT 禁止复用）；未 all-ack -> RT_E_SLOT_STATE。
    pub fn try_release(&self, index: usize) -> RtStatus {
        if index >= WORKER_QUARANTINE_SLOTS {
            return RtStatus::ERR_BAD_LAYOUT;
        }
        if self.state[index].load(Ordering::Acquire) != WORKER_QUARANTINE_OCCUPIED {
            return RtStatus::RT_E_SLOT_STATE;
        }
        if self.remaining[index].load(Ordering::Acquire) != 0 {
            return RtStatus::RT_E_SLOT_STATE;
        }
        match self.state[index].compare_exchange(
            WORKER_QUARANTINE_OCCUPIED,
            WORKER_QUARANTINE_FREE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => RtStatus::OK,
            Err(_) => RtStatus::RT_E_SLOT_STATE,
        }
    }

    /// 槽是否仍被占用（失联 worker 永不 ack -> 永不回收，INV-L5）。
    pub fn is_occupied(&self, index: usize) -> bool {
        match self.state.get(index) {
            Some(value) => value.load(Ordering::Acquire) == WORKER_QUARANTINE_OCCUPIED,
            None => false,
        }
    }

    /// 占用槽数（耗尽判定）。
    pub fn in_use(&self) -> u32 {
        let mut count = 0u32;
        let mut index = 0usize;
        while index < WORKER_QUARANTINE_SLOTS {
            if self.state[index].load(Ordering::Acquire) == WORKER_QUARANTINE_OCCUPIED {
                count += 1;
            }
            index += 1;
        }
        count
    }
}

impl Default for WorkerQuarantine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 2 核降级/重准入（core-v1 §7.4：独立 plan + 新 epoch，禁止旧 epoch 半图继续）
// ---------------------------------------------------------------------------

/// worker 集模式（2 核降级/重准入状态机；NONRT control 拥有）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerMode {
    /// 生产 4 worker 运行。
    FourCore,
    /// 已请求降级：停止新 block 提交，等全部 ack/quarantine 终态。
    Degrading,
    /// 2 worker 独立 plan + 新 epoch 运行（重新准入通过）。
    TwoCore,
    /// 重新准入失败：只允许安全 mute / last-good，不得以 2 worker 继续旧图。
    SafetyMute,
}

/// 2 核降级/重准入状态机（[NONRT] control 拥有；不进入 RT 调用图）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerReAdmission {
    /// 当前模式。
    pub mode: WorkerMode,
    /// 当前生效 plan（4 核或 2 核独立 plan）。
    pub plan: WorkerDagPlan,
    /// 当前 epoch_id（每次重准入 +1；epoch 永不复用）。
    pub epoch_id: u64,
}

impl WorkerReAdmission {
    /// 由初始 4 worker plan 构造（plan 必须自校验通过）。
    pub fn new(plan: WorkerDagPlan, epoch_id: u64) -> Result<Self, RtStatus> {
        let status = plan.validate();
        if !status.is_ok() {
            return Err(status);
        }
        Ok(Self {
            mode: WorkerMode::FourCore,
            plan,
            epoch_id,
        })
    }

    /// 当前 plan 的 worker 数。
    pub const fn expected_workers(&self) -> u8 {
        self.plan.expected_workers
    }

    /// [NONRT] 请求降级：置 switch_state=ABORT（master 不得再启动新 block），进入 Draining。
    pub fn request_degrade(&mut self, ctl: &KhaenRtControlV1) -> RtStatus {
        if self.mode != WorkerMode::FourCore {
            return RtStatus::RT_E_SLOT_STATE;
        }
        let status = control_valid(ctl);
        if !status.is_ok() {
            return status;
        }
        ctl.switch_state
            .store(WORKER_SWITCH_ABORT, Ordering::Release);
        self.mode = WorkerMode::Degrading;
        RtStatus::OK
    }

    /// 旧 epoch 是否已 drain 完（所有 ack 槽终态；ack 槽耗尽/未 ack 不能重准入）。
    pub fn drain_complete(&self, pool: &WorkerAckSlotPool) -> bool {
        if self.mode != WorkerMode::Degrading {
            return false;
        }
        pool.occupied() == 0
    }

    /// [NONRT] 重新准入 2 核独立 plan：必须已 Draining 且 ack 槽全终态；
    /// 成功返回新 epoch_id，失败进入 SafetyMute + 置 safety_request（绝不返回旧图继续）。
    ///
    /// 成功路径在内部完成版本提交（T09-review I01）：先写 two_worker flag、再写
    /// current_plan_version、最后清 switch_state=IDLE；master 只在 switch_state!=ABORT 时
    /// 启动 block，因此旧 4 核 plan（旧 version/expected=4）不存在短暂可接受窗口。
    /// current_plan_ptr（plan arena 基址）由 host/T26 持有并发布，本 leaf 不拥有 arena，
    /// 不伪造指针；T26 必须在启动播放前把 ptr 指向该 2 核 plan arena。
    /// 失败语义（I02）：可重试的配置/状态错误（未降级、drain 未完成、switch_state 非 ABORT）
    /// 返回 ERR_SWITCH_BUSY 且不置 safety_request；plan 构建/epoch 溢出等终态失败进入
    /// SafetyMute 并置 safety_request=1、保持 switch_state=ABORT。
    pub fn re_admit(
        &mut self,
        topology: &DirectTopology,
        block_size: u16,
        pool: &WorkerAckSlotPool,
        ctl: &KhaenRtControlV1,
    ) -> Result<u64, RtStatus> {
        if self.mode != WorkerMode::Degrading {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if !self.drain_complete(pool) {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if ctl.switch_state.load(Ordering::Acquire) != WORKER_SWITCH_ABORT {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        let next_version = self.plan.plan_version.wrapping_add(1);
        let plan = match WorkerDagPlan::build(
            topology,
            block_size,
            WORKER_COUNT_FALLBACK as u8,
            next_version,
        ) {
            Ok(value) => value,
            Err(err) => {
                self.mode = WorkerMode::SafetyMute;
                ctl.switch_state
                    .store(WORKER_SWITCH_ABORT, Ordering::Release);
                ctl.safety_request.store(1, Ordering::Release);
                return Err(err);
            }
        };
        let next_epoch = match self.epoch_id.checked_add(1) {
            Some(value) => value,
            None => {
                self.mode = WorkerMode::SafetyMute;
                ctl.switch_state
                    .store(WORKER_SWITCH_ABORT, Ordering::Release);
                ctl.safety_request.store(1, Ordering::Release);
                return Err(RtStatus::RT_E_INVARIANT);
            }
        };
        self.plan = plan;
        self.epoch_id = next_epoch;
        self.mode = WorkerMode::TwoCore;
        // I01 版本提交顺序：two_worker flag -> current_plan_version -> switch_state=IDLE。
        // switch_state 是 master 启动 block 的唯一放行位，最后清 ABORT 可保证旧 4 核 plan
        // 不会在新 version 发布前被 master 接受（无半图/旧图窗口）。
        let flags = ctl.flags.load(Ordering::Acquire);
        ctl.flags
            .store(flags | WORKER_CONTROL_FLAG_TWO_WORKER, Ordering::Release);
        ctl.current_plan_version
            .store(next_version, Ordering::Release);
        ctl.switch_state.store(0, Ordering::Release);
        // current_plan_ptr 不在本 leaf 发布（host/T26 持有 arena；见本函数文档）。
        Ok(next_epoch)
    }

    /// [NONRT] 重准入失败/安全兜底：只允许 safety mute，禁止继续旧图。
    pub fn to_safety_mute(&mut self, ctl: &KhaenRtControlV1) -> RtStatus {
        let status = control_valid(ctl);
        if !status.is_ok() {
            return status;
        }
        ctl.switch_state
            .store(WORKER_SWITCH_ABORT, Ordering::Release);
        ctl.safety_request.store(1, Ordering::Release);
        self.mode = WorkerMode::SafetyMute;
        RtStatus::OK
    }

    /// 当前是否允许启动新 block（2 核 profile 必须已重新准入；旧 epoch 不得继续）。
    pub fn accepts_new_blocks(&self, ctl: &KhaenRtControlV1) -> bool {
        let allowed_mode = self.mode == WorkerMode::FourCore || self.mode == WorkerMode::TwoCore;
        if !allowed_mode {
            return false;
        }
        ctl.switch_state.load(Ordering::Acquire) != WORKER_SWITCH_ABORT
    }
}

// ---------------------------------------------------------------------------
// master / worker block 步进（[RT]；T26 用其组合线程循环）
// ---------------------------------------------------------------------------

/// [RT] master 启动一个 block：校验 plan/profile/epoch，申请 ack 槽，发布 block_start。
///
/// 失败语义：switch_state=ABORT（降级中）-> ERR_SWITCH_BUSY（不得继续旧图）；
/// ack 槽耗尽 -> RT_E_ACK_SLOT_EXHAUSTED + 置 safety_request（§7.3/§7.6）。
pub fn master_start_block(
    ctl: &KhaenRtControlV1,
    pool: &mut WorkerAckSlotPool,
    plan: &WorkerDagPlan,
    desc: &KhaenBlockDescV1,
) -> Result<u32, RtStatus> {
    let status = control_valid(ctl);
    if !status.is_ok() {
        return Err(status);
    }
    let status = plan.validate();
    if !status.is_ok() {
        return Err(status);
    }
    if ctl.switch_state.load(Ordering::Acquire) == WORKER_SWITCH_ABORT {
        return Err(RtStatus::ERR_SWITCH_BUSY);
    }
    if desc.abi_version != ABI_VERSION || desc.block_size != plan.block_size {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if desc.valid_frames > desc.block_size as u32 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if desc.plan_version != plan.plan_version {
        return Err(RtStatus::RT_E_PLAN_MISMATCH);
    }
    if ctl.current_plan_version.load(Ordering::Acquire) != plan.plan_version {
        return Err(RtStatus::ERR_PLAN_STALE);
    }
    let flags = ctl.flags.load(Ordering::Acquire);
    let two = (flags & WORKER_CONTROL_FLAG_TWO_WORKER) != 0;
    let expected = if two {
        WORKER_COUNT_FALLBACK as u8
    } else {
        WORKER_COUNT_MAX as u8
    };
    if plan.expected_workers != expected {
        return Err(RtStatus::ERR_PLAN_STALE);
    }
    let slot = match pool.try_acquire(
        desc.block_seq,
        desc.epoch_id,
        plan.plan_version,
        plan.expected_workers,
        desc.play_deadline_ns,
    ) {
        Ok(value) => value,
        Err(err) => {
            ctl.safety_request.store(1, Ordering::Release);
            return Err(err);
        }
    };
    let status = publish_block_start(
        ctl,
        desc.epoch_id,
        desc.frame_index,
        plan.plan_version,
        slot as u32,
    );
    if !status.is_ok() {
        let _ = pool.recycle_free(slot);
        return Err(status);
    }
    let status = pool.mark_published(slot);
    if !status.is_ok() {
        return Err(status);
    }
    Ok(slot as u32)
}

/// [RT] worker 等待并校验自己的下一个 block 启动身份（序号必须 +1；plan/epoch 一致）。
pub fn worker_begin_block(
    ctl: &KhaenRtControlV1,
    plan: &WorkerDagPlan,
    last_seq: u64,
    spin_max_iters: u32,
) -> Result<(u64, WorkerBlockStart), RtStatus> {
    let status = plan.validate();
    if !status.is_ok() {
        return Err(status);
    }
    let (seq, start) = consume_block_start(ctl, last_seq, spin_max_iters)?;
    if start.plan_version != plan.plan_version {
        return Err(RtStatus::RT_E_PLAN_MISMATCH);
    }
    let flags = ctl.flags.load(Ordering::Acquire);
    let two = (flags & WORKER_CONTROL_FLAG_TWO_WORKER) != 0;
    let expected = if two {
        WORKER_COUNT_FALLBACK as u8
    } else {
        WORKER_COUNT_MAX as u8
    };
    if expected != plan.expected_workers {
        return Err(RtStatus::RT_E_PLAN_MISMATCH);
    }
    Ok((seq, start))
}

/// [RT] worker 在 phase 边界的有界等待（= wait_phase_done 的语义别名）。
pub fn worker_phase_barrier(
    ack: &KhaenBlockAckV1,
    phase: u8,
    expected_workers: u8,
    spin_max_iters: u32,
) -> RtStatus {
    wait_phase_done(ack, phase, expected_workers, spin_max_iters)
}

// ---------------------------------------------------------------------------
// 确定性四 worker 调度驱动（[TEST]/[NONRT]；单线程按固定 worker 顺序执行同一批
// [RT] phase 函数，用于 W=1/2/4 逐位一致与 DoD 对拍；真实线程循环由 T26 组合）
// ---------------------------------------------------------------------------

/// 一次 block 调度的统计（[TEST]/[NONRT]；不进入 RT 路径）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerScheduleStats {
    /// 每 phase 的 tile 数。
    pub phase_tasks: [u32; WORKER_PHASE_COUNT],
    /// 每 phase 每 worker 执行的 tile 数。
    pub worker_phase_tasks: [[u32; WORKER_COUNT_MAX]; WORKER_PHASE_COUNT],
    /// 每 worker 全部 phase 执行的 tile 数。
    pub worker_tasks_total: [u32; WORKER_COUNT_MAX],
    /// 每 phase 串行比例（Q16；phase tile 数 > W 的部分）。
    pub serial_fraction_q16: [u32; WORKER_PHASE_COUNT],
    /// 每 phase 实际并行 worker 数。
    pub parallel_workers: [u32; WORKER_PHASE_COUNT],
    /// 实际执行的 tile 总数（应 = plan.total_tasks）。
    pub executed_tiles: u32,
    /// invalid block 数（cutoff/迟到/有限性失败）。
    pub invalid_blocks: u32,
    /// cutoff 导致的 invalid 数。
    pub cutoff_blocks: u32,
    /// 迟到（phase barrier 未在界内 all-ack）计数。
    pub late_workers: u32,
}

fn zero_output(
    output: &mut [f32],
    out_stride: usize,
    containers: usize,
    block_size: usize,
) -> RtStatus {
    if out_stride < block_size {
        return RtStatus::ERR_STRIDE_ALIGN;
    }
    let need = match containers.checked_mul(out_stride) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    if output.len() < need {
        return RtStatus::ERR_CAPACITY;
    }
    let mut channel = 0usize;
    while channel < containers {
        let base = channel * out_stride;
        let mut n = 0usize;
        while n < block_size {
            output[base + n] = 0.0;
            n += 1;
        }
        channel += 1;
    }
    RtStatus::OK
}

/// [TEST]/[NONRT] 按静态 DAG 执行一个 block（固定 worker 顺序、同一批 RT phase 函数）。
///
/// 该驱动用于（a）W=1/2/4 逐位一致回归；（b）与 T07 direct 的 f32 对拍；
/// （c）ack/cutoff/epoch 标签的模型测试。它不是生产 RT 调度器：真实线程循环由 T26
/// 用 worker_begin_block/worker_phase_b*/ack_phase/worker_phase_barrier 组合。
///
/// 失败语义：任何 phase 失败/cutoff/late/输出非有限 -> 整块 invalid，输出被清零
/// （绝不发布半图），返回固定错误码。
pub fn execute_block_scheduled(
    plan: &WorkerDagPlan,
    coeffs: &DirectCoeffsF32<'_>,
    state: &mut [f32],
    input: &[f32],
    in_stride: usize,
    output: &mut [f32],
    out_stride: usize,
    epoch_id: u64,
    block_seq: u64,
    valid_frames: usize,
    ack: &KhaenBlockAckV1,
    partials: &mut WorkerPartials,
    bad_output: &AtomicU32,
    spin_max_iters: u32,
    stats: &mut WorkerScheduleStats,
) -> RtStatus {
    *stats = WorkerScheduleStats::default();
    let status = phase_validate_core(plan, coeffs, state.len(), valid_frames, 0);
    if !status.is_ok() {
        return status;
    }
    if valid_frames > plan.block_size as usize {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    if !expected_valid(plan.expected_workers) {
        return RtStatus::ERR_ADMISSION_SCHED;
    }
    if ack.expected_workers != plan.expected_workers {
        return RtStatus::RT_E_PLAN_MISMATCH;
    }
    if ack.epoch_id != epoch_id || ack.block_seq != block_seq {
        return RtStatus::RT_E_EPOCH_MISMATCH;
    }
    if ack.plan_version != plan.plan_version {
        return RtStatus::RT_E_PLAN_MISMATCH;
    }
    if (ack.flags & WORKER_ACK_FLAG_VALID) == 0 || (ack.flags & !WORKER_ACK_FLAG_MASK) != 0 {
        return RtStatus::RT_E_ABI_RUNTIME;
    }
    let topo = &plan.topology;
    let containers = topo.container_channels as usize;
    let block_size = plan.block_size as usize;
    if in_stride < block_size {
        return RtStatus::ERR_STRIDE_ALIGN;
    }
    let need_in = match (topo.n_src as usize).checked_mul(in_stride) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    if input.len() < need_in {
        return RtStatus::ERR_CAPACITY;
    }
    let status = zero_output(output, out_stride, containers, block_size);
    if !status.is_ok() {
        return status;
    }
    partials.zero();
    bad_output.store(0, Ordering::Release);
    if ack.cutoff.load(Ordering::Acquire) != 0 {
        stats.cutoff_blocks = 1;
        stats.invalid_blocks = 1;
        return RtStatus::RT_E_CUTOFF;
    }
    if block_size == 0 {
        return RtStatus::ERR_BLOCK_SIZE;
    }
    // valid_frames < block_size 只允许作为 DRAIN_TAIL（与 direct.rs 一致：只处理前
    // valid_frames 个样本，[valid, B) 输出 +0.0 且历史不推进零填充）。
    let mut phase = 0u8;
    while phase < WORKER_PHASE_COUNT as u8 {
        let tasks = plan.phase_task_count(phase) as u32;
        stats.phase_tasks[phase as usize] = tasks;
        stats.serial_fraction_q16[phase as usize] = plan.serial_fraction_q16(phase);
        let parallel = plan.phase_parallel_workers(phase) as u32;
        stats.parallel_workers[phase as usize] = parallel;
        let mut worker = 0u8;
        while worker < plan.expected_workers {
            if ack.cutoff.load(Ordering::Acquire) != 0 {
                stats.cutoff_blocks = 1;
                stats.invalid_blocks = 1;
                let status = zero_output(output, out_stride, containers, block_size);
                if !status.is_ok() {
                    return status;
                }
                return RtStatus::RT_E_CUTOFF;
            }
            let phase_status = if phase == 0 {
                worker_phase_b1(plan, worker, coeffs, state, input, in_stride, valid_frames)
            } else if phase == 1 {
                worker_phase_b2(plan, worker, coeffs, state, partials, valid_frames)
            } else {
                worker_phase_b3(
                    plan,
                    worker,
                    coeffs,
                    state,
                    partials,
                    output,
                    out_stride,
                    valid_frames,
                    bad_output,
                )
            };
            if !phase_status.is_ok() {
                stats.invalid_blocks = 1;
                let status = zero_output(output, out_stride, containers, block_size);
                if !status.is_ok() {
                    return status;
                }
                return phase_status;
            }
            let ack_status = ack_phase(ack, worker, phase);
            if !ack_status.is_ok() {
                stats.invalid_blocks = 1;
                let status = zero_output(output, out_stride, containers, block_size);
                if !status.is_ok() {
                    return status;
                }
                return ack_status;
            }
            let worker_tasks = plan.worker_task_count(phase, worker) as u32;
            stats.worker_phase_tasks[phase as usize][worker as usize] = worker_tasks;
            stats.worker_tasks_total[worker as usize] =
                stats.worker_tasks_total[worker as usize] + worker_tasks;
            stats.executed_tiles = stats.executed_tiles + worker_tasks;
            worker += 1;
        }
        let barrier = wait_phase_done(ack, phase, plan.expected_workers, spin_max_iters);
        if !barrier.is_ok() {
            stats.late_workers = stats.late_workers + 1;
            stats.invalid_blocks = 1;
            let status = zero_output(output, out_stride, containers, block_size);
            if !status.is_ok() {
                return status;
            }
            return barrier;
        }
        phase += 1;
    }
    if bad_output.load(Ordering::Acquire) != 0 {
        stats.invalid_blocks = 1;
        let status = zero_output(output, out_stride, containers, block_size);
        if !status.is_ok() {
            return status;
        }
        return RtStatus::RT_E_OUTPUT_NONFINITE;
    }
    if ack.cutoff.load(Ordering::Acquire) != 0 {
        stats.cutoff_blocks = 1;
        stats.invalid_blocks = 1;
        let status = zero_output(output, out_stride, containers, block_size);
        if !status.is_ok() {
            return status;
        }
        return RtStatus::RT_E_CUTOFF;
    }
    // 每个 tile 必须恰被执行一次（固定 owner、无 stealing、无遗漏）。
    let expected_tiles = plan.total_tasks() as u32;
    if stats.executed_tiles != expected_tiles {
        return RtStatus::RT_E_INVARIANT;
    }
    RtStatus::OK
}

// ---------------------------------------------------------------------------
// 编译期静态断言（core-v1 §3 / §11.13 / §13.2；引用 not 运行期代码）
// ---------------------------------------------------------------------------

const _: () = {
    // 容量/常量与 core-v1 冻结值一致。
    assert!(WORKER_COUNT_MAX == 4);
    assert!(WORKER_COUNT_FALLBACK == 2);
    assert!(WORKER_PHASE_COUNT == 3);
    assert!(WORKER_REDUCE_LANES == 4);
    assert!(WORKER_ACK_SLOTS == 4);
    assert!(WORKER_QUARANTINE_SLOTS == 8);
    assert!(WORKER_BLOCK_MAX == 128);
    assert!(WORKER_ROLES_MAX == 22);
    assert!(WORKER_PARTIAL_ROLE_STRIDE == 128);
    assert!(WORKER_PARTIAL_LANE_STRIDE == 22 * 128);
    assert!(WORKER_PARTIAL_SAMPLES == 4 * 22 * 128);
    assert!(WORKER_PARTIAL_SAMPLES == 11_264);
    // lane 间/角色间 partial 缓冲必须是 64 B（cache line）的整数倍。
    assert!((WORKER_PARTIAL_ROLE_STRIDE * core::mem::size_of::<f32>()) % 64 == 0);
    assert!((WORKER_PARTIAL_LANE_STRIDE * core::mem::size_of::<f32>()) % 64 == 0);
    assert!((WORKER_PARTIAL_SAMPLES * core::mem::size_of::<f32>()) % 64 == 0);
    // ACK_MASK(expected) 参数化（禁止硬写 0xF）。
    assert!(ack_mask(1) == 0x1);
    assert!(ack_mask(2) == 0x3);
    assert!(ack_mask(4) == 0xF);
    assert!(ack_mask(4) == crate::rt::ACK_MASK_ALL);
    // ack 槽/ quarantine 状态值互不重叠。
    assert!(WORKER_ACK_SLOT_FREE == 0 && WORKER_ACK_SLOT_INIT == 1);
    assert!(WORKER_ACK_SLOT_WAIT == 2 && WORKER_ACK_SLOT_ALL_ACK == 3);
    assert!(WORKER_ACK_SLOT_RECYCLABLE == 4 && WORKER_ACK_SLOT_STATE_MAX == 5);
    assert!(WORKER_QUARANTINE_FREE == 0 && WORKER_QUARANTINE_OCCUPIED == 1);
};

const _: () = {
    // §11.13 KhaenRtControlV1 布局（128 B / align 64；待 integrator 提升到 core::abi）。
    assert!(core::mem::size_of::<KhaenRtControlV1>() == 128);
    assert!(core::mem::align_of::<KhaenRtControlV1>() == 64);
    assert!(core::mem::offset_of!(KhaenRtControlV1, magic) == 0);
    assert!(core::mem::offset_of!(KhaenRtControlV1, abi_version) == 4);
    assert!(core::mem::offset_of!(KhaenRtControlV1, size) == 6);
    assert!(core::mem::offset_of!(KhaenRtControlV1, block_start_seq) == 8);
    assert!(core::mem::offset_of!(KhaenRtControlV1, block_start_epoch) == 16);
    assert!(core::mem::offset_of!(KhaenRtControlV1, block_start_frame) == 24);
    assert!(core::mem::offset_of!(KhaenRtControlV1, block_start_plan_version) == 32);
    assert!(core::mem::offset_of!(KhaenRtControlV1, block_start_slot) == 36);
    assert!(core::mem::offset_of!(KhaenRtControlV1, current_plan_ptr) == 40);
    assert!(core::mem::offset_of!(KhaenRtControlV1, current_plan_version) == 48);
    assert!(core::mem::offset_of!(KhaenRtControlV1, pending_plan_version) == 52);
    assert!(core::mem::offset_of!(KhaenRtControlV1, pending_plan_ptr) == 56);
    assert!(core::mem::offset_of!(KhaenRtControlV1, switch_state) == 64);
    assert!(core::mem::offset_of!(KhaenRtControlV1, safety_request) == 68);
    assert!(core::mem::offset_of!(KhaenRtControlV1, safety_reason) == 72);
    assert!(core::mem::offset_of!(KhaenRtControlV1, flags) == 76);
    assert!(core::mem::offset_of!(KhaenRtControlV1, heartbeat_seq) == 80);
    assert!(core::mem::offset_of!(KhaenRtControlV1, spin_max_iters) == 88);
    assert!(core::mem::offset_of!(KhaenRtControlV1, cutoff_drain_iters) == 92);
    assert!(core::mem::offset_of!(KhaenRtControlV1, tick_freq_hz) == 96);
    assert!(core::mem::offset_of!(KhaenRtControlV1, tick_read_budget) == 100);
    assert!(core::mem::offset_of!(KhaenRtControlV1, capture_ring_offset) == 104);
    assert!(core::mem::offset_of!(KhaenRtControlV1, playback_ring_offset) == 112);
    assert!(core::mem::offset_of!(KhaenRtControlV1, reserved) == 120);
};

const _: () = {
    // §13.2 无 RT 析构：本文件 RT 可见类型全部 needs_drop == false。
    assert!(!core::mem::needs_drop::<KhaenRtControlV1>());
    assert!(!core::mem::needs_drop::<WorkerBlockStart>());
    assert!(!core::mem::needs_drop::<WorkerDagPlan>());
    assert!(!core::mem::needs_drop::<WorkerPartials>());
    assert!(!core::mem::needs_drop::<WorkerAckSlotPool>());
    assert!(!core::mem::needs_drop::<WorkerQuarantine>());
    assert!(!core::mem::needs_drop::<WorkerMode>());
    assert!(!core::mem::needs_drop::<WorkerReAdmission>());
    assert!(!core::mem::needs_drop::<WorkerScheduleStats>());
    assert!(!core::mem::needs_drop::<RtStatus>());
    // partial 缓冲按 64 B 对齐，避免相邻 worker 写同一 cache line（F-34）。
    assert!(core::mem::align_of::<WorkerPartials>() == 64);
    assert!(core::mem::size_of::<WorkerPartials>() == WORKER_PARTIAL_SAMPLES * 4);
    assert!(core::mem::align_of::<WorkerAckSlotPool>() == 64);
    assert!(core::mem::align_of::<WorkerQuarantine>() == 64);
    assert!(core::mem::size_of::<WorkerBlockStart>() == 24);
};
