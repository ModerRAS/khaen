//! [NONRT] 启动期 RT 内核应用（F6 修复；task-216）—— mlockall / SCHED_FIFO / CPU 亲和。
//!
//! 背景（T71 板窗 F6）：host 的 T45 SchedPlan 只计划、不写内核
//! （rt.apply_affinity=false -> kernel_applied=false），4 个 worker 无界忙等占满 CPU，
//! I/O 主循环（1 period = 2.67 ms 预算）被 CFS/自旋拖过预算 -> 设备侧持续 underrun。
//!
//! 本模块只做**启动期一次**的真实内核写入（core-v1 §10.1 F-17：改亲和/优先级只允许在
//! admission 前一次设置；RT 块循环内不再调用）：
//! 1. mlockall(MCL_CURRENT|MCL_FUTURE)（进程级；后续映射也锁）；
//! 2. 主/I-O owner 线程 SCHED_FIFO，优先级取 T45 计划 io_priority（严格高于 worker）；
//! 3. worker 线程 SCHED_FIFO 取计划 dsp_priority，并绑定计划 affinity[k].cpu_index；
//! 4. 每次写入后 sched_getscheduler/sched_getparam/sched_getaffinity **读回校验**，
//!    不是"调用过就算成功"；
//! 5. 任一失败返回带 stage/errno/cpu/prio 的 ApplyError —— 调用方 fail closed
//!    （报错拒绝启动，不 warn 后自称 RT）。
//!
//! 为什么主线程必须避开 DSP_CPUS：主线程（I/O owner）以更高 FIFO 优先级自旋等 worker
//! 完成；若它与某个 worker 同核，就会把该 worker 饿死（wait-for 死锁）。因此
//! prepare_targets 强制 main.cpu 不在任何 worker CPU 上；没有可用非 DSP CPU 时直接拒启
//! （不猜、不冒险）。
//!
//! 边界（明确不做）：
//! - 不写 IRQ 线程亲和/优先级：host 进程不拥有内核 IRQ 线程；计划 irq_priority 只记录
//!   （报告 irq_priority_applied=false），IRQ 迁移仍由板端 irq-affinity-pin 负责；
//! - 不设置 rt_runtime/isolcpus/nohz_full/governor（core-v1 §15.1 明确禁止自动 rt_runtime=-1）；
//! - 不打开设备、不读配置、不分配 RT 内存（纯 startup 辅助）。
//!
//! 平台：真实写入只编译在 cfg(target_os = "linux")（直接 extern "C" FFI，无新增依赖）；
//! 非 Linux target 的 LinuxKernel 一律返回 ENOSYS —— 上层 apply_affinity=true 时 fail
//! closed。本机 Windows 的真实 syscall 未执行（NOT_RUN），由 Linux target cargo check +
//! MockKernel 单测覆盖参数/顺序/失败语义。

use core::fmt;

use khaen_rt_host::scheduler::{SchedPlan, CPU_UNUSED, DSP_CPU_MAX, MAX_CPU, WORKER_AFFINITY_COUNT};

/// 本 target 是否可执行真实 Linux 内核写入（仅 cfg(target_os = "linux")）。
pub const LINUX_TARGET: bool = cfg!(target_os = "linux");

// ---------------------------------------------------------------------------
// 常量（Linux UAPI；本模块不是 RT-MODULE，只在启动期调用一次）
// ---------------------------------------------------------------------------

/// mlockall 标志：锁当前已映射页（Linux MCL_CURRENT = 0x1）。
pub const MCL_CURRENT: i32 = 0x1;
/// mlockall 标志：锁未来映射页（Linux MCL_FUTURE = 0x2）。
pub const MCL_FUTURE: i32 = 0x2;
/// T18/T26 要求的 mlockall 组合：MCL_CURRENT | MCL_FUTURE（缺一不可）。
pub const MLOCK_ALL_FLAGS: i32 = MCL_CURRENT | MCL_FUTURE;
/// SCHED_FIFO 策略号（Linux sched.h：SCHED_FIFO = 1）。
pub const SCHED_FIFO_POLICY: i32 = 1;
/// ENOSYS：当前 target 不支持真实写入（非 Linux）。
pub const ENOSYS_LINUX_ONLY: i32 = 38;
/// 读回校验哨兵：策略不是 SCHED_FIFO（不是 errno 值，仅本模块报告用）。
pub const ERR_VERIFY_POLICY: i32 = 0x7F01;
/// 读回校验哨兵：优先级读回与计划不符。
pub const ERR_VERIFY_PRIORITY: i32 = 0x7F02;
/// 读回校验哨兵：亲和读回不含目标 CPU。
pub const ERR_VERIFY_AFFINITY: i32 = 0x7F03;
/// 输入错误哨兵：目标参数不成立（非 errno）。
pub const ERR_INPUTS: i32 = 0x7F10;

/// 全部 CPU 位（MAX_CPU = 64 时的 u64 全 1；防 1u64 << 64 溢出）。
const fn all_cpu_bits() -> u64 {
    if MAX_CPU >= 64 {
        u64::MAX
    } else {
        (1u64 << MAX_CPU) - 1
    }
}

// ---------------------------------------------------------------------------
// 错误：stage + errno + 上下文（fail-closed 的机械依据）
// ---------------------------------------------------------------------------

/// 失败阶段（报错必须能定位；禁止只返回 "失败"）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyStage {
    /// 读当前允许 CPU 集合（派生目标前）。
    Inputs = 1,
    /// mlockall(MCL_CURRENT|MCL_FUTURE)。
    Mlockall = 2,
    /// 主/I-O 线程 CPU 亲和。
    MainAffinity = 3,
    /// 主/I-O 线程 SCHED_FIFO。
    MainSched = 4,
    /// worker CPU 亲和。
    WorkerAffinity = 5,
    /// worker SCHED_FIFO。
    WorkerSched = 6,
    /// 写后读回校验不符。
    Verify = 7,
    /// 当前 target 无真实写入能力（非 Linux）。
    Unsupported = 8,
}

