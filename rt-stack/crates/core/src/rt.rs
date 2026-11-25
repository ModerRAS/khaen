//! core RT 类型、错误码与 v1 契约常量（core-v1 §3 / §9 / §12.3 / §13）。
//!
//! T06 scaffold：本模块固定常量、RtStatus 错误码、RT 可见类型与 [RT] 函数签名 stub。
//! T07/T08/T09/T12/T23/T56 实现具体 kernel/plan；任何公共类型/注册变更回到 integrator。
//!
//! RT-MODULE: DSP 纯 worker/master 路径禁止析构/分配/锁/syscall/日志/panic；
//! 见 core-v1 §10（F-01..F-44）与 §13（R-DROP-01..14、D-01..D-20）。
//! 本文件当前只有类型与 stub，没有运行时代码；lint 入口见 crates/rt-check。

use core::sync::atomic::{AtomicU32, AtomicU64};

use crate::abi::{
    TspiBlockAckV1, TspiBlockDescV1, TspiDiagFirV1, TspiPathV1, TspiPlanHeaderV1, TspiRoleV1,
    TspiWorkerStatsV1,
};

pub use crate::abi::{ABI_VERSION, TSPI_PLAN_MAGIC};

/// DSP worker 编号：0..=3（master = worker0）。
pub type WorkerId = u8;
/// 运行期 plan 提交版本（u32 单调）。
pub type PlanVersion = u32;
/// 输入时间轴绝对帧号。
pub type FrameIndex = u64;
/// 预算内 tick 读数（仅 master 每 block <= 3 次；正常路径 2 次，见 `TICK_READS_PER_BLOCK_MAX`）。
pub type Tick = u64;

// ---------------------------------------------------------------------------
// v1 契约常量（core-v1 §3；全部为编译期上界，不是"任何组合都保证 RT"）
// ---------------------------------------------------------------------------

/// MOTU UltraLite-mk5 capture 上限。
pub const MAX_INPUT_CHANNELS: usize = 20;
/// MOTU playback 上限。
pub const MAX_OUTPUT_CHANNELS: usize = 22;
/// 输入布局 role 数上限。
pub const MAX_INPUT_ROLES: usize = 16;
/// 输出容器/布局 role 数上限。
pub const MAX_OUTPUT_ROLES: usize = 16;
/// 低带 MIMO 显式 participants 上限（P1 = 5）。
pub const MAX_LOW_BAND_PARTICIPANTS: usize = 8;
/// 矩阵路径表容量。
pub const MAX_MATRIX_PATHS: usize = 128;
/// 每输出对角 FIR 上限（= MAX_OUTPUT_CHANNELS）。
pub const MAX_DIAG_FIRS: usize = 22;
/// 图节点数上限。
pub const MAX_PLAN_NODES: usize = 256;

/// v1 部署采样率（44.1/88.2 不承诺）。
pub const SAMPLE_RATES_HZ: [u32; 2] = [48_000, 96_000];
/// v1 运行准入块长（生产目标 128，最终独立窗口 64）。
pub const BLOCK_SIZES: [u16; 2] = [64, 128];
/// declared load 默认块长。
pub const BLOCK_DEFAULT: u16 = 128;

/// gate_ns = T_block / 2 的分子。
pub const GATE_NUM: u64 = 1;
/// gate_ns = T_block / 2 的分母。
pub const GATE_DEN: u64 = 2;
/// 128@48k 的 T_block（向上取整 ns）。
pub const TBLOCK_128_48K_NS: u64 = 2_666_667;
/// 64@48k 的 T_block（向上取整 ns）。
pub const TBLOCK_64_48K_NS: u64 = 1_333_333;
/// hard_cutoff 前留给 playback/I/O 尾部的默认安全量（100 us）。
pub const IO_TAIL_ALLOWANCE_NS_DEFAULT: u64 = 100_000;
/// 非 RT watchdog 采样周期。
pub const WATCHDOG_PERIOD_MS: u32 = 10;
/// 连续未见 heartbeat 的 block 数 -> 安全 mute 请求。
pub const WATCHDOG_STALE_BLOCKS: u32 = 4;

