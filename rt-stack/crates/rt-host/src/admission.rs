//! [NONRT] admission —— 仅内存准入（core-v1 §5.1/§3/§4.1/§4.2/§9.2/§9.3/§12.3）——WBS T18 写域。
//!
//! 本模块交付 RT 主模式启动前的内存准入：MCL_CURRENT|MCL_FUTURE 标志、RLIMIT_MEMLOCK
//! 判定、预创建线程栈预算、双图（2 × PlanArena）容量、控制面锁页容量预触。任何一项
//! 失败都返回冻结的 RtStatus 错误码且不产生 AdmissionReport、不启 RT 主模式——不得
//! warn 后自称 RT（core-v1 §5.1 / 决策 DSP-HOST-DECISION §5）。
//!
//! ## T18 边界（WBS T18；scheduler 独立 T45）
//!
//! | 归 T18（本文件） | 不归 T18 |
//! |---|---|
//! | MCL_CURRENT/MCL_FUTURE 标志值与 mlockall 一次调用 | CPU 拓扑/亲和/FIFO 优先级/IRQ 依赖（T45） |
//! | RLIMIT_MEMLOCK 探测/判定/容量纪律 | tick 源选择与 spin/cutoff/tick 标定（T19/T45） |
//! | 预创建线程栈（数量/上限/总预算）模型与超限拒启 | 真实线程创建/绑定/调度（T26/T45） |
//! | 双图 PlanArena 容量（current + pending/fallback）与两图预触 | plan 打包/校验/graph_id（T12/T13/T66） |
//! | 控制面（§11.13 共享结构 + 事件环 + ack/stats/hist）锁页容量预触 | block-version/worker ack/retire gate（T20） |
//! | 真实 arena 所有权、预触原语 pre_touch_bytes | 真实 arena mmap/持有/回收（T26） |
//!
//! AdmissionEnv 的 clock_source_ok/cpu_set_ok 只作为 T19/T45 已探测结果的准入闸门
//! 透传（false 即拒启），本模块不选源、不写调度参数。
//!
//! ## 失败语义（fail closed；不 warn 后自称 RT）
//!
//! - 校验/容量/权限/RLIMIT/mlockall/预触任一失败 → Err(RtStatus)；不构造报告；
//! - 失败发生在 mlockall 之前时不产生任何系统副作用（先算后锁，测试断言后端零调用）；
//! - mlockall 已成功后预触失败：仍返回 Err 且不启 RT 主模式；进程级锁页由 T26 的
//!   last-good/mute 策略处理（本模块不解除 RLIMIT/不 drop real arena）；
//! - 重试无状态：修好环境后再次 admit_with 可从零开始。
//!
//! ## 错误码（core-v1 §9.2 冻结，不新增码）
//!
//! | 码 | 名称 | T18 触发 |
//! |---:|---|---|
//! | 103 | ERR_BAD_LAYOUT | 非小端构建（A-ABI-4）；对齐参数非法 |
//! | 104 | ERR_RATE | env/plan fs 非 48k/96k 或两者不一致 |
//! | 105 | ERR_BLOCK_SIZE | env/plan B 非 64/128 或两者不一致 |
//! | 106 | ERR_CHANNELS | n_in/n_out 为 0 或越界（>20/>22） |
//! | 107 | ERR_ROLES | role 表 > 16 |
//! | 108 | ERR_PRECISION | plan precision != 0（runtime 只收 f32；A-ABI-10） |
//! | 110 | ERR_TAPS | max_taps > 16384（§3.3：runtime 准入拒绝） |
//! | 113 | ERR_CAPACITY | 容量算术溢出 / 声明预算 < 计算需求 |
//! | 114 | ERR_ADMISSION_MEMORY | RT 权限/mlock 权限/RLIMIT 不足/mlockall 失败/预触失败 |
//! | 115 | ERR_ADMISSION_SCHED | workers 非 {2,4}；cpu_set_ok=false（T45 输入） |
//! | 116 | ERR_ADMISSION_CLOCK | clock_source_ok=false（T19 输入） |
//! | 119 | ERR_PLAN_STALE | plan 内 workers 与 env 不一致（2 核是独立 profile） |
//! | 122 | ERR_UNSUPPORTED | 生产 debug 构建拒绝（§9.3）；非 Linux 平台后端不可用 |
//!
//! ## 内存区域模型（admission 预触的 5 个区域，顺序固定）
//!
//! | 区域 | 内容（公式） | 4096 页对齐后计入 mlock 预算 |
//! |---|---|---|
//! | Arena | capture 4 槽×B×N×4B + playback 4×B×O×4B + worker partial W×B×O×4B + quarantine 8×B×max(N,O)×4B + desc 8×64B + 安全静音 B×O×4B | 是 |
//! | GraphCurrent | PlanArena#1：header 128B + path/diag/role 表 + coeffs + ctl_meta + 频域 b-p 历史（每 path Q×FFT_SIZE×8B）+ 每 worker FFT scratch | 是 |
//! | GraphPending | PlanArena#2（pending/fallback/crossfade 旧图；始终双图预分配，§4.4/§5.5） | 是 |
//! | Stacks | 预创建线程栈：DSP workers + capture + playback + watchdog + control，每线程 512 KiB 上界（§3.3 MAX_WORKER_STACK_BYTES） | 是 |
//! | ControlPlane | §11.13 共享结构（RtControl 128B/SwapAck 64B/EventRingLane 8×64B/事件 1024×64B/WorkerRegs 4×128B/MasterRegs 128B/CrossfadeBudget 64B/ack 4×64B/WorkerStats 4×64B/hist 6×4×32×4B）+ CONTROL_RESERVE_BYTES（MCL_FUTURE 后控制面分配容量纪律预留） | 是 |
//!
//! MCL_FUTURE 生效后新分配都会被锁；因此 AdmissionEnv.preallocated_bytes（若声明）
//! 必须同时 ≥ 计算需求且 ≤ RLIMIT_MEMLOCK，否则后续控制面分配会破坏锁页预算 → 拒启。
//! preallocated_bytes == 0 表示未声明，按计算需求自身的页对齐值作为预算。
//!
//! ## 阈值来源（T18 按 core-v1 §1.2 授权定义 admission 阈值；非 ABI 常量）
//!
//! - STACK_BYTES_PER_THREAD = MAX_WORKER_STACK_BYTES = 524288（§3.3 硬上限；watchdog/
//!   control 是 SCHED_OTHER，本模块保守按同一上界计入，T45/T26 可只减不增）；
//! - FFT_SCRATCH_F32_ARRAYS = 4（time in/out + freq re/im，每 worker 每图一份）与
//!   FFT_COMPLEX_BYTES = 8（频域 b-p 历史 re+im f32）：T08/T26 实际 scratch 若更大必须
//!   回到本模块上调并重新准入，不得少算；
//! - CONTROL_RESERVE_BYTES = 64 KiB：MCL_FUTURE 后控制面（REST/WS/事件导出缓冲）的
//!   容量预留；T26 若需更大必须显式上调（容量纪律，不得静默超预算）。
//!
//! ## 平台后端与 S1 安全
//!
//! - Linux 64-bit：PlatformMemory 用直接 extern "C" 绑定 libc 的 mlockall(2)/
//!   getrlimit(2)/sysconf(3)（不新增 crate 依赖；errno 走 std::io::Error::last_os_error()），
//!   预触用按量分配 + 逐页写触 + 释放的容量探测；
//! - Windows/macOS 开发机：PlatformMemory::probe() 返回 ERR_UNSUPPORTED——admit() 在
//!   非 Linux 平台 fail closed；离线测试一律用 MockMemory 注入；
//! - 本模块不打开 ALSA/不启停服务/不写 tracefs/不落地任何板端动作，不扰 S1；
//!   真实 arena/线程/锁页生效核验属 T26/T29 板端窗口（NOT_RUN，见 evidence/T18）。
//!
//! ## 测试注册（与 T16/T17 同法）
//!
//! cargo 只自动发现 tests/*.rs；tests/T18/ 子目录经本文件末尾
//! #[cfg(test)] mod admission_tests { include!(...) } 进入 lib 单测构建。
//! integrator 不要再把 tests/T18 写进 public_registration.rs（会重复注册）。
//!
//! 运行：cargo test -p tspi-rt-host admission（executed > 0）。