impl ApplyStage {
    /// 稳定名字（JSON/日志/测试用）。
    pub const fn name(self) -> &'static str {
        match self {
            ApplyStage::Inputs => "inputs",
            ApplyStage::Mlockall => "mlockall",
            ApplyStage::MainAffinity => "main_affinity",
            ApplyStage::MainSched => "main_sched",
            ApplyStage::WorkerAffinity => "worker_affinity",
            ApplyStage::WorkerSched => "worker_sched",
            ApplyStage::Verify => "verify",
            ApplyStage::Unsupported => "unsupported",
        }
    }
}

/// 内核应用失败（Copy；可经 AtomicU32 从 worker 线程带回父线程）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyError {
    /// 失败阶段。
    pub stage: ApplyStage,
    /// errno（正数；或本模块 ERR_VERIFY_* / ERR_INPUTS 哨兵）。
    pub errno: i32,
    /// 相关 worker 编号（-1 = 主线程/不适用）。
    pub worker: i16,
    /// 相关 CPU（CPU_UNUSED = 不适用）。
    pub cpu: u16,
    /// 相关 FIFO 优先级（0 = 不适用）。
    pub priority: u8,
}

impl ApplyError {
    /// 构造（worker=-1/cpu=UNUSED/priority=0）。
    pub const fn new(stage: ApplyStage, errno: i32) -> Self {
        Self {
            stage,
            errno,
            worker: -1,
            cpu: CPU_UNUSED,
            priority: 0,
        }
    }

    /// 附加 worker 编号。
    pub const fn with_worker(mut self, worker: i16) -> Self {
        self.worker = worker;
        self
    }

    /// 附加 CPU 与优先级。
    pub const fn with_target(mut self, cpu: u16, priority: u8) -> Self {
        self.cpu = cpu;
        self.priority = priority;
        self
    }

    /// 编码成 u32（stage 高 8 位 + errno 低 16 位；worker 槽由父线程按下标保存）。
    pub const fn encode(&self) -> u32 {
        ((self.stage as u32) << 24) | ((self.errno as u32) & 0xFFFF)
    }

    /// 从 encode 解回（worker/cpu/prio 由调用方补）。
    pub const fn decode(code: u32) -> Self {
        let stage = match ((code >> 24) & 0xFF) as u8 {
            1 => ApplyStage::Inputs,
            2 => ApplyStage::Mlockall,
            3 => ApplyStage::MainAffinity,
            4 => ApplyStage::MainSched,
            5 => ApplyStage::WorkerAffinity,
            6 => ApplyStage::WorkerSched,
            7 => ApplyStage::Verify,
            _ => ApplyStage::Unsupported,
        };
        Self {
            stage,
            errno: (code & 0xFFFF) as i32,
            worker: -1,
            cpu: CPU_UNUSED,
            priority: 0,
        }
    }

    /// errno/哨兵名字（排障用；未知只报数值）。
    pub const fn errno_name(errno: i32) -> &'static str {
        match errno {
            0 => "OK",
            1 => "EPERM",
            3 => "ESRCH",
            11 => "EAGAIN",
            12 => "ENOMEM",
            22 => "EINVAL",
            38 => "ENOSYS",
            ERR_VERIFY_POLICY => "ERR_VERIFY_POLICY",
            ERR_VERIFY_PRIORITY => "ERR_VERIFY_PRIORITY",
            ERR_VERIFY_AFFINITY => "ERR_VERIFY_AFFINITY",
            ERR_INPUTS => "ERR_INPUTS",
            _ => "ERRNO",
        }
    }
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stage={} errno={}({})",
            self.stage.name(),
            self.errno,
            Self::errno_name(self.errno)
        )?;
        if self.worker >= 0 {
            write!(f, " worker={}", self.worker)?;
        }
        if self.cpu != CPU_UNUSED {
            write!(f, " cpu={}", self.cpu)?;
        }
        if self.priority != 0 {
            write!(f, " fifo_priority={}", self.priority)?;
        }
        Ok(())
    }
}

impl std::error::Error for ApplyError {}

// ---------------------------------------------------------------------------
// 目标（由 T45 SchedPlan 派生）
// ---------------------------------------------------------------------------

/// 单个线程的 RT 目标：绑哪个 CPU（CPU_UNUSED = 不绑）、用哪级 FIFO 优先级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadRtTarget {
    /// 目标 CPU（CPU_UNUSED = 不设置亲和）。
    pub cpu: u16,
    /// FIFO 优先级（1..=99）。
    pub priority: u8,
}

impl ThreadRtTarget {
    /// 构造。
    pub const fn new(cpu: u16, priority: u8) -> Self {
        Self { cpu, priority }
    }
}

/// 从 T45 计划派生的完整 RT 应用目标（启动期一次）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtTargets {
    /// worker 数（2/4）。
    pub workers: u8,
    /// 逐 worker 目标（前 workers 条有效）。
    pub worker: [ThreadRtTarget; DSP_CPU_MAX],
    /// 主/I-O owner 线程目标。
    pub main: ThreadRtTarget,
    /// 计划 IRQ 线程优先级（仅记录；host 不拥有 IRQ 线程）。
    pub irq_priority: u8,
}