/// 常规 profile 每 route 有效支撑上限。
pub const TAPS_NORMAL_MAX: u32 = 4096;
/// extended profile 显式上限。
pub const TAPS_EXTENDED_MAX: u32 = 16_384;
/// 超过此值仅设计工具 offline 输出，runtime 准入拒绝。
pub const TAPS_DESIGN_ONLY_ABOVE: u32 = 16_384;

/// 每路径分区数上限。侧路 24000 点、低速率块 16 点时 Q = 1500。
/// 计划路径 16384 点、块长 64 仍是 Q = 256。直通抽头上限仍是 4096。
pub const MAX_PARTITIONS: u32 = 1500;
/// 生产 4 worker。
pub const RT_WORKERS: u8 = 4;
/// 2 核是独立 profile 重新准入（不是失核后继续半图）。
pub const RT_WORKERS_FALLBACK: u8 = 2;
/// 仅 benchmark 比较的 worker 数；1/2 不授予生产资格。
pub const RT_WORKERS_BENCH: [u8; 3] = [1, 2, 4];
/// B1/B2/B3 三个 worker phase。
pub const PARALLEL_PHASES: u8 = 3;

/// capture -> DSP 环固定槽数。
pub const SPSC_CAPTURE_SLOTS: usize = 4;
/// DSP -> playback 环固定槽数。
pub const SPSC_PLAYBACK_SLOTS: usize = 4;
/// 隔离池固定槽数。
pub const QUARANTINE_SLOTS: usize = 8;
/// 事件环固定槽数（8 lane x 128 槽）。
pub const EVENT_RING_SLOTS: usize = 1024;
/// 事件 LANE 数。
pub const EVENT_LANES: usize = 8;
/// 每 lane 固定槽数。
pub const EVENT_LANE_SLOTS: usize = 128;
/// 每 worker 每 metric 固定直方图桶数。
pub const HIST_BUCKETS: usize = 32;
/// 每 RT 线程栈上限（512 KiB）。
pub const MAX_WORKER_STACK_BYTES: usize = 524_288;

/// worker 位图位数。
pub const WORKER_BITS: u8 = 4;
/// all-ack 位图（4 worker 全完成）。
pub const ACK_MASK_ALL: u32 = 0xF;
/// graph_id SHA-256 原始字节数。
pub const GRAPH_ID_BYTES: usize = 32;
/// 每 block 预算内 tick 采样次数上限（仅 master；纯 worker 0 次）。
///
/// core-v1 §3.4/§6.4/§7.3 I04（v1.1 FIXED）：上限 = **3** ——
/// B1 进入 1 次（t_entry）；**仅当 spin 预算耗尽**做 hard_cutoff 判定时可选 1 次（t_cut，正常不读）；
/// B3 退出 1 次（t_exit）。正常路径只读 2 次；`compute_ticks = t_exit - t_entry`。
/// T06 scaffold 曾误写 2（T105 按 spec 对齐）。
pub const TICK_READS_PER_BLOCK_MAX: u32 = 3;

/// plan 切换状态：IDLE（无待生效 plan）。
pub const SWITCH_IDLE: u32 = 0;
/// plan 切换状态：ARMED（pending plan 已发布，等待 block 边界提交）。
pub const SWITCH_ARMED: u32 = 1;

// ---------------------------------------------------------------------------
// 时限/长度公式（core-v1 §3.5、§3.3）
// ---------------------------------------------------------------------------

/// T_block = B / fs，向上取整 ns（不得因取整放宽门）。
pub const fn t_block_ns_ceil(sample_rate_hz: u32, block_size: u16) -> u64 {
    let num = (block_size as u64) * 1_000_000_000;
    let fs = sample_rate_hz as u64;
    (num + fs - 1) / fs
}