use tspi_dsp_core::abi::{TspiDiagFirV1, TspiPathV1, TspiRoleV1};
use tspi_dsp_core::rt::{
    fft_size, max_partitions, PlanVersion, PlanView, RtStatus, BLOCK_SIZES, EVENT_RING_SLOTS,
    HIST_BUCKETS, MAX_DIAG_FIRS, MAX_INPUT_CHANNELS, MAX_MATRIX_PATHS, MAX_OUTPUT_CHANNELS,
    MAX_OUTPUT_ROLES, MAX_WORKER_STACK_BYTES, QUARANTINE_SLOTS, RT_WORKERS, RT_WORKERS_FALLBACK,
    SAMPLE_RATES_HZ, SPSC_CAPTURE_SLOTS, SPSC_PLAYBACK_SLOTS, TAPS_EXTENDED_MAX,
};

// ---------------------------------------------------------------------------
// mlock / RLIMIT 冻结语义常量（Linux ABI；与 core-v1 §5.1/§9.3 对应）
// ---------------------------------------------------------------------------

/// mlockall 标志：锁当前已映射页（Linux MCL_CURRENT = 0x1）。
pub const MCL_CURRENT: u32 = 0x1;
/// mlockall 标志：锁未来映射页（Linux MCL_FUTURE = 0x2）。
pub const MCL_FUTURE: u32 = 0x2;
/// T18 要求的 mlockall 组合：MCL_CURRENT | MCL_FUTURE（缺一不可）。
pub const MLOCK_ALL_FLAGS: u32 = MCL_CURRENT | MCL_FUTURE;
/// getrlimit 资源号：RLIMIT_MEMLOCK（Linux 通用 ABI = 8，x86_64/aarch64 一致）。
pub const RLIMIT_MEMLOCK_RESOURCE: i32 = 8;
/// RLIM_INFINITY（u64::MAX；rlim_cur == RLIM_INFINITY 表示无上限）。
pub const RLIM_INFINITY: u64 = u64::MAX;
/// 默认页大小（平台探测失败/非 Linux 的保守值；aarch64/x86_64 Linux 默认 4 KiB）。
pub const DEFAULT_PAGE_SIZE_BYTES: u64 = 4096;
/// 合法页大小下限（防御异常探测值；真实 Linux 页大小 >= 4 KiB，下限 1 KiB 已足够宽）。
pub const MIN_PAGE_SIZE_BYTES: u64 = 1024;

/// 是否同时包含 MCL_CURRENT 与 MCL_FUTURE（T18 的 mlockall 组合校验）。
pub const fn mlock_flags_complete(flags: u32) -> bool {
    flags & MLOCK_ALL_FLAGS == MLOCK_ALL_FLAGS
}

// ---------------------------------------------------------------------------
// 预创建线程栈 / 控制面 / 双图容量常量（T18 admission 阈值，非 ABI）
// ---------------------------------------------------------------------------

/// 每 RT 线程栈预算 = core-v1 §3.3 MAX_WORKER_STACK_BYTES（512 KiB）。
pub const STACK_BYTES_PER_THREAD: u64 = MAX_WORKER_STACK_BYTES as u64;
/// DSP worker 每线程栈（与 §3.3 上界一致）。
pub const DSP_STACK_BYTES: u64 = STACK_BYTES_PER_THREAD;
/// capture/playback I/O 线程每线程栈（保守按同一上界）。
pub const IO_STACK_BYTES: u64 = STACK_BYTES_PER_THREAD;
/// watchdog 线程栈（SCHED_OTHER；保守按同一上界，T45/T26 只可下调）。
pub const WATCHDOG_STACK_BYTES: u64 = STACK_BYTES_PER_THREAD;
/// control 线程栈（SCHED_OTHER；保守按同一上界，T45/T26 只可下调）。
pub const CONTROL_STACK_BYTES: u64 = STACK_BYTES_PER_THREAD;

/// I/O 线程数：capture + playback（core-v1 §4.1）。
pub const IO_THREADS: u64 = 2;
/// watchdog 线程数（core-v1 §4.1）。
pub const WATCHDOG_THREADS: u64 = 1;
/// control 线程数（§4.1：≥1；T18 按 1 计，扩容是容量纪律变更）。
pub const CONTROL_THREADS: u64 = 1;

/// 预创建线程总数：workers + IO + watchdog + control（生产 4 核 = 8，2 核独立 profile = 6）。
pub const fn precreated_threads(workers: u8) -> u64 {
    workers as u64 + IO_THREADS + WATCHDOG_THREADS + CONTROL_THREADS
}

/// 预创建线程栈总字节（每线程按 STACK_BYTES_PER_THREAD 上界）。
pub const fn precreated_stacks_bytes(workers: u8) -> u64 {
    precreated_threads(workers) * STACK_BYTES_PER_THREAD
}