impl RtTargets {
    /// 自校验：worker 数/CPU 合法且互斥、主线程不与 worker 同核、优先级排序。
    pub fn validate(&self) -> Result<(), ApplyError> {
        if self.workers as usize != 2 && self.workers as usize != WORKER_AFFINITY_COUNT {
            return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS));
        }
        let mut seen = [CPU_UNUSED; DSP_CPU_MAX];
        let mut k = 0usize;
        while k < self.workers as usize {
            let target = self.worker[k];
            if target.cpu == CPU_UNUSED || target.cpu as usize >= MAX_CPU {
                return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                    .with_worker(k as i16)
                    .with_target(target.cpu, target.priority));
            }
            if target.priority < 1 || target.priority > 99 {
                return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                    .with_worker(k as i16)
                    .with_target(target.cpu, target.priority));
            }
            let mut j = 0usize;
            while j < k {
                if seen[j] == target.cpu {
                    return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                        .with_worker(k as i16)
                        .with_target(target.cpu, target.priority));
                }
                j += 1;
            }
            seen[k] = target.cpu;
            k += 1;
        }
        if self.main.cpu == CPU_UNUSED || self.main.cpu as usize >= MAX_CPU {
            return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                .with_target(self.main.cpu, self.main.priority));
        }
        if self.main.priority < 1 || self.main.priority > 99 {
            return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                .with_target(self.main.cpu, self.main.priority));
        }
        // 主线程（更高 FIFO）必须避开所有 worker CPU：否则会饿死该 worker（wait-for 死锁）。
        let mut k = 0usize;
        while k < self.workers as usize {
            if self.worker[k].cpu == self.main.cpu {
                return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                    .with_target(self.main.cpu, self.main.priority));
            }
            k += 1;
        }
        // I/O 优先级必须严格高于 worker（F6：I/O 主循环不得被 DSP 忙等压住）。
        let mut k = 0usize;
        while k < self.workers as usize {
            if self.worker[k].priority >= self.main.priority {
                return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                    .with_target(self.worker[k].cpu, self.worker[k].priority));
            }
            k += 1;
        }
        Ok(())
    }

    /// worker 目标数组（供 BlockWorkers::start_rt；未用槽 None）。
    pub fn worker_targets(&self) -> [Option<ThreadRtTarget>; WORKER_AFFINITY_COUNT] {
        let mut out = [None; WORKER_AFFINITY_COUNT];
        let mut k = 0usize;
        while k < self.workers as usize && k < WORKER_AFFINITY_COUNT {
            out[k] = Some(self.worker[k]);
            k += 1;
        }
        out
    }
}

/// RT 应用配置（来自 [rt] 段；只读值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtApplyConfig {
    /// rt.apply_affinity（false = 只计划，不写内核）。
    pub apply_affinity: bool,
    /// rt.mlockall（apply_affinity=true 时是否 mlockall；默认 true）。
    pub mlockall: bool,
    /// rt.io_cpu（CPU_UNUSED = 自动选第一个非 DSP CPU）。
    pub io_cpu: u16,
}

// ---------------------------------------------------------------------------
// 后端抽象（真实 Linux FFI / 测试 Mock）
// ---------------------------------------------------------------------------

/// 内核写入后端（启动期专用；每个方法作用在**调用线程**上）。
pub trait KernelBackend {
    /// 是否真实内核后端（非 Linux 的占位实现 = false）。
    fn is_real(&self) -> bool;
    /// mlockall(flags)；失败返回 errno。
    fn mlockall(&mut self, flags: i32) -> Result<(), i32>;
    /// 当前线程设 SCHED_FIFO（priority 1..=99）。
    fn sched_set_fifo(&mut self, priority: u8) -> Result<(), i32>;
    /// 当前线程调度策略（读回校验用；SCHED_FIFO=1）。
    fn sched_policy(&mut self) -> Result<i32, i32>;
    /// 当前线程 FIFO 优先级（读回校验用；非 RT 返回 0）。
    fn sched_priority(&mut self) -> Result<u8, i32>;
    /// 当前线程绑单 CPU。
    fn sched_set_cpu(&mut self, cpu: u16) -> Result<(), i32>;
    /// 当前线程允许 CPU 位图（bit k = CPU k）。
    fn sched_cpu_mask(&mut self) -> Result<u64, i32>;
}

/// 真实后端句柄（Linux 上是真 FFI；非 Linux 所有方法返回 ENOSYS）。
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxKernel {
    _private: (),
}

/// 构造真实后端句柄（无副作用；非 Linux 上调用其方法才失败）。
pub fn linux_kernel() -> LinuxKernel {
    LinuxKernel { _private: () }
}

#[cfg(target_os = "linux")]
mod linux_ffi {
    /// Linux struct sched_param（只含 sched_priority 足够 FIFO）。
    #[repr(C)]
    pub(super) struct SchedParam {
        pub sched_priority: i32,
    }

    extern "C" {
        pub(super) fn mlockall(flags: i32) -> i32;
        pub(super) fn sched_setscheduler(pid: i32, policy: i32, param: *const SchedParam) -> i32;
        pub(super) fn sched_getscheduler(pid: i32) -> i32;
        pub(super) fn sched_getparam(pid: i32, param: *mut SchedParam) -> i32;
        pub(super) fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u64) -> i32;
        pub(super) fn sched_getaffinity(pid: i32, cpusetsize: usize, mask: *mut u64) -> i32;
    }

    /// 最近一次 syscall 的 errno（正数；取不到 = 0）。
    pub(super) fn last_errno() -> i32 {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(value) if value < 0 => -value,
            Some(value) => value,
            None => 0,
        }
    }
}

#[cfg(target_os = "linux")]
impl KernelBackend for LinuxKernel {
    fn is_real(&self) -> bool {
        true
    }

    fn mlockall(&mut self, flags: i32) -> Result<(), i32> {
        // SAFETY：mlockall 只接受整型标志；无指针/所有权语义。
        let rc = unsafe { linux_ffi::mlockall(flags) };
        if rc == 0 {
            Ok(())
        } else {
            Err(linux_ffi::last_errno())
        }
    }

    fn sched_set_fifo(&mut self, priority: u8) -> Result<(), i32> {
        let param = linux_ffi::SchedParam {
            sched_priority: priority as i32,
        };
        // SAFETY：param 是栈上有效 #[repr(C)] 结构；pid 0 = 调用线程。
        let rc = unsafe { linux_ffi::sched_setscheduler(0, SCHED_FIFO_POLICY, &param) };
        if rc == 0 {
            Ok(())
        } else {
            Err(linux_ffi::last_errno())
        }
    }