/// gate_ns = T_block / 2，向下取整 ns（core-v1 §3.5/附录 B.1）。
pub const fn gate_ns_floor(sample_rate_hz: u32, block_size: u16) -> u64 {
    (block_size as u64) * 1_000_000_000 / (GATE_DEN * sample_rate_hz as u64)
}

/// 共享 FFT 长度 K = 2 x B。
pub const fn fft_size(block_size: u16) -> u32 {
    2 * block_size as u32
}

/// 每路径分区数 Q = ceil(taps / B)。
pub const fn max_partitions(taps: u32, block_size: u16) -> u32 {
    let b = block_size as u32;
    if b == 0 {
        return 0;
    }
    (taps + b - 1) / b
}

/// 融合长度 L_F = L_G + L_P - 1（D 计入支撑）。
pub const fn fused_len_max(l_g: u32, l_p: u32) -> u32 {
    l_g + l_p - 1
}

// ---------------------------------------------------------------------------
// RtStatus：固定错误码（core-v1 §9.2；禁止 Result<_, String> 跨 RT 边界）
// ---------------------------------------------------------------------------

/// RT/准入错误码（repr(transparent) u32，Copy，无 Drop）。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RtStatus(pub u32);

impl RtStatus {
    /// 0 = OK。
    pub const OK: RtStatus = RtStatus(0);