/// §11.13 TspiRtControlV1 字节数。
pub const RT_CONTROL_BYTES: u64 = 128;
/// §11.13 TspiSwapAckV1 字节数。
pub const SWAP_ACK_BYTES: u64 = 64;
/// §11.13 TspiEventRingLaneV1 字节数。
pub const EVENT_LANE_HEADER_BYTES: u64 = 64;
/// 事件环 lane 数（§8.1：固定 8 lane）。
pub const EVENT_LANES: u64 = 8;
/// §11.5 TspiEventV1 记录字节数。
pub const EVENT_RECORD_BYTES: u64 = 64;
/// §11.13 TspiWorkerRegsV1 字节数。
pub const WORKER_REGS_BYTES: u64 = 128;
/// §11.13 TspiMasterRegsV1 字节数。
pub const MASTER_REGS_BYTES: u64 = 128;
/// §11.13 TspiCrossfadeBudgetV1 字节数。
pub const CROSSFADE_BUDGET_BYTES: u64 = 64;
/// §3.4 ACK_SLOTS（固定 ack 槽数，与 playback ring 对齐）。
pub const ACK_SLOTS: u64 = 4;
/// §11.6 TspiBlockAckV1 字节数（ack 槽元素）。
pub const BLOCK_ACK_BYTES: u64 = 64;
/// §11.8 TspiWorkerStatsV1 字节数。
pub const WORKER_STATS_BYTES: u64 = 64;
/// §8.4 每 worker 每 metric 固定直方图 metric 数。
pub const HIST_METRICS: u64 = 6;
/// §8.4 直方图桶值字节数（u32）。
pub const HIST_BUCKET_BYTES: u64 = 4;
/// MCL_FUTURE 之后控制面分配容量纪律预留（T18 阈值；T26 只可上调不得静默超）。
pub const CONTROL_RESERVE_BYTES: u64 = 64 * 1024;
/// 每 worker 每图 FFT scratch 的 f32 数组数（time in/out + freq re/im）。
pub const FFT_SCRATCH_F32_ARRAYS: u64 = 4;
/// 频域 b-p 历史每 bin 字节数（re+im f32）。
pub const FFT_COMPLEX_BYTES: u64 = 8;
/// f32 样本字节数（RT arena payload）。
pub const SAMPLE_BYTES: u64 = 4;

/// §11.10 PlanArena header 字节数（TspiPlanHeaderV1）。
pub const PLAN_HEADER_BYTES: u64 = 128;
/// §11.10 path 表项字节数（TspiPathV1）。
pub const PATH_ENTRY_BYTES: u64 = core::mem::size_of::<TspiPathV1>() as u64;
/// §11.10 diag FIR 表项字节数（TspiDiagFirV1）。
pub const DIAG_ENTRY_BYTES: u64 = core::mem::size_of::<TspiDiagFirV1>() as u64;
/// §11.10 role 表项字节数（TspiRoleV1）。
pub const ROLE_ENTRY_BYTES: u64 = core::mem::size_of::<TspiRoleV1>() as u64;
/// §11.3 TspiBlockDescV1（SPSC desc 槽元素）字节数。
pub const DESC_BYTES: u64 = 64;

/// §11.13 控制面共享结构字节（不含 CONTROL_RESERVE_BYTES）。
pub const fn control_plane_structs_bytes() -> u64 {
    RT_CONTROL_BYTES
        + SWAP_ACK_BYTES
        + EVENT_LANES * EVENT_LANE_HEADER_BYTES
        + EVENT_RING_SLOTS as u64 * EVENT_RECORD_BYTES
        + RT_WORKERS as u64 * WORKER_REGS_BYTES
        + MASTER_REGS_BYTES
        + CROSSFADE_BUDGET_BYTES
        + ACK_SLOTS * BLOCK_ACK_BYTES
        + RT_WORKERS as u64 * WORKER_STATS_BYTES
        + HIST_METRICS * RT_WORKERS as u64 * HIST_BUCKETS as u64 * HIST_BUCKET_BYTES
}

/// 控制面锁页容量 = §11.13 结构 + CONTROL_RESERVE_BYTES。
pub const fn control_plane_total_bytes() -> u64 {
    control_plane_structs_bytes() + CONTROL_RESERVE_BYTES
}

/// 每图每 worker FFT scratch 字节。
pub const fn graph_scratch_bytes(block_size: u16, workers: u8) -> u64 {
    (workers as u64) * (fft_size(block_size) as u64) * FFT_SCRATCH_F32_ARRAYS * SAMPLE_BYTES
}

/// 单 path 频域 b-p 历史字节：Q × FFT_SIZE × (re+im f32)。
pub const fn path_history_bytes(taps: u32, block_size: u16) -> u64 {
    (max_partitions(taps, block_size) as u64) * (fft_size(block_size) as u64) * FFT_COMPLEX_BYTES
}

// ---------------------------------------------------------------------------
// 算术原语（checked；溢出/非法参数 → 冻结码，不 panic）
// ---------------------------------------------------------------------------

/// 页对齐向上取整；align == 0 → ERR_BAD_LAYOUT，溢出 → ERR_CAPACITY。
pub fn align_up(value: u64, align: u64) -> Result<u64, RtStatus> {
    if align == 0 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let rem = value % align;
    if rem == 0 {
        return Ok(value);
    }
    value.checked_add(align - rem).ok_or(RtStatus::ERR_CAPACITY)
}

/// 覆盖 bytes 所需页数（page == 0 → ERR_BAD_LAYOUT）。
pub fn pages_for(bytes: u64, page: u64) -> Result<u64, RtStatus> {
    if page == 0 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if bytes == 0 {
        return Ok(0);
    }
    let full = bytes / page;
    if bytes % page == 0 {
        Ok(full)
    } else {
        full.checked_add(1).ok_or(RtStatus::ERR_CAPACITY)
    }
}

/// 逐页写触真实内存区间（admission 预触原语；返回写触页数）。
///
/// 逐页写一个字节迫使页驻留（配合 MCL_CURRENT|MCL_FUTURE 即为锁页预触）；本函数
/// 无 unsafe、无分配，T26 在真实 owned arena 上复用同一原语（§12.3 预触语义）。
/// page == 0 时不做任何写触并返回 0。
pub fn pre_touch_bytes(buf: &mut [u8], page: u64) -> u64 {
    if page == 0 {
        return 0;
    }
    let page_usize = match usize::try_from(page) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    if page_usize == 0 {
        return 0;
    }
    let mut touched = 0u64;
    let mut offset = 0usize;
    while offset < buf.len() {
        buf[offset] = 0xA5;
        touched += 1;
        match offset.checked_add(page_usize) {
            Some(next) => offset = next,
            None => break,
        }
    }
    touched
}

fn checked_mul(parts: &[u64]) -> Result<u64, RtStatus> {
    let mut acc: u64 = 1;
    let mut index = 0usize;
    while index < parts.len() {
        acc = match acc.checked_mul(parts[index]) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_CAPACITY),
        };
        index += 1;
    }
    Ok(acc)
}

fn checked_add(a: u64, b: u64) -> Result<u64, RtStatus> {
    a.checked_add(b).ok_or(RtStatus::ERR_CAPACITY)
}

// ---------------------------------------------------------------------------
// 区域枚举与内存容量模型
// ---------------------------------------------------------------------------

/// admission 预触区域（顺序固定；测试断言顺序）。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionId {
    /// RtArena 可变 payload/desc/安全静音（§4.2）。
    Arena = 0,
    /// PlanArena #1（当前生效图）。
    GraphCurrent = 1,
    /// PlanArena #2（pending/fallback/crossfade 旧图；始终预分配）。
    GraphPending = 2,
    /// 预创建线程栈。
    Stacks = 3,
    /// 控制面共享结构 + CONTROL_RESERVE_BYTES。
    ControlPlane = 4,
}

impl RegionId {
    /// 固定预触顺序：Arena → GraphCurrent → GraphPending → Stacks → ControlPlane。
    pub const ALL: [RegionId; 5] = [
        RegionId::Arena,
        RegionId::GraphCurrent,
        RegionId::GraphPending,
        RegionId::Stacks,
        RegionId::ControlPlane,
    ];