    fn sched_policy(&mut self) -> Result<i32, i32> {
        // SAFETY：sched_getscheduler 只读，无指针；pid 0 = 调用线程。
        let policy = unsafe { linux_ffi::sched_getscheduler(0) };
        if policy < 0 {
            Err(linux_ffi::last_errno())
        } else {
            Ok(policy)
        }
    }

    fn sched_priority(&mut self) -> Result<u8, i32> {
        let mut param = linux_ffi::SchedParam { sched_priority: 0 };
        // SAFETY：param 是栈上有效可写 #[repr(C)] 结构。
        let rc = unsafe { linux_ffi::sched_getparam(0, &mut param) };
        if rc == 0 {
            Ok(param.sched_priority.clamp(0, 99) as u8)
        } else {
            Err(linux_ffi::last_errno())
        }
    }

    fn sched_set_cpu(&mut self, cpu: u16) -> Result<(), i32> {
        if cpu as usize >= MAX_CPU {
            return Err(ERR_INPUTS);
        }
        let mask: u64 = 1u64 << cpu;
        // SAFETY：mask 是栈上有效 u64；cpusetsize 与 mask 一致；pid 0 = 调用线程。
        let rc = unsafe { linux_ffi::sched_setaffinity(0, core::mem::size_of::<u64>(), &mask) };
        if rc == 0 {
            Ok(())
        } else {
            Err(linux_ffi::last_errno())
        }
    }

    fn sched_cpu_mask(&mut self) -> Result<u64, i32> {
        let mut mask: u64 = 0;
        // SAFETY：mask 是栈上有效可写 u64；cpusetsize 与 mask 一致。
        let rc = unsafe { linux_ffi::sched_getaffinity(0, core::mem::size_of::<u64>(), &mut mask) };
        if rc == 0 {
            Ok(mask & all_cpu_bits())
        } else {
            Err(linux_ffi::last_errno())
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl KernelBackend for LinuxKernel {
    fn is_real(&self) -> bool {
        false
    }

    fn mlockall(&mut self, _flags: i32) -> Result<(), i32> {
        Err(ENOSYS_LINUX_ONLY)
    }

    fn sched_set_fifo(&mut self, _priority: u8) -> Result<(), i32> {
        Err(ENOSYS_LINUX_ONLY)
    }

    fn sched_policy(&mut self) -> Result<i32, i32> {
        Err(ENOSYS_LINUX_ONLY)
    }

    fn sched_priority(&mut self) -> Result<u8, i32> {
        Err(ENOSYS_LINUX_ONLY)
    }

    fn sched_set_cpu(&mut self, _cpu: u16) -> Result<(), i32> {
        Err(ENOSYS_LINUX_ONLY)
    }

    fn sched_cpu_mask(&mut self) -> Result<u64, i32> {
        Err(ENOSYS_LINUX_ONLY)
    }
}

// ---------------------------------------------------------------------------
// 应用（启动期一次；每个函数返回详细错误）
// ---------------------------------------------------------------------------

/// 应用角色（决定错误 stage 与报告语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyRole {
    /// 主/I-O owner 线程（4 worker 块循环的 master）。
    Main,
    /// 常驻 DSP worker。
    Worker,
}

impl ApplyRole {
    /// 亲和失败 stage。
    const fn affinity_stage(self) -> ApplyStage {
        match self {
            ApplyRole::Main => ApplyStage::MainAffinity,
            ApplyRole::Worker => ApplyStage::WorkerAffinity,
        }
    }

    /// 调度失败 stage。
    const fn sched_stage(self) -> ApplyStage {
        match self {
            ApplyRole::Main => ApplyStage::MainSched,
            ApplyRole::Worker => ApplyStage::WorkerSched,
        }
    }
}

/// 读当前线程允许 CPU 集合（派生目标/校验前）。
pub fn allowed_cpu_mask(backend: &mut dyn KernelBackend) -> Result<u64, ApplyError> {
    backend
        .sched_cpu_mask()
        .map_err(|errno| ApplyError::new(ApplyStage::Inputs, errno))
}

/// 进程级 mlockall（只调用一次；成功 = 已锁 current+future）。
pub fn apply_mlockall(backend: &mut dyn KernelBackend, flags: i32) -> Result<(), ApplyError> {
    backend
        .mlockall(flags)
        .map_err(|errno| ApplyError::new(ApplyStage::Mlockall, errno))
}

/// 由 T45 计划 + 实测允许 CPU 集合派生 RT 目标（纯函数，不写内核）。
///
/// 失败语义（ERR_INPUTS 或 errno）：worker 数非 2/4、worker CPU 未配置/越界/不在允许集合/
/// 重复、主线程 CPU 与 worker 同核、允许集合里没有非 DSP CPU、I/O 优先级不高于 worker。
pub fn prepare_targets(
    sched: &SchedPlan,
    config: RtApplyConfig,
    allowed_mask: u64,
) -> Result<RtTargets, ApplyError> {
    if !config.apply_affinity {
        return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS));
    }
    let workers = sched.workers;
    if workers as usize != 2 && workers as usize != WORKER_AFFINITY_COUNT {
        return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS));
    }
    let allowed = allowed_mask & all_cpu_bits();
    if allowed == 0 {
        return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS));
    }
    let mut dsp_mask = 0u64;
    let mut worker = [ThreadRtTarget::new(CPU_UNUSED, 0); DSP_CPU_MAX];
    let mut k = 0usize;
    while k < workers as usize {
        let entry = sched.affinity[k];
        let cpu = entry.cpu_index;
        if cpu == CPU_UNUSED || cpu as usize >= MAX_CPU {
            return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                .with_worker(k as i16)
                .with_target(cpu, entry.sched_priority));
        }
        if allowed & (1u64 << cpu) == 0 {
            return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS)
                .with_worker(k as i16)
                .with_target(cpu, entry.sched_priority));
        }
        worker[k] = ThreadRtTarget::new(cpu, entry.sched_priority);
        dsp_mask |= 1u64 << cpu;
        k += 1;
    }
    // 主/I-O 线程：显式 io_cpu 或自动选 allowed 集合里第一个非 DSP CPU。
    // 必须避开 DSP CPU（否则 master 更高 FIFO 自旋会饿死同核 worker -> 死锁）。
    let main_cpu = if config.io_cpu == CPU_UNUSED {
        let free = allowed & !dsp_mask;
        if free == 0 {
            return Err(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS));
        }
        free.trailing_zeros() as u16
    } else {
        let cpu = config.io_cpu;
        if cpu as usize >= MAX_CPU || allowed & (1u64 << cpu) == 0 || dsp_mask & (1u64 << cpu) != 0
        {
            return Err(
                ApplyError::new(ApplyStage::Inputs, ERR_INPUTS).with_target(cpu, sched.io_priority)
            );
        }
        cpu
    };
    let targets = RtTargets {
        workers,
        worker,
        main: ThreadRtTarget::new(main_cpu, sched.io_priority),
        irq_priority: sched.irq_priority,
    };
    targets.validate()?;
    Ok(targets)
}