    /// 是否 OK。
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }

    /// 低 16 位错误码。
    pub const fn code(self) -> u32 {
        self.0 & 0xFFFF
    }

    /// 由 code 构造（scaffold/测试用；生产只用常量）。
    pub const fn from_code(code: u32) -> Self {
        Self(code)
    }

    // 101-199：配置/ABI/准入（非 RT 返回，拒绝 plan/启动）
    /// 结构版本不匹配。
    pub const ERR_ABI_VERSION: Self = Self(101);
    /// 魔数不匹配。
    pub const ERR_MAGIC: Self = Self(102);
    /// 字段/大小/偏移不符。
    pub const ERR_BAD_LAYOUT: Self = Self(103);
    /// fs 不在 48k/96k。
    pub const ERR_RATE: Self = Self(104);
    /// B 不在 64/128。
    pub const ERR_BLOCK_SIZE: Self = Self(105);
    /// N/O 越界或与设备不符。
    pub const ERR_CHANNELS: Self = Self(106);
    /// role/output 映射/宽度不符。
    pub const ERR_ROLES: Self = Self(107);
    /// runtime 只收 f32 plan。
    pub const ERR_PRECISION: Self = Self(108);
    /// stride 非 16 样本倍数或 channel 基址非 64B 对齐。
    pub const ERR_STRIDE_ALIGN: Self = Self(109);
    /// > 16384 或 extended 未准入。
    pub const ERR_TAPS: Self = Self(110);
    /// 路径重复/越界/长度不符/非零尾缺失。
    pub const ERR_PATH_TABLE: Self = Self(111);
    /// graph_id 与内容不符。
    pub const ERR_HASH: Self = Self(112);
    /// arena/双图/quarantine 容量不足。
    pub const ERR_CAPACITY: Self = Self(113);
    /// 不启 RT 主模式；last-good/mute。
    pub const ERR_ADMISSION_MEMORY: Self = Self(114);
    /// 不启 RT 主模式（调度/拓扑）。
    pub const ERR_ADMISSION_SCHED: Self = Self(115);
    /// 不启 RT 主模式（时钟源）。
    pub const ERR_ADMISSION_CLOCK: Self = Self(116);
    /// owner 冲突，不得抢占。
    pub const ERR_DEVICE_BUSY: Self = Self(117);
    /// actual hwparams 不支持。
    pub const ERR_DEVICE_PARAMS: Self = Self(118);
    /// role/fs/P/G/hash 变化未重设计的 stale commit 被拒。
    pub const ERR_PLAN_STALE: Self = Self(119);
    /// 在途切换；REST 返回 conflict。
    pub const ERR_SWITCH_BUSY: Self = Self(120);
    /// 拒绝新 commit/切图；SafetyMute。
    pub const ERR_QUARANTINE_EXHAUSTED: Self = Self(121);
    /// 未知/未实现（含上混/下混/未知命令）。
    pub const ERR_UNSUPPORTED: Self = Self(122);

    // 201-299：block 局部 RT 错误（计数、丢块或 sanitize、继续流）
    /// 本块 invalid；丢弃；计数；quarantine。
    pub const RT_E_PARTIAL_LATE: Self = Self(201);
    /// 本块 invalid；安全输出；quarantine。
    pub const RT_E_CUTOFF: Self = Self(202);
    /// 本块 invalid；安全输出。
    pub const RT_E_BLOCK_INVALID: Self = Self(203);
    /// 丢弃本块；不跨 epoch；计数。
    pub const RT_E_EPOCH_MISMATCH: Self = Self(204);
    /// 丢弃本块；请求重新 stage/abort。
    pub const RT_E_PLAN_MISMATCH: Self = Self(205);
    /// fault（不变量破坏）-> SafetyMute。
    pub const RT_E_SLOT_STATE: Self = Self(206);
    /// 丢新块；连续 >= 4 -> 请求新 epoch。
    pub const RT_E_QUEUE_OVERRUN: Self = Self(207);
    /// 补静音；连续 >= 3 -> 请求 SafetyMute。
    pub const RT_E_QUEUE_UNDERRUN: Self = Self(208);
    /// sanitize 为 0；计数；置 INPUT_SANITIZED；继续。
    pub const RT_E_INPUT_NONFINITE: Self = Self(209);
    /// 饱和到 [-1, 1]；计数；继续。
    pub const RT_E_INPUT_CLIP: Self = Self(210);

    // 301-399：流级安全错误（ProtectionFallback / SafetyMute）
    /// 本块替换为预分配安全静音；> 0 即 fault 级。
    pub const RT_E_OUTPUT_NONFINITE: Self = Self(301);
    /// 保护/合理性检查失败；SafetyMute。
    pub const RT_E_GUARD_FAIL: Self = Self(302);
    /// 连续迟到超阈值 -> ProtectionFallback。
    pub const RT_E_LATE_STREAK: Self = Self(303);
    /// 连续欠载超阈值 -> SafetyMute。
    pub const RT_E_UNDERRUN_STREAK: Self = Self(304);
    /// 连续溢出超阈值 -> 请求新 epoch。
    pub const RT_E_OVERRUN_STREAK: Self = Self(305);
    /// 拒绝新提交/切图；SafetyMute。
    pub const RT_E_QUARANTINE_FULL: Self = Self(306);
    /// 进入 SilenceMute（若有安全静音）否则 Faulted。
    pub const RT_E_FALLBACK_UNAVAILABLE: Self = Self(307);

    // 401-499：进程级 fault（SafetyMute/Faulted，不自动恢复）
    /// SafetyMute/Faulted；不释放 quarantine；等进程重启。
    pub const RT_E_WORKER_LOST: Self = Self(401);
    /// SafetyMute/Faulted。
    pub const RT_E_MASTER_LOST: Self = Self(402);
    /// Faulted；禁止继续输出。
    pub const RT_E_INVARIANT: Self = Self(403);
    /// Faulted（结构/字段/flag 非法）。
    pub const RT_E_ABI_RUNTIME: Self = Self(404);
    /// 本块 invalid；硬截止超时；连续 2 块 -> SafetyMute。
    pub const RT_E_DEADLINE_HARD: Self = Self(405);
    /// SafetyMute/Faulted（heartbeat 超时）。
    pub const RT_E_WATCHDOG: Self = Self(406);
    /// 进入设备恢复状态机；期间静音；恢复需新 epoch。
    pub const RT_E_DEVICE_LOST: Self = Self(407);
}

// ---------------------------------------------------------------------------
// RT 可见 view 类型（core-v1 §12.3 / §13.2；全部 needs_drop == false）
// ---------------------------------------------------------------------------