    /// 稳定名称（事件/证据/错误导出用，不含分配）。
    pub const fn name(self) -> &'static str {
        match self {
            RegionId::Arena => "arena",
            RegionId::GraphCurrent => "graph_current",
            RegionId::GraphPending => "graph_pending",
            RegionId::Stacks => "stacks",
            RegionId::ControlPlane => "control_plane",
        }
    }
}

/// admission 计算的容量模型（未对齐原始字节；页对齐在 aligned_* 方法里做）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryPlan {
    /// capture 4 槽 payload（B×N×f32）。
    pub capture_payload_bytes: u64,
    /// playback 4 槽 payload（B×O×f32）。
    pub playback_payload_bytes: u64,
    /// 每 worker 私有 partial（B×O×f32）。
    pub worker_partial_bytes: u64,
    /// quarantine 8 槽（B×max(N,O)×f32）。
    pub quarantine_bytes: u64,
    /// 安全静音输出块（B×O×f32；admission 预分配，§5.5）。
    pub safety_silence_bytes: u64,
    /// SPSC desc 槽（8×64 B）。
    pub desc_bytes: u64,
    /// RtArena 合计（上 6 项相加；不含 PlanArena/栈/控制面）。
    pub arena_bytes: u64,
    /// 每图每 worker FFT scratch（两图各一份）。
    pub graph_scratch_bytes: u64,
    /// 单图 PlanArena 字节（current）。
    pub graph_current_bytes: u64,
    /// 单图 PlanArena 字节（pending/fallback）。
    pub graph_pending_bytes: u64,
    /// 预创建线程栈总字节（未页对齐）。
    pub stacks_bytes: u64,
    /// 控制面锁页容量（§11.13 结构 + 预留）。
    pub control_plane_bytes: u64,
    /// 以上全部原始字节合计（未页对齐；供诊断/断言用）。
    pub total_bytes: u64,
    /// plan 非零 path 数（容量核算输入）。
    pub path_count: u32,
    /// plan 对角 FIR 数（容量核算输入）。
    pub diag_count: u32,
}

impl MemoryPlan {
    /// 区域原始字节。
    pub fn region_bytes(&self, region: RegionId) -> u64 {
        match region {
            RegionId::Arena => self.arena_bytes,
            RegionId::GraphCurrent => self.graph_current_bytes,
            RegionId::GraphPending => self.graph_pending_bytes,
            RegionId::Stacks => self.stacks_bytes,
            RegionId::ControlPlane => self.control_plane_bytes,
        }
    }

    /// 区域页对齐字节（预触/mlock 预算口径）。
    pub fn aligned_region_bytes(&self, region: RegionId, page: u64) -> Result<u64, RtStatus> {
        align_up(self.region_bytes(region), page)
    }

    /// 全部区域页对齐合计（= 必须能被 RLIMIT_MEMLOCK 容纳的量）。
    pub fn aligned_total_bytes(&self, page: u64) -> Result<u64, RtStatus> {
        let mut total = 0u64;
        for region in RegionId::ALL {
            total = checked_add(total, self.aligned_region_bytes(region, page)?)?;
        }
        Ok(total)
    }

    /// 双图合计（current + pending）。
    pub fn graphs_bytes(&self) -> u64 {
        self.graph_current_bytes + self.graph_pending_bytes
    }
}