/// 对**当前线程**写 RT：先绑核（避免成为 FIFO 后再迁移的毛刺），再设 SCHED_FIFO，
/// 最后读回校验（策略/优先级/亲和）。
pub fn apply_thread(
    backend: &mut dyn KernelBackend,
    target: ThreadRtTarget,
    role: ApplyRole,
) -> Result<(), ApplyError> {
    if !backend.is_real() {
        return Err(ApplyError::new(ApplyStage::Unsupported, ENOSYS_LINUX_ONLY)
            .with_target(target.cpu, target.priority));
    }
    if target.cpu != CPU_UNUSED {
        backend.sched_set_cpu(target.cpu).map_err(|errno| {
            ApplyError::new(role.affinity_stage(), errno).with_target(target.cpu, target.priority)
        })?;
        let mask = backend.sched_cpu_mask().map_err(|errno| {
            ApplyError::new(ApplyStage::Verify, errno).with_target(target.cpu, target.priority)
        })?;
        if target.cpu as usize >= MAX_CPU || mask & (1u64 << target.cpu) == 0 {
            return Err(ApplyError::new(ApplyStage::Verify, ERR_VERIFY_AFFINITY)
                .with_target(target.cpu, target.priority));
        }
    }
    backend.sched_set_fifo(target.priority).map_err(|errno| {
        ApplyError::new(role.sched_stage(), errno).with_target(target.cpu, target.priority)
    })?;
    let policy = backend.sched_policy().map_err(|errno| {
        ApplyError::new(ApplyStage::Verify, errno).with_target(target.cpu, target.priority)
    })?;
    if policy != SCHED_FIFO_POLICY {
        return Err(ApplyError::new(ApplyStage::Verify, ERR_VERIFY_POLICY)
            .with_target(target.cpu, target.priority));
    }
    let priority = backend.sched_priority().map_err(|errno| {
        ApplyError::new(ApplyStage::Verify, errno).with_target(target.cpu, target.priority)
    })?;
    if priority != target.priority {
        return Err(ApplyError::new(ApplyStage::Verify, ERR_VERIFY_PRIORITY)
            .with_target(target.cpu, target.priority));
    }
    Ok(())
}

/// worker 线程入口专用：用真实 Linux 后端对当前线程写 RT（非 Linux fail closed）。
pub fn apply_worker_thread(target: ThreadRtTarget) -> Result<(), ApplyError> {
    let mut backend = linux_kernel();
    apply_thread(&mut backend, target, ApplyRole::Worker)
}

// ---------------------------------------------------------------------------
// 报告（JSON；kernel_applied 只能由"全部读回校验通过"置 true）
// ---------------------------------------------------------------------------

/// 单 worker 应用结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedThread {
    /// 计划 CPU。
    pub cpu: u16,
    /// 计划 FIFO 优先级。
    pub priority: u8,
    /// 是否已真实写入并读回校验通过。
    pub applied: bool,
}

/// 启动期 RT 应用报告（写入 sched JSON 的 kernel_apply 段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtApplyReport {
    /// 是否请求应用（rt.apply_affinity）。
    pub requested: bool,
    /// 后端名字（linux / non-linux / none）。
    pub backend: &'static str,
    /// 是否全部真实写入并通过读回校验（唯一可以声称 RT 的依据）。
    pub kernel_applied: bool,
    /// mlockall 是否成功。
    pub mlockall: bool,
    /// mlockall 标志（0 = 未调用）。
    pub mlock_flags: i32,
    /// 主/I-O 线程 CPU（CPU_UNUSED = 未绑）。
    pub main_cpu: u16,
    /// 主/I-O 线程 FIFO 优先级。
    pub main_priority: u8,
    /// 已应用 worker 数。
    pub workers_applied: u8,
    /// 计划 worker 数。
    pub worker_count: u8,
    /// 逐 worker 结果。
    pub workers: [AppliedThread; DSP_CPU_MAX],
    /// 计划 IRQ 线程优先级（仅记录）。
    pub irq_priority: u8,
    /// 原因（applied / rt.apply_affinity=false / ...）。
    pub reason: &'static str,
}

impl RtApplyReport {
    /// 未请求应用时的报告（rt.apply_affinity=false；必须诚实报告 kernel_applied=false）。
    pub fn disabled(irq_priority: u8) -> Self {
        Self {
            requested: false,
            backend: "none",
            kernel_applied: false,
            mlockall: false,
            mlock_flags: 0,
            main_cpu: CPU_UNUSED,
            main_priority: 0,
            workers_applied: 0,
            worker_count: 0,
            workers: [AppliedThread {
                cpu: CPU_UNUSED,
                priority: 0,
                applied: false,
            }; DSP_CPU_MAX],
            irq_priority,
            reason: "rt.apply_affinity=false",
        }
    }