/// 非拥有、无 Drop 的 plan 借用（只在 block 内有效，见 INV-L1/INV-L2）。
#[derive(Clone, Copy, Debug)]
pub struct PlanRef {
    ptr: core::ptr::NonNull<TspiPlanHeaderV1>,
    version: PlanVersion,
}

impl PlanRef {
    /// [NONRT] 从 mlocked plan arena 基址构造；调用者保证指针有效期跨越使用窗口。
    ///
    /// # Safety
    /// ptr 必须指向一个 alive 且 mlock 的 PlanArena（INV-L2），并保持到本 ref 不再使用。
    pub unsafe fn from_raw(ptr: *const TspiPlanHeaderV1, version: PlanVersion) -> Option<Self> {
        // 调用者保证指针有效；这里只做空指针检查。
        let nn = core::ptr::NonNull::new(ptr.cast_mut())?;
        Some(Self { ptr: nn, version })
    }

    /// 本 block 使用的 plan 版本（INV-L1：master 在 block 边界只读一次）。
    pub const fn version(self) -> PlanVersion {
        self.version
    }

    /// plan 头原始指针（[RT] 只读；不得解引用到 owned 类型）。
    pub fn as_ptr(self) -> *const TspiPlanHeaderV1 {
        self.ptr.as_ptr()
    }

    /// [RT] 只读 view；表视图仅涵盖 header 声明的表。
    ///
    /// # Safety
    /// 调用者保证 ptr/version 与 current_plan_ptr 一致，且 plan arena 在本次 block 内不被 unmap
    /// （retire gate RG-1..RG-4，core-v1 §7.5）。
    pub unsafe fn view(self) -> PlanView<'static> {
        // SAFETY: 见函数 Safety 契约。
        let header: &'static TspiPlanHeaderV1 = unsafe { &*self.ptr.as_ptr() };
        // scaffold：表切片由 T12/T26 按 header 偏移建立；本 stub 返回空表（不会被生产路径使用）。
        PlanView {
            header,
            paths: &[],
            diags: &[],
            roles: &[],
            coeffs: &[],
        }
    }
}

/// [NONRT]/[RT] plan arena 的只读视图（引用，无 Drop）。
#[derive(Clone, Copy, Debug)]
pub struct PlanView<'a> {
    /// plan 头。
    pub header: &'a TspiPlanHeaderV1,
    /// 路径表（按 (out_index, in_index) 升序）。
    pub paths: &'a [TspiPathV1],
    /// 对角 FIR 表（按 out_index 升序）。
    pub diags: &'a [TspiDiagFirV1],
    /// role 表（按 role_id 升序）。
    pub roles: &'a [TspiRoleV1],
    /// 系数 blob（f32 LE 按表顺序拼接）。
    pub coeffs: &'a [u8],
}

/// 一个 block 的 planar f32 输入/输出视图（非拥有；core 只见 f32 planar）。
pub struct BlockIo<'a> {
    /// block 身份。
    pub desc: &'a TspiBlockDescV1,
    /// 输入 planar f32（in_stride 样本/通道）。
    pub input: &'a [f32],
    /// 输出 planar f32（out_stride 样本/通道）。
    pub output: &'a mut [f32],
    /// 输入通道 stride（样本，16 的倍数）。
    pub in_stride: u32,
    /// 输出通道 stride（样本，16 的倍数）。
    pub out_stride: u32,
}

/// worker 的 RT 上下文（非拥有；ack/stats 由 RtArena 持有）。
pub struct WorkerRtCtx<'a> {
    /// 本 worker 编号。
    pub worker: WorkerId,
    /// 本 block ack（原子字段在内部，不拥有）。
    pub ack: &'a TspiBlockAckV1,
    /// 本 worker 统计。
    pub stats: &'a mut TspiWorkerStatsV1,
}