/// 由 plan 视图 + env 计算内存容量模型（纯算术；调用前必须先 validate）。
///
/// plan 表规模以 PlanView 实际切片为准（真实需要的内存），header.total_coeff_bytes
/// 与 coeffs.len() 取较大者（fail-safe）。任何乘法/加法溢出 → ERR_CAPACITY。
pub fn plan_memory(plan: &PlanView<'_>, env: &AdmissionEnv) -> Result<MemoryPlan, RtStatus> {
    let header = plan.header;
    let b = header.block_size as u64;
    let workers = env.rt_workers as u64;
    let n = header.n_in as u64;
    let o = header.n_out as u64;
    let max_channels = if n > o { n } else { o };

    let capture_payload_bytes = checked_mul(&[SPSC_CAPTURE_SLOTS as u64, b, n, SAMPLE_BYTES])?;
    let playback_payload_bytes = checked_mul(&[SPSC_PLAYBACK_SLOTS as u64, b, o, SAMPLE_BYTES])?;
    let worker_partial_bytes = checked_mul(&[workers, b, o, SAMPLE_BYTES])?;
    let quarantine_bytes = checked_mul(&[QUARANTINE_SLOTS as u64, b, max_channels, SAMPLE_BYTES])?;
    let safety_silence_bytes = checked_mul(&[b, o, SAMPLE_BYTES])?;
    let desc_bytes = checked_mul(&[
        (SPSC_CAPTURE_SLOTS + SPSC_PLAYBACK_SLOTS) as u64,
        DESC_BYTES,
    ])?;
    let mut arena_bytes = capture_payload_bytes;
    arena_bytes = checked_add(arena_bytes, playback_payload_bytes)?;
    arena_bytes = checked_add(arena_bytes, worker_partial_bytes)?;
    arena_bytes = checked_add(arena_bytes, quarantine_bytes)?;
    arena_bytes = checked_add(arena_bytes, safety_silence_bytes)?;
    arena_bytes = checked_add(arena_bytes, desc_bytes)?;

    // 表：path/diag/role（按实际切片长度，§11.10 64B 对齐由 align_up 计入）。
    let path_table_bytes = checked_mul(&[plan.paths.len() as u64, PATH_ENTRY_BYTES])?;
    let diag_table_bytes = checked_mul(&[plan.diags.len() as u64, DIAG_ENTRY_BYTES])?;
    let role_table_bytes = checked_mul(&[plan.roles.len() as u64, ROLE_ENTRY_BYTES])?;

    // 系数 blob：header 声明值与实际切片取大（fail-safe）。
    let declared_coeffs = header.total_coeff_bytes as u64;
    let actual_coeffs = plan.coeffs.len() as u64;
    let coeff_bytes = if declared_coeffs > actual_coeffs {
        declared_coeffs
    } else {
        actual_coeffs
    };
    let ctl_meta_bytes = header.ctl_meta_len as u64;

    // 频域 b-p 历史：逐 path/diag 按 Q × FFT_SIZE × 8 B 累加（checked）。
    let mut history_bytes = 0u64;
    for path in plan.paths {
        history_bytes = checked_add(
            history_bytes,
            path_history_bytes(path.taps, header.block_size),
        )?;
    }
    let mut diag_history_bytes = 0u64;
    for diag in plan.diags {
        diag_history_bytes = checked_add(
            diag_history_bytes,
            path_history_bytes(diag.taps, header.block_size),
        )?;
    }

    let graph_scratch_bytes = graph_scratch_bytes(header.block_size, env.rt_workers);

    // 单图：header + 表 + 系数 + ctl_meta + 历史 + scratch，各段 64B 对齐后相加。
    let mut graph_one = align_up(PLAN_HEADER_BYTES, 64)?;
    graph_one = checked_add(graph_one, align_up(path_table_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(diag_table_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(role_table_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(coeff_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(ctl_meta_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(history_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(diag_history_bytes, 64)?)?;
    graph_one = checked_add(graph_one, align_up(graph_scratch_bytes, 64)?)?;

    let stacks_bytes = precreated_stacks_bytes(env.rt_workers);
    let control_plane_bytes = control_plane_total_bytes();

    let mut total_bytes = arena_bytes;
    total_bytes = checked_add(total_bytes, graph_one)?;
    total_bytes = checked_add(total_bytes, graph_one)?;
    total_bytes = checked_add(total_bytes, stacks_bytes)?;
    total_bytes = checked_add(total_bytes, control_plane_bytes)?;

    Ok(MemoryPlan {
        capture_payload_bytes,
        playback_payload_bytes,
        worker_partial_bytes,
        quarantine_bytes,
        safety_silence_bytes,
        desc_bytes,
        arena_bytes,
        graph_scratch_bytes,
        graph_current_bytes: graph_one,
        graph_pending_bytes: graph_one,
        stacks_bytes,
        control_plane_bytes,
        total_bytes,
        path_count: plan.paths.len() as u32,
        diag_count: plan.diags.len() as u32,
    })
}

// ---------------------------------------------------------------------------
// admission 输入/报告（T06 scaffold 字段冻结；T18 不改语义）
// ---------------------------------------------------------------------------

/// admission 输入环境（拓扑/RT 权限/RLIMIT/CPU set/clock 源/栈/mlock 预算；NONRT）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionEnv {
    /// 目标采样率（48000/96000）。
    pub sample_rate_hz: u32,
    /// 目标块长（64/128）。
    pub block_size: u16,
    /// 计划 worker 数（4 或 2；2 核是独立 profile）。
    pub rt_workers: u8,
    /// RLIMIT_MEMLOCK 上限（字节）；0 = 未探测，由后端读取。
    pub mlock_limit_bytes: u64,
    /// 预触/预分配总字节预算；0 = 未声明，按计算需求自身页对齐值作为预算。
    pub preallocated_bytes: u64,
    /// RT 权限探测是否通过。
    pub rt_privilege_ok: bool,
    /// 预算内 tick 源可用性（T19 探测结果，T18 只做闸门）。
    pub clock_source_ok: bool,
    /// CPU set/拓扑是否可绑定（T45 探测结果，T18 只做闸门）。
    pub cpu_set_ok: bool,

    // ---- v1.1/I15 + T45 O-03 合并的 sched 字段（T105）----
    /// 允许的 CPU 掩码（T45 实测 Cpus_allowed_list；0 = 未声明）。
    pub cpu_set_mask: u64,
    /// 允许的 CPU 列表（最多 4 项，未用项 0；T45 实测后回写）。
    pub allowed_cpu: [u16; 4],
    /// T45 参数化计划选定的实际 CPU（admission 原样记录进报告；全 0 = 未声明）。
    pub actual_cpu: [u16; 4],
    /// 计划 expected workers（0 = 用 `rt_workers`；正常为 2/4，须与 rt_workers 一致）。
    pub expected_workers: u8,
    /// 每 worker FIFO 调度优先级（§3.4 初值 70；T45 提供）。
    pub sched_priority: [u8; 4],
    /// 依赖的 IRQ/设备位图（T45 位义；0 = 未声明）。
    pub irq_dependency_mask: u64,
}

impl AdmissionEnv {
    /// 保守默认：48k/128/4 worker，其余探测标 false。
    pub const fn new() -> Self {
        Self {
            sample_rate_hz: 48_000,
            block_size: 128,
            rt_workers: 4,
            mlock_limit_bytes: 0,
            preallocated_bytes: 0,
            rt_privilege_ok: false,
            clock_source_ok: false,
            cpu_set_ok: false,
            cpu_set_mask: 0,
            allowed_cpu: [0; 4],
            actual_cpu: [0; 4],
            expected_workers: 0,
            sched_priority: [70; 4],
            irq_dependency_mask: 0,
        }
    }
}

impl Default for AdmissionEnv {
    fn default() -> Self {
        Self::new()
    }
}

/// admission 成功报告（失败时不产生；不得用 warn 代替）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionReport {
    /// 本 plan 版本。
    pub plan_version: PlanVersion,
    /// RtArena 字节（页对齐；Arena 区域）。
    pub arena_bytes: u64,
    /// 已 mlock 字节（5 区域页对齐合计）。
    pub mlocked_bytes: u64,
    /// RT 线程栈字节（页对齐；Stacks 区域）。
    pub stack_bytes: u64,
    /// worker 数。
    pub workers: u8,
    /// plan 的 graph_id（SHA-256）。
    pub graph_id: [u8; 32],

    // ---- v1.1/I15 + T45 O-03 合并的 sched 报告字段（T105）----
    /// 实际 expected workers（= env.expected_workers，0 时取 rt_workers）。
    pub expected_workers: u8,
    /// 参数化 all-ack 位图（expected∈{2,4} 时 0x3/0xF；禁止硬写 0xF）。
    pub ack_mask: u32,
    /// 每 worker 实际 CPU（T45 计划回写；未声明全 0）。
    pub actual_cpu: [u16; 4],
    /// 每 worker FIFO 优先级（T45 计划回写）。
    pub sched_priority: [u8; 4],
    /// 依赖的 IRQ/设备位图（T45 位义）。
    pub irq_dependency_mask: u64,
}

// ---------------------------------------------------------------------------
// 平台后端抽象（可注入 mock；Linux 真实 syscall 见 platform 模块）
// ---------------------------------------------------------------------------

/// admission 所使用的内存系统后端（NONRT；仅启动期调用）。
///
/// 契约：返回 Err 即视为该步骤失败（admission 统一映射为冻结码后拒启）；实现不得
/// warn/降级后返回 Ok。真实实现的 pre_touch 必须是真实写触/容量探测，不得空转。
pub trait MemoryBackend {
    /// 读取 RLIMIT_MEMLOCK 当前 soft limit（RLIM_INFINITY = 无上限）。
    fn read_memlock_limit(&mut self) -> Result<u64, RtStatus>;

    /// 平台页大小（字节；必须为 2 的幂，否则 admission 拒启）。
    fn page_size(&mut self) -> u64;

    /// 执行 mlockall(flags)；T18 只以 MLOCK_ALL_FLAGS 调用一次。
    fn mlock_all(&mut self, flags: u32) -> Result<(), RtStatus>;

    /// 对指定区域按 bytes 做页预触/锁页容量探测。
    fn pre_touch(&mut self, region: RegionId, bytes: u64) -> Result<(), RtStatus>;
}

/// mock 预触调用记录项（测试断言顺序/字节数用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PretouchCall {
    /// 区域 id。
    pub region: u8,
    /// 请求预触字节（页对齐）。
    pub bytes: u64,
}

/// mock 预触调用日志容量（5 区域 + 余量）。
pub const PRETOUCH_LOG_CAP: usize = 8;

/// 可注入的 mock 内存后端（离线测试用；可注入 mlockall/RLIMIT/预触失败）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockMemory {
    page_size: u64,
    memlock_limit: u64,
    read_limit_error: Option<u32>,
    mlock_error: Option<u32>,
    pretouch_fail_at: Option<u32>,
    pretouch_fail_region: Option<u8>,
    pretouch_error: u32,
    read_limit_calls: u32,
    mlock_calls: u32,
    mlock_flags: u32,
    pretouch_calls: u32,
    pretouch_log: [PretouchCall; PRETOUCH_LOG_CAP],
    touched_bytes: u64,
}

impl MockMemory {
    /// 默认：4 KiB 页、RLIMIT 无限、全部成功、无记录。
    pub const fn new() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE_BYTES,
            memlock_limit: RLIM_INFINITY,
            read_limit_error: None,
            mlock_error: None,
            pretouch_fail_at: None,
            pretouch_fail_region: None,
            pretouch_error: RtStatus::ERR_ADMISSION_MEMORY.code(),
            read_limit_calls: 0,
            mlock_calls: 0,
            mlock_flags: 0,
            pretouch_calls: 0,
            pretouch_log: [PretouchCall {
                region: 0,
                bytes: 0,
            }; PRETOUCH_LOG_CAP],
            touched_bytes: 0,
        }
    }

    /// 设置页大小。
    pub const fn with_page_size(mut self, page_size: u64) -> Self {
        self.page_size = page_size;
        self
    }

    /// 设置 RLIMIT_MEMLOCK 返回值。
    pub const fn with_memlock_limit(mut self, limit: u64) -> Self {
        self.memlock_limit = limit;
        self
    }

    /// 注入 RLIMIT 读取失败（返回给定错误码）。
    pub const fn failing_read_limit(mut self, code: u32) -> Self {
        self.read_limit_error = Some(code);
        self
    }

    /// 注入 mlockall 失败（返回给定错误码）。
    pub const fn failing_mlock(mut self, code: u32) -> Self {
        self.mlock_error = Some(code);
        self
    }

    /// 注入第 index 次（0-based）预触调用失败。
    pub const fn failing_pretouch_at(mut self, index: u32) -> Self {
        self.pretouch_fail_at = Some(index);
        self
    }

    /// 注入指定区域预触失败。
    pub const fn failing_pretouch_region(mut self, region: RegionId) -> Self {
        self.pretouch_fail_region = Some(region as u8);
        self
    }

    /// 设置预触失败返回码。
    pub const fn with_pretouch_error(mut self, code: u32) -> Self {
        self.pretouch_error = code;
        self
    }

    /// RLIMIT 读取调用次数。
    pub const fn read_limit_calls(&self) -> u32 {
        self.read_limit_calls
    }

    /// mlockall 调用次数。
    pub const fn mlock_calls(&self) -> u32 {
        self.mlock_calls
    }

    /// 最近一次 mlockall 的 flags（未调用为 0）。
    pub const fn mlock_flags(&self) -> u32 {
        self.mlock_flags
    }

    /// 预触调用次数。
    pub const fn pretouch_calls(&self) -> u32 {
        self.pretouch_calls
    }

    /// 第 index 次预触记录（越界/未发生返回 None）。
    pub fn pretouch_call(&self, index: usize) -> Option<PretouchCall> {
        if (index as u32) >= self.pretouch_calls {
            return None;
        }
        self.pretouch_log.get(index).copied()
    }

    /// 成功预触累计字节。
    pub const fn touched_bytes(&self) -> u64 {
        self.touched_bytes
    }

    fn record_pretouch(&mut self, region: RegionId, bytes: u64) {
        let index = self.pretouch_calls as usize;
        if index < PRETOUCH_LOG_CAP {
            self.pretouch_log[index] = PretouchCall {
                region: region as u8,
                bytes,
            };
        }
        self.pretouch_calls += 1;
    }
}