    /// 由已发生事实构造报告（不作弊：只有全部 applied 才 kernel_applied=true）。
    pub fn from_applied(
        targets: &RtTargets,
        mlockall: bool,
        mlock_flags: i32,
        main_applied: bool,
        workers_applied: u8,
    ) -> Self {
        let mut workers = [AppliedThread {
            cpu: CPU_UNUSED,
            priority: 0,
            applied: false,
        }; DSP_CPU_MAX];
        let mut k = 0usize;
        while k < targets.workers as usize && k < DSP_CPU_MAX {
            workers[k] = AppliedThread {
                cpu: targets.worker[k].cpu,
                priority: targets.worker[k].priority,
                applied: workers_applied > k as u8,
            };
            k += 1;
        }
        let kernel_applied = mlockall && main_applied && workers_applied == targets.workers;
        Self {
            requested: true,
            backend: if LINUX_TARGET { "linux" } else { "non-linux" },
            kernel_applied,
            mlockall,
            mlock_flags,
            main_cpu: targets.main.cpu,
            main_priority: targets.main.priority,
            workers_applied,
            worker_count: targets.workers,
            workers,
            irq_priority: targets.irq_priority,
            reason: if kernel_applied { "applied" } else { "partial" },
        }
    }

    /// 序列化为 JSON 对象（kernel_apply 段；值全部受控，无需转义）。
    pub fn to_json(&self) -> String {
        let main_cpu = if self.main_cpu == CPU_UNUSED {
            "null".to_string()
        } else {
            self.main_cpu.to_string()
        };
        let sched_policy = if self.kernel_applied {
            "SCHED_FIFO"
        } else {
            "none"
        };
        let mut workers = String::with_capacity(96);
        let mut k = 0usize;
        while k < self.worker_count as usize && k < DSP_CPU_MAX {
            if k != 0 {
                workers.push(',');
            }
            let entry = self.workers[k];
            let cpu = if entry.cpu == CPU_UNUSED {
                "null".to_string()
            } else {
                entry.cpu.to_string()
            };
            let _ = core::fmt::Write::write_fmt(
                &mut workers,
                format_args!(
                    "{{\"cpu\":{},\"priority\":{},\"applied\":{}}}",
                    cpu, entry.priority, entry.applied
                ),
            );
            k += 1;
        }
        format!(
            "{{\"requested\":{},\"backend\":\"{}\",\"kernel_applied\":{},\"sched_policy\":\"{}\",\"mlockall\":{},\"mlock_flags\":\"{}\",\"main_cpu\":{},\"main_priority\":{},\"workers_applied\":{},\"workers\":[{}],\"irq_priority_planned\":{},\"irq_priority_applied\":false,\"reason\":\"{}\"}}",
            self.requested,
            self.backend,
            self.kernel_applied,
            sched_policy,
            self.mlockall,
            if self.mlock_flags == 0 {
                "none".to_string()
            } else if self.mlock_flags == MLOCK_ALL_FLAGS {
                "MCL_CURRENT|MCL_FUTURE".to_string()
            } else {
                format!("0x{:x}", self.mlock_flags)
            },
            main_cpu,
            self.main_priority,
            self.workers_applied,
            workers,
            self.irq_priority,
            self.reason
        )
    }
}