/// RT 控制块（core-v1 §13.2：原子字段+固定数组，needs_drop == false）。
///
/// 位置说明：core-v1 §12.3 的 core::rt::master_block 签名需要 &RtControl，
/// 而 §13.2 把类型位置写在 rt-host::swap；T06 scaffold 以 core::rt 为规范定义、
/// 由 khaen-rt-host::swap 重导出，保证两个路径都存在（见 README 契约偏差记录）。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct RtControl {
    /// current plan arena 地址位（0 = 无）；只由 master 在 block 边界读。
    pub current_plan_addr: AtomicU64,
    /// pending plan arena 地址位（0 = 无）。
    pub pending_plan_addr: AtomicU64,
    /// 当前生效 plan 版本。
    pub current_plan_version: AtomicU32,
    /// 待生效 plan 版本。
    pub pending_plan_version: AtomicU32,
    /// SWITCH_IDLE / SWITCH_ARMED。
    pub switch_state: AtomicU32,
    /// retire gate 已就绪的旧版本（0 = 无）。
    pub retire_ready_version: AtomicU32,
    /// 置位的 fault 请求（RtStatus code；只由 watchdog/control 写）。
    pub fault_request: AtomicU32,
    /// 置位的安全 mute 请求（1 = 请求；只由 watchdog/control 写）。
    pub mute_request: AtomicU32,
    /// 当前 epoch_id。
    pub epoch_id: AtomicU64,
    /// master heartbeat 的 block_seq。
    pub heartbeat_block_seq: AtomicU64,
    /// 固定保留区，必须为 0（scaffold 占位；T20 若需扩展必须走 v1.1）。
    pub reserved: [u64; 25],
}

impl RtControl {
    /// 构造全零控制块（仅 admission 前/测试）。
    pub const fn new() -> Self {
        Self {
            current_plan_addr: AtomicU64::new(0),
            pending_plan_addr: AtomicU64::new(0),
            current_plan_version: AtomicU32::new(0),
            pending_plan_version: AtomicU32::new(0),
            switch_state: AtomicU32::new(SWITCH_IDLE),
            retire_ready_version: AtomicU32::new(0),
            fault_request: AtomicU32::new(0),
            mute_request: AtomicU32::new(0),
            epoch_id: AtomicU64::new(0),
            heartbeat_block_seq: AtomicU64::new(0),
            reserved: [0; 25],
        }
    }
}