impl Default for MockMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBackend for MockMemory {
    fn read_memlock_limit(&mut self) -> Result<u64, RtStatus> {
        self.read_limit_calls += 1;
        match self.read_limit_error {
            Some(code) => Err(RtStatus::from_code(code)),
            None => Ok(self.memlock_limit),
        }
    }

    fn page_size(&mut self) -> u64 {
        self.page_size
    }

    fn mlock_all(&mut self, flags: u32) -> Result<(), RtStatus> {
        self.mlock_calls += 1;
        self.mlock_flags = flags;
        match self.mlock_error {
            Some(code) => Err(RtStatus::from_code(code)),
            None => Ok(()),
        }
    }

    fn pre_touch(&mut self, region: RegionId, bytes: u64) -> Result<(), RtStatus> {
        let index = self.pretouch_calls;
        self.record_pretouch(region, bytes);
        if self.pretouch_fail_at == Some(index) || self.pretouch_fail_region == Some(region as u8) {
            return Err(RtStatus::from_code(self.pretouch_error));
        }
        self.touched_bytes = self.touched_bytes.saturating_add(bytes);
        Ok(())
    }
}

/// 平台内存后端（Linux 64-bit 真实 syscall；其它平台 probe() 恒 ERR_UNSUPPORTED）。
#[cfg(any(not(target_os = "linux"), not(target_pointer_width = "64")))]
mod platform {
    use super::{MemoryBackend, RegionId, RtStatus, DEFAULT_PAGE_SIZE_BYTES};

    /// 非 Linux64 开发平台的占位后端：fail closed，不冒充真实 mlock/RLIMIT。
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct PlatformMemory;

    impl PlatformMemory {
        /// 探测平台后端；非 Linux64 不可用。
        pub fn probe() -> Result<Self, RtStatus> {
            Err(RtStatus::ERR_UNSUPPORTED)
        }
    }

    impl MemoryBackend for PlatformMemory {
        fn read_memlock_limit(&mut self) -> Result<u64, RtStatus> {
            Err(RtStatus::ERR_UNSUPPORTED)
        }

        fn page_size(&mut self) -> u64 {
            DEFAULT_PAGE_SIZE_BYTES
        }

        fn mlock_all(&mut self, _flags: u32) -> Result<(), RtStatus> {
            Err(RtStatus::ERR_UNSUPPORTED)
        }

        fn pre_touch(&mut self, _region: RegionId, _bytes: u64) -> Result<(), RtStatus> {
            Err(RtStatus::ERR_UNSUPPORTED)
        }
    }
}