// ---------------------------------------------------------------------------
// 单元测试（Mock 后端：参数/顺序/失败/读回校验；非 Linux target fail-closed）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use khaen_rt_host::scheduler::{
        plan as sched_plan, SchedRequest, TopologySnapshot, CPU_UNKNOWN_CORE,
    };

    fn plan_4worker() -> SchedPlan {
        let mut online = 0u64;
        let mut cores = [CPU_UNKNOWN_CORE; MAX_CPU];
        let mut cpu = 0u16;
        while cpu < 8 {
            online |= 1u64 << cpu;
            cores[cpu as usize] = cpu as i8;
            cpu += 1;
        }
        let topo = TopologySnapshot::new()
            .with_online_mask(online)
            .with_possible_mask(online)
            .with_allowed_mask(online)
            .with_isolated_mask(0)
            .with_nohz_full_mask(0)
            .with_core_map(cores);
        let mut req = SchedRequest::new()
            .with_workers(4)
            .with_priorities(70, 80, 90)
            .with_tick(1_000_000_000, 2)
            .with_calibration(1_000_000, 4096);
        let mut index = 0usize;
        while index < 4 {
            req = req.with_dsp_cpu(index, 2 + index as u16);
            index += 1;
        }
        sched_plan(&req, &topo).expect("synthetic 4-worker plan")
    }

    fn config() -> RtApplyConfig {
        RtApplyConfig {
            apply_affinity: true,
            mlockall: true,
            io_cpu: CPU_UNUSED,
        }
    }

    #[derive(Default)]
    struct MockKernel {
        fail_mlockall: Option<i32>,
        fail_set_cpu: Option<i32>,
        fail_set_fifo: Option<i32>,
        fail_mask: Option<i32>,
        /// 强制读回策略（None = 默认 SCHED_FIFO）。
        forced_policy: Option<i32>,
        /// 强制读回优先级（None = 返回实际写入值）。
        forced_priority: Option<u8>,
        /// 强制读回亲和掩码（None = 返回实际写入掩码）。
        forced_mask: Option<u64>,
        mask: u64,
        priority: u8,
        calls: Vec<String>,
    }

    impl MockKernel {
        fn new(mask: u64) -> Self {
            Self {
                mask,
                ..Self::default()
            }
        }
    }

    impl KernelBackend for MockKernel {
        fn is_real(&self) -> bool {
            true
        }

        fn mlockall(&mut self, flags: i32) -> Result<(), i32> {
            self.calls.push(format!("mlockall:{flags}"));
            match self.fail_mlockall {
                Some(errno) => Err(errno),
                None => Ok(()),
            }
        }

        fn sched_set_fifo(&mut self, priority: u8) -> Result<(), i32> {
            self.calls.push(format!("fifo:{priority}"));
            match self.fail_set_fifo {
                Some(errno) => Err(errno),
                None => {
                    self.priority = priority;
                    Ok(())
                }
            }
        }

        fn sched_policy(&mut self) -> Result<i32, i32> {
            self.calls.push("get_policy".to_string());
            Ok(self.forced_policy.unwrap_or(SCHED_FIFO_POLICY))
        }

        fn sched_priority(&mut self) -> Result<u8, i32> {
            self.calls.push("get_priority".to_string());
            Ok(self.forced_priority.unwrap_or(self.priority))
        }

        fn sched_set_cpu(&mut self, cpu: u16) -> Result<(), i32> {
            self.calls.push(format!("affinity:{cpu}"));
            match self.fail_set_cpu {
                Some(errno) => Err(errno),
                None => {
                    if cpu as usize >= MAX_CPU {
                        return Err(22);
                    }
                    self.mask = 1u64 << cpu;
                    Ok(())
                }
            }
        }

        fn sched_cpu_mask(&mut self) -> Result<u64, i32> {
            self.calls.push("get_affinity".to_string());
            match self.fail_mask {
                Some(errno) => Err(errno),
                None => Ok(self.forced_mask.unwrap_or(self.mask)),
            }
        }
    }

    const ALLOWED_0_7: u64 = 0xFF;

    #[test]
    fn t45apply_constants_mlock_flags() {
        assert_eq!(MLOCK_ALL_FLAGS, 3);
        assert_eq!(MCL_CURRENT, 1);
        assert_eq!(MCL_FUTURE, 2);
        assert_eq!(SCHED_FIFO_POLICY, 1);
        assert_eq!(all_cpu_bits().count_ones(), MAX_CPU as u32);
    }

    #[test]
    fn t45apply_prepare_targets_from_plan_auto_io_cpu() {
        let sched = plan_4worker();
        let targets = prepare_targets(&sched, config(), ALLOWED_0_7).expect("targets");
        assert_eq!(targets.workers, 4);
        assert_eq!(targets.main.cpu, 0, "自动选第一个非 DSP CPU");
        assert_eq!(targets.main.priority, 80);
        assert_eq!(targets.irq_priority, 90);
        let mut k = 0usize;
        while k < 4 {
            assert_eq!(targets.worker[k].cpu, 2 + k as u16);
            assert_eq!(targets.worker[k].priority, 70);
            k += 1;
        }
        assert!(targets.validate().is_ok());
    }

    #[test]
    fn t45apply_prepare_targets_respects_explicit_io_cpu() {
        let sched = plan_4worker();
        let mut cfg = config();
        cfg.io_cpu = 7;
        let targets = prepare_targets(&sched, cfg, ALLOWED_0_7).expect("targets");
        assert_eq!(targets.main.cpu, 7);
    }

    #[test]
    fn t45apply_prepare_targets_rejects_io_cpu_on_dsp_cpu() {
        let sched = plan_4worker();
        let mut cfg = config();
        cfg.io_cpu = 3;
        let error = prepare_targets(&sched, cfg, ALLOWED_0_7).expect_err("must reject");
        assert_eq!(error.stage, ApplyStage::Inputs);
    }

    #[test]
    fn t45apply_prepare_targets_rejects_no_free_cpu() {
        let sched = plan_4worker();
        // 允许集合只剩 DSP CPU：主线程无核可放（与 worker 同核会饿死 worker）。
        let error = prepare_targets(&sched, config(), 0b11_1100).expect_err("must reject");
        assert_eq!(error.stage, ApplyStage::Inputs);
        assert_eq!(error.errno, ERR_INPUTS);
    }

    #[test]
    fn t45apply_prepare_targets_rejects_worker_cpu_not_allowed() {
        let sched = plan_4worker();
        let error = prepare_targets(&sched, config(), 0b0000_0111).expect_err("cpu 5 not allowed");
        assert_eq!(error.stage, ApplyStage::Inputs);
    }

    #[test]
    fn t45apply_prepare_targets_requires_apply_affinity() {
        let sched = plan_4worker();
        let mut cfg = config();
        cfg.apply_affinity = false;
        assert!(prepare_targets(&sched, cfg, ALLOWED_0_7).is_err());
    }

    #[test]
    fn t45apply_prepare_targets_rejects_empty_allowed_mask() {
        assert!(prepare_targets(&plan_4worker(), config(), 0).is_err());
    }

    #[test]
    fn t45apply_targets_validate_rejects_shared_main_cpu() {
        let sched = plan_4worker();
        let mut targets = prepare_targets(&sched, config(), ALLOWED_0_7).expect("targets");
        targets.main.cpu = targets.worker[1].cpu;
        assert_eq!(
            targets.validate().expect_err("must reject").stage,
            ApplyStage::Inputs
        );
    }

    #[test]
    fn t45apply_apply_main_affinity_then_fifo_and_verify() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        let target = ThreadRtTarget::new(0, 80);
        apply_thread(&mut backend, target, ApplyRole::Main).expect("apply main");
        assert_eq!(
            backend.calls,
            vec![
                "affinity:0",
                "get_affinity",
                "fifo:80",
                "get_policy",
                "get_priority"
            ]
        );
        assert_eq!(backend.priority, 80);
        assert_eq!(backend.mask, 1);
    }

    #[test]
    fn t45apply_apply_worker_fifo_and_affinity() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        apply_thread(&mut backend, ThreadRtTarget::new(5, 70), ApplyRole::Worker)
            .expect("apply worker");
        assert_eq!(backend.mask, 1u64 << 5);
        assert_eq!(backend.priority, 70);
    }

    #[test]
    fn t45apply_mlockall_failure_fails_loudly_before_any_sched() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.fail_mlockall = Some(12);
        let error = apply_mlockall(&mut backend, MLOCK_ALL_FLAGS).expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::Mlockall);
        assert_eq!(error.errno, 12);
        assert!(format!("{error}").contains("ENOMEM"));
        assert_eq!(backend.calls, vec!["mlockall:3".to_string()]);
    }

    #[test]
    fn t45apply_affinity_failure_stops_before_fifo() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.fail_set_cpu = Some(1);
        let error = apply_thread(&mut backend, ThreadRtTarget::new(0, 80), ApplyRole::Main)
            .expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::MainAffinity);
        assert_eq!(error.errno, 1);
        assert_eq!(backend.priority, 0, "亲和失败后不得再设 FIFO");
        assert_eq!(backend.calls, vec!["affinity:0".to_string()]);
    }

    #[test]
    fn t45apply_fifo_failure_reports_eperm() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.fail_set_fifo = Some(1);
        let error = apply_thread(&mut backend, ThreadRtTarget::new(0, 80), ApplyRole::Main)
            .expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::MainSched);
        assert_eq!(error.errno, 1);
        assert!(format!("{error}").contains("EPERM"));
    }

    #[test]
    fn t45apply_verify_policy_mismatch_detected() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.forced_policy = Some(0);
        let error = apply_thread(&mut backend, ThreadRtTarget::new(0, 80), ApplyRole::Main)
            .expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::Verify);
        assert_eq!(error.errno, ERR_VERIFY_POLICY);
    }

    #[test]
    fn t45apply_verify_priority_mismatch_detected() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.forced_priority = Some(50);
        let error = apply_thread(&mut backend, ThreadRtTarget::new(0, 80), ApplyRole::Main)
            .expect_err("must fail");
        assert_eq!(error.errno, ERR_VERIFY_PRIORITY);
    }

    #[test]
    fn t45apply_verify_affinity_mismatch_detected() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.forced_mask = Some(0);
        let error = apply_thread(&mut backend, ThreadRtTarget::new(3, 70), ApplyRole::Worker)
            .expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::Verify);
        assert_eq!(error.errno, ERR_VERIFY_AFFINITY);
    }

    #[test]
    fn t45apply_allowed_mask_error_reports_inputs_stage() {
        let mut backend = MockKernel::new(ALLOWED_0_7);
        backend.fail_mask = Some(1);
        let error = allowed_cpu_mask(&mut backend).expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::Inputs);
    }

    #[test]
    fn t45apply_error_encode_decode_roundtrip() {
        let error = ApplyError::new(ApplyStage::WorkerSched, 1).with_worker(2);
        let decoded = ApplyError::decode(error.encode());
        assert_eq!(decoded.stage, ApplyStage::WorkerSched);
        assert_eq!(decoded.errno, 1);
        assert_eq!(decoded.worker, -1, "worker 索引由父线程按槽保存");
        let inputs = ApplyError::decode(ApplyError::new(ApplyStage::Inputs, ERR_INPUTS).encode());
        assert_eq!(inputs.stage, ApplyStage::Inputs);
        assert_eq!(inputs.errno, ERR_INPUTS);
    }

    #[test]
    fn t45apply_report_disabled_is_honest() {
        let report = RtApplyReport::disabled(90);
        assert!(!report.kernel_applied);
        assert_eq!(report.backend, "none");
        let json = report.to_json();
        assert!(json.contains("\"requested\":false"), "{json}");
        assert!(json.contains("\"kernel_applied\":false"), "{json}");
        assert!(
            json.contains("\"reason\":\"rt.apply_affinity=false\""),
            "{json}"
        );
        assert!(json.contains("\"sched_policy\":\"none\""), "{json}");
    }

    #[test]
    fn t45apply_report_from_applied_requires_all_readbacks() {
        let sched = plan_4worker();
        let targets = prepare_targets(&sched, config(), ALLOWED_0_7).expect("targets");
        let applied = RtApplyReport::from_applied(&targets, true, MLOCK_ALL_FLAGS, true, 4);
        assert!(applied.kernel_applied);
        let json = applied.to_json();
        assert!(json.contains("\"kernel_applied\":true"), "{json}");
        assert!(json.contains("\"sched_policy\":\"SCHED_FIFO\""), "{json}");
        assert!(
            json.contains("\"mlock_flags\":\"MCL_CURRENT|MCL_FUTURE\""),
            "{json}"
        );
        assert!(json.contains("\"workers_applied\":4"), "{json}");
        assert!(json.contains("\"applied\":true"), "{json}");
        assert!(json.contains("\"irq_priority_applied\":false"), "{json}");
        // 任一项缺失都不得声称 kernel_applied。
        let partial = RtApplyReport::from_applied(&targets, true, MLOCK_ALL_FLAGS, true, 3);
        assert!(!partial.kernel_applied);
        assert_eq!(partial.reason, "partial");
        let no_mlock = RtApplyReport::from_applied(&targets, false, 0, true, 4);
        assert!(!no_mlock.kernel_applied);
        assert!(no_mlock.to_json().contains("\"mlock_flags\":\"none\""));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn t45apply_non_linux_backend_fails_closed() {
        assert!(!LINUX_TARGET);
        let mut backend = linux_kernel();
        assert!(!backend.is_real());
        assert_eq!(backend.mlockall(MLOCK_ALL_FLAGS), Err(ENOSYS_LINUX_ONLY));
        assert_eq!(backend.sched_set_fifo(80), Err(ENOSYS_LINUX_ONLY));
        assert_eq!(backend.sched_cpu_mask(), Err(ENOSYS_LINUX_ONLY));
        let error = apply_thread(&mut backend, ThreadRtTarget::new(0, 80), ApplyRole::Main)
            .expect_err("must fail");
        assert_eq!(error.stage, ApplyStage::Unsupported);
        assert_eq!(error.errno, ENOSYS_LINUX_ONLY);
        assert!(format!("{error}").contains("ENOSYS"));
        let worker = apply_worker_thread(ThreadRtTarget::new(2, 70)).expect_err("must fail");
        assert_eq!(worker.stage, ApplyStage::Unsupported);
    }
}