impl Default for RtControl {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// [RT] 函数签名 stub（core-v1 §12.3；T06 只建签名，不实现语义）
// ---------------------------------------------------------------------------

/// [RT] worker 的 block 处理入口（T09 实现；当前 stub）。
pub fn worker_block(
    worker: WorkerId,
    plan: PlanRef,
    io: BlockIo<'_>,
    ctx: &mut WorkerRtCtx<'_>,
) -> RtStatus {
    let _ = (worker, plan, io, ctx);
    // scaffold stub：不是 v1 RT 错误语义；T09 实现后必须替换。
    RtStatus::ERR_UNSUPPORTED
}

/// [RT] master 的 block 处理入口（T09 实现；当前 stub）。
pub fn master_block(
    plan: PlanRef,
    io: BlockIo<'_>,
    ctl: &RtControl,
    ack: &TspiBlockAckV1,
) -> RtStatus {
    let _ = (plan, io, ctl, ack);
    // scaffold stub：不是 v1 RT 错误语义；T09 实现后必须替换。
    RtStatus::ERR_UNSUPPORTED
}

/// [RT] 全速 direct kernel（T07 在 direct.rs 实现；当前 stub）。
pub fn kernel_direct(plan: PlanRef, io: &BlockIo<'_>) -> RtStatus {
    let _ = (plan, io);
    // scaffold stub：T07 实现后必须替换。
    RtStatus::ERR_UNSUPPORTED
}

/// [RT] partitioned/FFT kernel（T08 在 partitioned.rs 实现；当前 stub）。
pub fn kernel_partitioned(plan: PlanRef, io: &BlockIo<'_>) -> RtStatus {
    let _ = (plan, io);
    // scaffold stub：T08 实现后必须替换。
    RtStatus::ERR_UNSUPPORTED
}

const _: () = {
    // 容量/时限常量必须是 core-v1 §3/附录 B 的冻结值。
    assert!(MAX_INPUT_CHANNELS == 20);
    assert!(MAX_OUTPUT_CHANNELS == 22);
    assert!(MAX_INPUT_ROLES == 16);
    assert!(MAX_OUTPUT_ROLES == 16);
    assert!(MAX_LOW_BAND_PARTICIPANTS == 8);
    assert!(MAX_MATRIX_PATHS == 128);
    assert!(MAX_DIAG_FIRS == 22);
    assert!(MAX_PLAN_NODES == 256);
    assert!(SAMPLE_RATES_HZ[0] == 48_000 && SAMPLE_RATES_HZ[1] == 96_000);
    assert!(BLOCK_SIZES[0] == 64 && BLOCK_SIZES[1] == 128);
    assert!(BLOCK_DEFAULT == 128);
    // core-v1 §3.2 字面值：128@48k 取 ceil(2_666_667)，64@48k 取 1_333_333（比精确值小，是更严的
    // 保守值；§3.2 两行的取整口径不完全一致，T06 保留字面值并登记偏差 O-1）。
    assert!(TBLOCK_128_48K_NS == t_block_ns_ceil(48_000, 128));
    assert!(TBLOCK_64_48K_NS <= t_block_ns_ceil(48_000, 64));
    assert!(gate_ns_floor(48_000, 128) == 1_333_333);
    assert!(gate_ns_floor(48_000, 64) == 666_666);
    assert!(gate_ns_floor(96_000, 128) == 666_666);
    assert!(gate_ns_floor(96_000, 64) == 333_333);
    assert!(fft_size(64) == 128 && fft_size(128) == 256);
    assert!(MAX_PARTITIONS == 1500);
    assert!(max_partitions(16_384, 64) == 256);
    assert!(max_partitions(24_000, 16) == MAX_PARTITIONS);
    assert!(max_partitions(16_384, 128) == 128);
    assert!(fused_len_max(4096, 1024) == 5119);
    assert!(fused_len_max(8192, 4096) == 12287);
    assert!(
        TAPS_NORMAL_MAX == 4096 && TAPS_EXTENDED_MAX == 16_384 && TAPS_DESIGN_ONLY_ABOVE == 16_384
    );
    assert!(RT_WORKERS == 4 && RT_WORKERS_FALLBACK == 2 && PARALLEL_PHASES == 3);
    assert!(SPSC_CAPTURE_SLOTS == 4 && SPSC_PLAYBACK_SLOTS == 4 && QUARANTINE_SLOTS == 8);
    assert!(EVENT_LANES * EVENT_LANE_SLOTS == EVENT_RING_SLOTS && EVENT_RING_SLOTS == 1024);
    assert!(HIST_BUCKETS == 32 && MAX_WORKER_STACK_BYTES == 524_288);
    assert!(WORKER_BITS == 4 && ACK_MASK_ALL == (1u32 << WORKER_BITS) - 1);
    // core-v1 I04/v1.1：master 每 block 预算内 tick 上限 = 3（B1 1 + 可选 cutoff 1 + B3 1）。
    assert!(GRAPH_ID_BYTES == 32 && TICK_READS_PER_BLOCK_MAX == 3);
    assert!(RT_WORKERS_BENCH[0] == 1 && RT_WORKERS_BENCH[1] == 2 && RT_WORKERS_BENCH[2] == 4);
    assert!(!core::mem::needs_drop::<RtStatus>());
    assert!(!core::mem::needs_drop::<PlanRef>());
    assert!(!core::mem::needs_drop::<PlanView<'static>>());
    assert!(!core::mem::needs_drop::<WorkerRtCtx<'static>>());
    assert!(!core::mem::needs_drop::<RtControl>());
};