/// Linux 64-bit 真实后端：mlockall(2) + getrlimit(2) + sysconf(3) 直接 FFI。
///
/// 说明：直接声明 libc 符号（std 已链接 libc），不引入新 crate 依赖；errno 经
/// std::io::Error::last_os_error() 读取。预触为容量探测（按量分配 + 逐页写触 + 释放），
/// 真实 owned arena 的预触由 T26 用同一 pre_touch_bytes 原语在 arena 上执行。
/// 本路径未在 Linux 实机执行（本任务开发机 Windows；NOT_RUN 见 evidence/T18）。
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod platform {
    use super::{
        pages_for, pre_touch_bytes, MemoryBackend, RegionId, RtStatus, DEFAULT_PAGE_SIZE_BYTES,
    };
    use core::ffi::{c_int, c_long};

    /// struct rlimit（glibc/musl 64-bit：两个 unsigned long）。
    #[repr(C)]
    struct Rlimit {
        rlim_cur: u64,
        rlim_max: u64,
    }

    /// Linux RLIMIT_MEMLOCK（bits/resource.h，x86_64/aarch64 均为 8）。
    const RLIMIT_MEMLOCK: c_int = 8;
    /// _SC_PAGESIZE（bits/confname.h 通用值 30）。
    const SC_PAGESIZE: c_int = 30;
    /// sysconf 返回值低于此值时按默认 4 KiB 处理（防御异常）。
    const MIN_SANE_PAGE_SIZE: c_long = 1024;

    unsafe extern "C" {
        fn mlockall(flags: c_int) -> c_int;
        fn getrlimit(resource: c_int, rlim: *mut Rlimit) -> c_int;
        fn sysconf(name: c_int) -> c_long;
    }

    /// Linux 64-bit 真实内存后端。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PlatformMemory {
        page_size: u64,
    }

    impl PlatformMemory {
        /// 探测平台：读取页大小（失败回退 DEFAULT_PAGE_SIZE_BYTES，不静默成功）。
        pub fn probe() -> Result<Self, RtStatus> {
            // SAFETY: sysconf 只读常量系统参数；无指针参数。
            let raw = unsafe { sysconf(SC_PAGESIZE) };
            let page_size = if raw >= MIN_SANE_PAGE_SIZE {
                raw as u64
            } else {
                DEFAULT_PAGE_SIZE_BYTES
            };
            Ok(Self { page_size })
        }
    }

    impl MemoryBackend for PlatformMemory {
        fn read_memlock_limit(&mut self) -> Result<u64, RtStatus> {
            let mut limit = Rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: 指针指向本函数栈上初始化的 Rlimit，getrlimit 写入恰好该结构。
            let rc = unsafe { getrlimit(RLIMIT_MEMLOCK, &mut limit) };
            if rc != 0 {
                return Err(RtStatus::ERR_ADMISSION_MEMORY);
            }
            // soft limit 不得高于 hard limit（防御异常/被篡改的探测值；两者都读，避免死字段）。
            if limit.rlim_cur > limit.rlim_max {
                return Err(RtStatus::ERR_ADMISSION_MEMORY);
            }
            Ok(limit.rlim_cur)
        }

        fn page_size(&mut self) -> u64 {
            self.page_size
        }

        fn mlock_all(&mut self, flags: u32) -> Result<(), RtStatus> {
            let flags_c = c_int::try_from(flags).map_err(|_| RtStatus::ERR_ADMISSION_MEMORY)?;
            // SAFETY: mlockall 只接受整型标志；无指针/所有权语义。
            let rc = unsafe { mlockall(flags_c) };
            if rc != 0 {
                return Err(RtStatus::ERR_ADMISSION_MEMORY);
            }
            Ok(())
        }

        fn pre_touch(&mut self, _region: RegionId, bytes: u64) -> Result<(), RtStatus> {
            // 容量探测：真实分配 + 逐页写触（NONRT，admission 一次）；不保留所有权。
            let len = usize::try_from(bytes).map_err(|_| RtStatus::ERR_ADMISSION_MEMORY)?;
            let mut probe = vec![0u8; len];
            let touched = pre_touch_bytes(probe.as_mut_slice(), self.page_size);
            let expected = pages_for(bytes, self.page_size)?;
            if touched != expected {
                return Err(RtStatus::ERR_ADMISSION_MEMORY);
            }
            Ok(())
        }
    }
}

pub use platform::PlatformMemory;

/// 当前构建是否有可用的真实平台内存后端（Linux 64-bit）。
pub const fn platform_memory_available() -> bool {
    cfg!(all(target_os = "linux", target_pointer_width = "64"))
}

// ---------------------------------------------------------------------------
// 准入校验与执行
// ---------------------------------------------------------------------------

/// 生产 RT 二进制构建门（core-v1 §9.3：admission 必须拒绝 debug build 的生产 RT 二进制）。
///
/// 生产入口 admit() 传入 cfg!(debug_assertions)/cfg!(test)；单测构建（cfg(test)）
/// 放行以允许离线模型测试，生产 debug 构建返回 ERR_UNSUPPORTED（不启 RT 主模式）。
pub const fn production_build_gate(debug_assertions: bool, is_test: bool) -> Result<(), RtStatus> {
    if debug_assertions && !is_test {
        Err(RtStatus::ERR_UNSUPPORTED)
    } else {
        Ok(())
    }
}

/// 纯校验（不接触后端）：env 范围/平台常量/plan-env 一致性/容量上限。
pub fn validate(plan: &PlanView<'_>, env: &AdmissionEnv) -> Result<(), RtStatus> {
    // A-ABI-4：端序只支持小端（aarch64/x86_64 均 LE）。
    if cfg!(not(target_endian = "little")) {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    // §3.2：runtime 只接受 48k/96k 与 64/128。
    if !SAMPLE_RATES_HZ.contains(&env.sample_rate_hz) {
        return Err(RtStatus::ERR_RATE);
    }
    if !BLOCK_SIZES.contains(&env.block_size) {
        return Err(RtStatus::ERR_BLOCK_SIZE);
    }
    // §7.4：4 worker 生产 / 2 worker 独立降级 profile；其它 = 调度准入拒绝。
    // T105/v1.1 I15：expected_workers（0 = 用 rt_workers）必须与 rt_workers 一致。
    if env.expected_workers != 0 && env.expected_workers != env.rt_workers {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    if env.rt_workers != RT_WORKERS && env.rt_workers != RT_WORKERS_FALLBACK {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    // T18 内存权限闸门（CAP_IPC_LOCK / RT 权限由 T01/T26 探测后传入）。
    if !env.rt_privilege_ok {
        return Err(RtStatus::ERR_ADMISSION_MEMORY);
    }
    // T19 探测结果透传闸门。
    if !env.clock_source_ok {
        return Err(RtStatus::ERR_ADMISSION_CLOCK);
    }
    // T45 探测结果透传闸门。
    if !env.cpu_set_ok {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    // plan 与 env 必须一致（不一致 = 计划过期/未按实际设备重设计）。
    if plan.header.sample_rate != env.sample_rate_hz
        || !SAMPLE_RATES_HZ.contains(&plan.header.sample_rate)
    {
        return Err(RtStatus::ERR_RATE);
    }
    if plan.header.block_size != env.block_size || !BLOCK_SIZES.contains(&plan.header.block_size) {
        return Err(RtStatus::ERR_BLOCK_SIZE);
    }
    // A-ABI-10：runtime 只收 f32 plan。
    if plan.header.precision != 0 {
        return Err(RtStatus::ERR_PRECISION);
    }
    // 2 核是独立 profile：plan 内 worker 数与 env 不符 = stale，必须重编译/重准入。
    if plan.header.workers != env.rt_workers {
        return Err(RtStatus::ERR_PLAN_STALE);
    }
    // 通道数（容量公式与 §3.1 上界）。
    if plan.header.n_in == 0
        || plan.header.n_out == 0
        || plan.header.n_in as usize > MAX_INPUT_CHANNELS
        || plan.header.n_out as usize > MAX_OUTPUT_CHANNELS
    {
        return Err(RtStatus::ERR_CHANNELS);
    }
    // 表规模上限（§3.1）。
    if plan.paths.len() > MAX_MATRIX_PATHS || plan.diags.len() > MAX_DIAG_FIRS {
        return Err(RtStatus::ERR_CAPACITY);
    }
    if plan.roles.len() > MAX_OUTPUT_ROLES {
        return Err(RtStatus::ERR_ROLES);
    }
    // §3.3：超过 16384 taps 仅设计工具 offline，runtime 准入拒绝。
    if plan.header.max_taps > TAPS_EXTENDED_MAX {
        return Err(RtStatus::ERR_TAPS);
    }
    Ok(())
}

/// 仅内存 admission（可注入后端；生产入口 admit() 的模型核心）。
///
/// 固定顺序：validate（纯校验）→ plan_memory（容量算术）→ 页/预算/RLIMIT 判定 →
/// mlockall(MCL_CURRENT|MCL_FUTURE) → 5 区域页预触 → AdmissionReport。
/// 任何一步 Err 立即返回（不构造报告、不继续后续步骤）。
pub fn admit_with<B: MemoryBackend>(
    plan: &PlanView<'_>,
    env: &AdmissionEnv,
    backend: &mut B,
) -> Result<AdmissionReport, RtStatus> {
    validate(plan, env)?;
    let memory = plan_memory(plan, env)?;

    // 页大小必须为 2 的幂且 >= MIN_PAGE_SIZE_BYTES（防御异常平台探测值）。
    let page = backend.page_size();
    if page < MIN_PAGE_SIZE_BYTES || !page.is_power_of_two() {
        return Err(RtStatus::ERR_ADMISSION_MEMORY);
    }
    let aligned_total = memory.aligned_total_bytes(page)?;

    // 声明预算（0 = 未声明，按计算需求自身页对齐值）。
    let declared_budget = if env.preallocated_bytes == 0 {
        aligned_total
    } else {
        env.preallocated_bytes
    };
    if declared_budget < aligned_total {
        return Err(RtStatus::ERR_CAPACITY);
    }

    // RLIMIT_MEMLOCK：env 已探测则用之，否则读后端；0 = 无锁页权限。
    let limit = if env.mlock_limit_bytes != 0 {
        env.mlock_limit_bytes
    } else {
        backend
            .read_memlock_limit()
            .map_err(|_| RtStatus::ERR_ADMISSION_MEMORY)?
    };
    if limit == 0 {
        return Err(RtStatus::ERR_ADMISSION_MEMORY);
    }
    if limit != RLIM_INFINITY && declared_budget > limit {
        return Err(RtStatus::ERR_ADMISSION_MEMORY);
    }

    // mlockall 只在全部静态判定通过后调用一次（MCL_CURRENT | MCL_FUTURE）。
    backend
        .mlock_all(MLOCK_ALL_FLAGS)
        .map_err(|_| RtStatus::ERR_ADMISSION_MEMORY)?;

    // 固定顺序页预触（失败即拒启；已发生的进程级 mlockall 由 T26 的 last-good/mute 策略处理）。
    for region in RegionId::ALL {
        let bytes = memory.aligned_region_bytes(region, page)?;
        backend
            .pre_touch(region, bytes)
            .map_err(|_| RtStatus::ERR_ADMISSION_MEMORY)?;
    }

    // v1.1/I15 + T45 O-03：sched 字段从 env 归并进报告（expected_workers 0 = 取 rt_workers；
    // ack_mask 按 expected 参数化，B-19 禁止硬写 0xF）。
    let expected_workers = if env.expected_workers == 0 {
        env.rt_workers
    } else {
        env.expected_workers
    };
    let ack_mask = if expected_workers == 0 || expected_workers >= 32 {
        0
    } else {
        (1u32 << expected_workers) - 1
    };
    Ok(AdmissionReport {
        plan_version: plan.header.plan_version,
        arena_bytes: memory.aligned_region_bytes(RegionId::Arena, page)?,
        mlocked_bytes: aligned_total,
        stack_bytes: memory.aligned_region_bytes(RegionId::Stacks, page)?,
        workers: env.rt_workers,
        graph_id: plan.header.graph_id,
        expected_workers,
        ack_mask,
        actual_cpu: env.actual_cpu,
        sched_priority: env.sched_priority,
        irq_dependency_mask: env.irq_dependency_mask,
    })
}

/// 生产 admission 入口（core-v1 §12.3 冻结签名）：构建门 + 平台后端 + 模型核心。
///
/// 失败即拒启（不产生报告、不启 RT 主模式）。单测/离线验证用 admit_with + MockMemory；
/// 非 Linux 开发平台 PlatformMemory::probe() 返回 ERR_UNSUPPORTED（fail closed）。
pub fn admit(plan: &PlanView<'_>, env: &AdmissionEnv) -> Result<AdmissionReport, RtStatus> {
    production_build_gate(cfg!(debug_assertions), cfg!(test))?;
    let mut backend = PlatformMemory::probe()?;
    admit_with(plan, env, &mut backend)
}

// ---------------------------------------------------------------------------
// 编译期断言（core-v1 §3/§9/§11.13/§13 与 T18 阈值）
// ---------------------------------------------------------------------------

const _: () = {
    // mlock/RLIMIT 冻结值（Linux ABI）。
    assert!(MCL_CURRENT == 0x1 && MCL_FUTURE == 0x2 && MLOCK_ALL_FLAGS == 0x3);
    assert!(mlock_flags_complete(MCL_CURRENT | MCL_FUTURE));
    assert!(!mlock_flags_complete(MCL_CURRENT));
    assert!(RLIMIT_MEMLOCK_RESOURCE == 8 && RLIM_INFINITY == u64::MAX);
    // 栈/线程预算。
    assert!(STACK_BYTES_PER_THREAD == 524_288);
    assert!(precreated_threads(4) == 8 && precreated_threads(2) == 6);
    assert!(precreated_stacks_bytes(4) == 8 * 524_288);
    assert!(precreated_stacks_bytes(2) == 6 * 524_288);
    // §11.13 控制面结构合计（128+64+8×64+1024×64+4×128+128+64+4×64+4×64+6×4×32×4）。
    assert!(control_plane_structs_bytes() == 70_528);
    assert!(control_plane_total_bytes() == 70_528 + 65_536);
    // 图容量公式向量。
    assert!(graph_scratch_bytes(128, 4) == 16_384);
    assert!(graph_scratch_bytes(64, 4) == 8_192);
    assert!(path_history_bytes(4096, 128) == 32 * 256 * 8);
    assert!(path_history_bytes(4096, 64) == 64 * 128 * 8);
    assert!(path_history_bytes(0, 128) == 0);
    // T18 类型无 Drop（core-v1 §13：RT 可见/跨边界类型）。
    assert!(!core::mem::needs_drop::<AdmissionReport>());
    assert!(!core::mem::needs_drop::<AdmissionEnv>());
    assert!(!core::mem::needs_drop::<MemoryPlan>());
    assert!(!core::mem::needs_drop::<RegionId>());
    assert!(!core::mem::needs_drop::<RtStatus>());
};

#[cfg(test)]
mod admission_tests {
    include!("../tests/T18/admission_models.rs");
}
