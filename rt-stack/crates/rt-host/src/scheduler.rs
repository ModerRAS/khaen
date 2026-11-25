//! [NONRT] RT 调度/亲和/IRQ 依赖参数化计划（core-v1 §4.1/§7.4/§11.10/§11.13/§12.3）——WBS T45 写域。
//!
//! 职责：把**实测**拓扑（CPU 在线/允许集合、物理 core、隔离集合、IRQ→CPU 亲和）与准入
//! 参数（profile worker 数、FIFO 优先级、每 worker 依赖 IRQ 位图、tick/spin/cutoff 标定值）
//! 归一化为可审计的参数化计划：
//! - 每 worker 一条 TspiWorkerAffinityV1（32 B / align 8，§11.13）亲和表；
//! - 2 worker 与 4 worker 是**独立 profile**（独立计划/hash/expected_workers/ACK_MASK）；
//! - 准入判决（SchedPlan::validate_against / gate_block）与 §12.3 的 sched 报告字段；
//! - 失核时**整块/整图拒收**（gate_block 返回 RejectWholeBlock），绝不返回“剩余 CPU
//!   子集继续旧图”；降级只能走 two_worker_re_admission 生成**新的独立 2 worker 计划**。
//!
//! 边界（明确不做）：
//! - **不写内核**：本模块无任何内核设置调用、无 FFI、无 unsafe、无文件/进程/环境读取；不设置
//!   亲和/优先级/IRQ 亲和/irqbalance/isolcpus/nohz_full/rcu_nocbs/governor/rt_runtime。
//!   计划只输出参数；真正应用只在 T26 host 启动期一次性执行（WBS T45 DoD：“参数化计划而不
//!   自动写内核”）。apply_plan_to_kernel() 恒返回 ERR_UNSUPPORTED，防止调用方误以为
//!   scheduler 会自动落内核。
//! - 不创建 RT 线程、不分配 RT 内存：本模块类型全是 POD（needs_drop == false），由 T18
//!   admission / T26 host 消费；RT block/ack 等待协议属 T09 core::workers，不在本模块。
//! - 不做真实测量：拓扑文本解析函数只消费 S0-02/S0-03/S0-05 被动快照（WBS §7 命令组）的
//!   **内容字符串**；采集本身（lscpu / sys 节点 / IRQ 亲和）由 e2e-runner 授权窗口执行，本次
//!   板端 NOT_RUN。
//!
//! 与相邻任务接口对齐：
//! - T18 admission：AdmissionEnv.cpu_set_ok 由本模块 SchedPlan::cpu_set_ok 提供；本模块的
//!   plan() 只在全部调度闸门通过时返回计划（失败即 ERR_ADMISSION_SCHED=115）。
//! - T09 workers：expected_workers ∈ {RT_WORKERS=4, RT_WORKERS_FALLBACK=2}，ack_mask() 复用
//!   core::workers::ack_mask（ACK_MASK(expected)，禁止硬写 0xF）；spin/cutoff 默认值复用 T09 的
//!   WORKER_DEFAULT_*；2 worker=独立 plan/epoch 语义与 T09 WorkerReAdmission 一致。
//! - T16 spsc：无共享状态（计划只含 CPU/优先级/IRQ 参数，不携带 ring/desc/payload），无需接口变更。
//!
//! 契约偏差/澄清（详见 evidence/T45/MANIFEST.md §6）：
//! - D-01：§12.3 冻结名 plan_affinity(env: &AdmissionEnv) 与本 leaf 签名
//!   plan_affinity(req: &SchedRequest, topo: &TopologySnapshot) 不同。AdmissionEnv 已在
//!   T105/v1.1 I15 合并 cpu_set_mask/allowed_cpu/expected_workers 等 T45 字段（见
//!   rt-host/src/admission.rs）；T26 目前从 config 构建 SchedRequest 调本模块，AdmissionEnv
//!   是否统一成 env 签名由 integrator 决定；本 leaf 不反向依赖 AdmissionEnv。
//! - D-02：TspiWorkerAffinityV1 已提升到 core::abi（§11.13，32 B/align 8；task-105）；
//!   PlanHeader 也已有 v1.1.2 errata E1 定稿的 affinity_table_offset（u32@96）/
//!   affinity_count（u16@100）字段（reserved1@94、reserved2@102 为显式 padding，必须 0；
//!   size 128/align 64 不变）。本模块仍保留 leaf 本地同布局类型 + 编译期断言（本批次只改
//!   注释、不改逻辑）；后续可改为 re-export core::abi 类型。
//! - D-03：§11.13 把 affinity.irq_dependency_mask 的位义留给 T45；本模块定义 bit0=USB capture、
//!   bit1=USB playback、bit2=PTP/timer、bit3=eth/AoIP、bit4=ALSA period，仅这些位允许出现。
//! - D-04：§12.3 的 affinity_hash [u8;8] 算法未定义；本模块用域分隔 FNV-1a-64（含 workers 与
//!   有效条目字节），确定性、跨平台；T26 报告必须原样记录，提升时不得静默换算法。
//! - D-05：TICK_READS_PER_BLOCK_MAX 在 core-v1 §3.4 为 3（I04；v1.1.2 errata E2 重申），
//!   core::rt 常量已由 task-105 对齐为 3（O-01 关闭）。本模块 TICK_READ_BUDGET_MAX 仍为
//!   本地常量 3（不消费 core 常量、不改 leaf 依赖），二者同值一致。

use core::mem::{align_of, needs_drop, size_of};

use khaen_core::rt::{RtStatus, RT_WORKERS, RT_WORKERS_FALLBACK};
use khaen_core::workers::{
    ack_mask, WORKER_DEFAULT_CUTOFF_DRAIN_ITERS, WORKER_DEFAULT_SPIN_MAX_ITERS,
};

// ---------------------------------------------------------------------------
// 常量（core-v1 §3.4/§4.1/§11.13；IRQ 位义/哈希域为 T45 定义，见 D-03/D-04）
// ---------------------------------------------------------------------------

/// 最大 CPU 编号上界（u64 位图容量；aarch64 板 8 核、x86 开发机亦远小于 64）。
pub const MAX_CPU: usize = 64;
/// DSP worker 上限（= RT_WORKERS；affinity 表最多 4 条）。
pub const DSP_CPU_MAX: usize = RT_WORKERS as usize;
/// plan affinity 表条目上限（core-v1 §3.4 WORKER_AFFINITY_COUNT）。
pub const WORKER_AFFINITY_COUNT: usize = 4;
/// TspiWorkerAffinityV1 单条字节（§11.13：32 B / align 8）。
pub const AFFINITY_ENTRY_BYTES: usize = 32;
/// affinity 表最大字节（4 × 32 = 128）。
pub const AFFINITY_TABLE_MAX_BYTES: usize = WORKER_AFFINITY_COUNT * AFFINITY_ENTRY_BYTES;
/// affinity_hash 字节数（§12.3 affinity_hash: [u8;8]）。
pub const AFFINITY_HASH_BYTES: usize = 8;

/// FIFO 优先级合法范围（SCHED_FIFO 1..=99）。
pub const FIFO_PRIORITY_MIN: u8 = 1;
/// FIFO 优先级合法范围上界。
pub const FIFO_PRIORITY_MAX: u8 = 99;
/// DSP worker FIFO 初值（§4.1；仅初值，实测依赖链校正后仍须通过本模块排序校验）。
pub const DSP_PRIORITY_INITIAL: u8 = 70;
/// ALSA I/O owner FIFO 初值（§4.1）。
pub const IO_PRIORITY_INITIAL: u8 = 80;
/// IRQ 线程 FIFO 初值（DSP-HOST-DECISION §5；须高于其供数 DSP 等待）。
pub const IRQ_PRIORITY_INITIAL: u8 = 90;
/// 每 block master 预算内 tick 读取上限（core-v1 §3.4 I04/E2=3；与
/// core::rt::TICK_READS_PER_BLOCK_MAX 同值，D-05 已关闭）。
pub const TICK_READ_BUDGET_MAX: u32 = 3;

/// affinity flags bit0：RT 线程（本计划内恒置位）。
pub const AFFINITY_FLAG_RT: u16 = 1 << 0;
/// affinity flags bit1：实测 CPU 属于 isolated 集合。
pub const AFFINITY_FLAG_ISOLATED_CPU: u16 = 1 << 1;
/// affinity flags 允许位掩码（其余位必须为 0）。
pub const AFFINITY_FLAG_KNOWN_MASK: u16 = AFFINITY_FLAG_RT | AFFINITY_FLAG_ISOLATED_CPU;

/// 未使用 CPU 标记（u16::MAX：DSP_CPUS 未配置/不存在的槽）。
pub const CPU_UNUSED: u16 = u16::MAX;
/// 未知物理 core 标记。
pub const CPU_UNKNOWN_CORE: i8 = -1;

/// IRQ 依赖位：USB capture（xhci/ALSA capture 供数；master 依赖）。
pub const IRQ_BIT_USB_CAPTURE: u64 = 1 << 0;
/// IRQ 依赖位：USB playback（write/poll 完成；I/O 区依赖，DSP 不直接依赖）。
pub const IRQ_BIT_USB_PLAYBACK: u64 = 1 << 1;
/// IRQ 依赖位：PTP/timer（时间戳/epoch 来源）。
pub const IRQ_BIT_PTP_TIMER: u64 = 1 << 2;
/// IRQ 依赖位：eth/AoIP（AES67/Inferno 端点）。
pub const IRQ_BIT_ETH_AOIP: u64 = 1 << 3;
/// IRQ 依赖位：ALSA period/hw 中断。
pub const IRQ_BIT_ALSA_PERIOD: u64 = 1 << 4;
/// 允许出现在 irq_dependency_mask 的位（其余位 = ERR_ADMISSION_SCHED；D-03）。
pub const IRQ_BIT_KNOWN_MASK: u64 = IRQ_BIT_USB_CAPTURE
    | IRQ_BIT_USB_PLAYBACK
    | IRQ_BIT_PTP_TIMER
    | IRQ_BIT_ETH_AOIP
    | IRQ_BIT_ALSA_PERIOD;
/// 无 IRQ 依赖。
pub const IRQ_DEPENDENCY_NONE: u64 = 0;
/// IRQ class 数（= 位图宽度）。
pub const IRQ_CLASS_COUNT: usize = 8;

/// T45 是否执行内核写入：恒 false（本模块只产出计划；应用归 T26 启动期）。
pub const KERNEL_WRITES_PERFORMED: bool = false;

/// affinity_hash 域分隔标签（D-04）。
const AFFINITY_HASH_DOMAIN: &[u8] = b"tspi-affinity-v1";
const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

// ---------------------------------------------------------------------------
// §11.13 TspiWorkerAffinityV1（32 B / align 8；core::abi 已提供，leaf 保留同布局副本——D-02）
// ---------------------------------------------------------------------------

/// worker 亲和/优先级/IRQ 依赖表项（core-v1 §11.13；**align 例外**：32 B / align 8）。
///
/// 字段偏移（§11.13）：worker_id=0、sched_priority=1、flags=2、cpu_index=4、reserved0=6、
/// irq_dependency_mask=8、reserved1=16、reserved2=24；总 32 B，needs_drop == false。
/// 本类型是 plan 内 POD 表项（不是跨线程原子结构），只由本模块在 admission 前构建/校验。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiWorkerAffinityV1 {
    /// worker 编号 0..3。
    pub worker_id: u8,
    /// FIFO 优先级（初值 70；实测校正后必须满足 dsp < io <= irq）。
    pub sched_priority: u8,
    /// bit0=rt、bit1=isolated_cpu；其余 0。
    pub flags: u16,
    /// 实际（实测）拓扑 CPU 编号。
    pub cpu_index: u16,
    /// 必须为 0。
    pub reserved0: u16,
    /// 依赖的 IRQ class 位图（位义见 D-03）。
    pub irq_dependency_mask: u64,
    /// 必须为 0。
    pub reserved1: u64,
    /// 必须为 0。
    pub reserved2: u64,
}

impl TspiWorkerAffinityV1 {
    /// 构造一条亲和表项（reserved 恒 0；flags 只允许已知位）。
    pub const fn new(
        worker_id: u8,
        sched_priority: u8,
        flags: u16,
        cpu_index: u16,
        irq_dependency_mask: u64,
    ) -> Self {
        Self {
            worker_id,
            sched_priority,
            flags: flags & AFFINITY_FLAG_KNOWN_MASK,
            cpu_index,
            reserved0: 0,
            irq_dependency_mask,
            reserved1: 0,
            reserved2: 0,
        }
    }

    /// 全零表项（未使用槽）。
    pub const ZERO: Self = Self {
        worker_id: 0,
        sched_priority: 0,
        flags: 0,
        cpu_index: 0,
        reserved0: 0,
        irq_dependency_mask: 0,
        reserved1: 0,
        reserved2: 0,
    };

    /// 是否 RT 表项（bit0）。
    pub const fn is_rt(self) -> bool {
        self.flags & AFFINITY_FLAG_RT != 0
    }

    /// 是否标记 isolated CPU（bit1）。
    pub const fn is_isolated(self) -> bool {
        self.flags & AFFINITY_FLAG_ISOLATED_CPU != 0
    }

    /// 表项不变量自校验（字段范围/保留位/已有位；不查拓扑）。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if self.sched_priority < FIFO_PRIORITY_MIN || self.sched_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.flags & !AFFINITY_FLAG_KNOWN_MASK != 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if !self.is_rt() {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.cpu_index == CPU_UNUSED || self.cpu_index as usize >= MAX_CPU {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.reserved0 != 0 || self.reserved1 != 0 || self.reserved2 != 0 {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        if self.irq_dependency_mask & !IRQ_BIT_KNOWN_MASK != 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        Ok(())
    }

    /// 按 §11.13 偏移编码为 32 B（LE；保留位写 0）。用于 plan affinity 表打包/哈希。
    pub fn to_bytes(self) -> [u8; AFFINITY_ENTRY_BYTES] {
        let mut out = [0u8; AFFINITY_ENTRY_BYTES];
        out[0] = self.worker_id;
        out[1] = self.sched_priority;
        out[2] = (self.flags & 0xFF) as u8;
        out[3] = (self.flags >> 8) as u8;
        out[4] = (self.cpu_index & 0xFF) as u8;
        out[5] = (self.cpu_index >> 8) as u8;
        out[6] = (self.reserved0 & 0xFF) as u8;
        out[7] = (self.reserved0 >> 8) as u8;
        let mut mask = self.irq_dependency_mask;
        let mut i = 8;
        while i < 16 {
            out[i] = (mask & 0xFF) as u8;
            mask >>= 8;
            i += 1;
        }
        out
    }
}

// ---------------------------------------------------------------------------
// 实测拓扑快照（纯 POD，不读设备；由 S0-02/S0-03/S0-05 文本解析或 T26 注入）
// ---------------------------------------------------------------------------

/// 实测 CPU/IRQ 拓扑快照（NONRT，Copy，needs_drop == false）。
///
/// 语义：online_mask/possible_mask/isolated_mask/nohz_full_mask/allowed_mask 是实测集合
/// （bit k = CPU k）；allowed_mask == 0 表示**未采集**（准入拒绝，不猜）；irq_present_mask 记录
/// 已测到 CPU 亲和的 IRQ class，irq_cpu[k] == CPU_UNUSED 表示缺证据。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopologySnapshot {
    /// /sys/devices/system/cpu/online（bit k = CPU k 在线）。
    pub online_mask: u64,
    /// /sys/devices/system/cpu/possible。
    pub possible_mask: u64,
    /// /sys/devices/system/cpu/isolated（isolcpus）。
    pub isolated_mask: u64,
    /// /sys/devices/system/cpu/nohz_full（或 cmdline nohz_full=）。
    pub nohz_full_mask: u64,
    /// RT 线程允许的 CPU 集合（/proc/<pid>/status Cpus_allowed_list）。
    pub allowed_mask: u64,
    /// 已测到亲和的 IRQ class 位图（bit = IRQ_BIT_*）。
    pub irq_present_mask: u64,
    /// 每 IRQ class 实测 CPU（CPU_UNUSED = 缺证据）。
    pub irq_cpu: [u16; IRQ_CLASS_COUNT],
    /// 每 CPU 物理 core id（lscpu CORE；CPU_UNKNOWN_CORE = 未知）。用于拒绝 SMT 兄弟核共用。
    pub core_of_cpu: [i8; MAX_CPU],
}

impl TopologySnapshot {
    /// 空快照：全部集合空、core/IRQ 未知（其后用 builder/解析函数填充）。
    pub const fn new() -> Self {
        Self {
            online_mask: 0,
            possible_mask: 0,
            isolated_mask: 0,
            nohz_full_mask: 0,
            allowed_mask: 0,
            irq_present_mask: 0,
            irq_cpu: [CPU_UNUSED; IRQ_CLASS_COUNT],
            core_of_cpu: [CPU_UNKNOWN_CORE; MAX_CPU],
        }
    }

    /// 设置 online 集合。
    pub const fn with_online_mask(mut self, mask: u64) -> Self {
        self.online_mask = mask;
        self
    }

    /// 设置 possible 集合。
    pub const fn with_possible_mask(mut self, mask: u64) -> Self {
        self.possible_mask = mask;
        self
    }

    /// 设置 isolated 集合。
    pub const fn with_isolated_mask(mut self, mask: u64) -> Self {
        self.isolated_mask = mask;
        self
    }

    /// 设置 nohz_full 集合。
    pub const fn with_nohz_full_mask(mut self, mask: u64) -> Self {
        self.nohz_full_mask = mask;
        self
    }

    /// 设置 RT 允许 CPU 集合（必须由实测 Cpus_allowed_list 提供；0 = 未采集）。
    pub const fn with_allowed_mask(mut self, mask: u64) -> Self {
        self.allowed_mask = mask;
        self
    }

    /// 设置物理 core 映射（lscpu CORE 列）。
    pub const fn with_core_map(mut self, cores: [i8; MAX_CPU]) -> Self {
        self.core_of_cpu = cores;
        self
    }

    /// CPU 是否在线（越界/未采集 = false）。
    pub const fn cpu_online(&self, cpu: u16) -> bool {
        (cpu as usize) < MAX_CPU && self.online_mask & (1u64 << cpu) != 0
    }

    /// CPU 是否属于 isolated 集合。
    pub const fn cpu_isolated(&self, cpu: u16) -> bool {
        (cpu as usize) < MAX_CPU && self.isolated_mask & (1u64 << cpu) != 0
    }

    /// CPU 是否在实测允许集合内（allowed_mask==0 = 未采集 ⇒ false，准入拒绝）。
    pub const fn cpu_allowed(&self, cpu: u16) -> bool {
        (cpu as usize) < MAX_CPU && self.allowed_mask & (1u64 << cpu) != 0
    }

    /// CPU 物理 core id（未测 = CPU_UNKNOWN_CORE）。
    pub const fn cpu_core(&self, cpu: u16) -> i8 {
        if cpu as usize >= MAX_CPU {
            CPU_UNKNOWN_CORE
        } else {
            self.core_of_cpu[cpu as usize]
        }
    }

    /// 单个 IRQ class 位是否有实测 CPU 亲和证据；返回该 CPU。
    pub fn irq_cpu_for_bit(&self, bit: u64) -> Option<u16> {
        if bit.count_ones() != 1 {
            return None;
        }
        let index = bit.trailing_zeros() as usize;
        if index >= IRQ_CLASS_COUNT || self.irq_present_mask & bit == 0 {
            return None;
        }
        let cpu = self.irq_cpu[index];
        if cpu == CPU_UNUSED || cpu as usize >= MAX_CPU {
            None
        } else {
            Some(cpu)
        }
    }

    /// mask 内每个 IRQ 位是否都有实测亲和证据。
    pub fn has_irq_evidence(&self, mask: u64) -> bool {
        if mask & !IRQ_BIT_KNOWN_MASK != 0 {
            return false;
        }
        let mut rest = mask;
        while rest != 0 {
            let bit = rest & rest.wrapping_neg();
            if self.irq_cpu_for_bit(bit).is_none() {
                return false;
            }
            rest ^= bit;
        }
        true
    }

    /// 设置某 IRQ class 的实测 CPU（class_index 0..IRQ_CLASS_COUNT，对应 bit=1<<index）。
    pub fn set_irq_cpu(&mut self, class_index: usize, cpu: u16) -> Result<(), RtStatus> {
        if class_index >= IRQ_CLASS_COUNT || cpu == CPU_UNUSED || cpu as usize >= MAX_CPU {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        self.irq_cpu[class_index] = cpu;
        self.irq_present_mask |= 1u64 << class_index;
        Ok(())
    }

    /// 由实测 smp_affinity_list 掩码设置 IRQ class（取最低 CPU；空掩码拒绝）。
    pub fn set_irq_affinity_mask(
        &mut self,
        class_index: usize,
        affinity_mask: u64,
    ) -> Result<(), RtStatus> {
        let cpu = match first_cpu_in_mask(affinity_mask) {
            Some(value) => value,
            None => return Err(RtStatus::ERR_ADMISSION_SCHED),
        };
        self.set_irq_cpu(class_index, cpu)
    }

    /// 一致性自校验（不查计划）：集合互相包含关系必须与实测口径一致。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if self.online_mask == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.allowed_mask == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.online_mask & !self.possible_mask != 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.allowed_mask & !self.online_mask != 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.isolated_mask & !self.online_mask != 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.irq_present_mask & !IRQ_BIT_KNOWN_MASK != 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut cpu = 0usize;
        while cpu < MAX_CPU {
            if self.core_of_cpu[cpu] < CPU_UNKNOWN_CORE {
                return Err(RtStatus::ERR_BAD_LAYOUT);
            }
            cpu += 1;
        }
        Ok(())
    }

    /// 实测入口：lscpu -e 文本 + /sys online + Cpus_allowed_list + isolated + /proc/cmdline。
    ///
    /// 只解析传入字符串（纯函数，不读设备）；allowed 为空 = 未采集 ⇒ 拒绝。IRQ 亲和需另用
    /// set_irq_affinity_mask 注入（来自 /proc/irq/N/smp_affinity_list）；本函数不猜 IRQ。
    pub fn from_measured(
        lscpu: &str,
        online: &str,
        allowed: &str,
        isolated: &str,
        cmdline: &str,
    ) -> Result<Self, RtStatus> {
        let table = parse_lscpu_e(lscpu)?;
        let online_parsed = parse_cpu_list(online)?;
        let online_mask = if online_parsed != 0 {
            online_parsed
        } else {
            table.online_mask
        };
        if online_mask == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let allowed_mask = parse_cpu_list(allowed)?;
        if allowed_mask == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let snapshot = Self {
            online_mask,
            possible_mask: table.possible_mask,
            isolated_mask: parse_cpu_list(isolated)?,
            nohz_full_mask: parse_cmdline_nohz_full(cmdline)?,
            allowed_mask,
            irq_present_mask: 0,
            irq_cpu: [CPU_UNUSED; IRQ_CLASS_COUNT],
            core_of_cpu: table.core_of_cpu,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

impl Default for TopologySnapshot {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 实测文本解析（纯函数；WBS §7 被动命令格式；不读设备/不写内核）
// ---------------------------------------------------------------------------

/// 十进制 CPU 编号（0..MAX_CPU-1）；空/非数字/越界拒绝。
fn parse_dec_cpu(bytes: &[u8]) -> Result<u16, RtStatus> {
    if bytes.is_empty() {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    let mut value: u32 = 0;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        value = value * 10 + (b - b'0') as u32;
        if value as usize >= MAX_CPU {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        i += 1;
    }
    Ok(value as u16)
}

/// 解析内核 CPU 列表文本（0-3,6,7 风格；online/isolated/allowed/smp_affinity_list 通用）。
///
/// 空文本 = 空集合（如 isolated 无隔离核）返回 0；语法错误/编号 >= MAX_CPU 返回
/// ERR_ADMISSION_SCHED。结果位图 bit k = CPU k。
pub fn parse_cpu_list(text: &str) -> Result<u64, RtStatus> {
    let bytes = text.trim().as_bytes();
    if bytes.is_empty() {
        return Ok(0);
    }
    let mut mask: u64 = 0;
    let mut start = 0usize;
    let mut i = 0usize;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b',' {
            let token = &bytes[start..i];
            if token.is_empty() {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            let mut dash: Option<usize> = None;
            let mut j = 0usize;
            while j < token.len() {
                let b = token[j];
                if b == b'-' {
                    if dash.is_some() {
                        return Err(RtStatus::ERR_ADMISSION_SCHED);
                    }
                    dash = Some(j);
                } else if !b.is_ascii_digit() {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                j += 1;
            }
            let (lo, hi) = match dash {
                Some(pos) => (
                    parse_dec_cpu(&token[..pos])?,
                    parse_dec_cpu(&token[pos + 1..])?,
                ),
                None => {
                    let only = parse_dec_cpu(token)?;
                    (only, only)
                }
            };
            if lo > hi {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            let mut cpu = lo;
            while cpu <= hi {
                mask |= 1u64 << cpu;
                cpu += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    Ok(mask)
}

/// 解析 isolcpus=（支持 isolcpus=2-3 与 isolcpus=domain,2-3 形式；可多次出现取并集）。
pub fn parse_cmdline_isolcpus(cmdline: &str) -> Result<u64, RtStatus> {
    parse_cmdline_cpu_mask(cmdline, "isolcpus=")
}

/// 解析 nohz_full=（格式同 isolcpus；nohz_full=all 等非列表值忽略并可审计）。
pub fn parse_cmdline_nohz_full(cmdline: &str) -> Result<u64, RtStatus> {
    parse_cmdline_cpu_mask(cmdline, "nohz_full=")
}

/// 解析 cmdline 中 key= 的 CPU 列表（内部通用；首个逗号前的非列表标志被跳过）。
fn parse_cmdline_cpu_mask(cmdline: &str, key: &str) -> Result<u64, RtStatus> {
    let mut mask = 0u64;
    for token in cmdline.split_ascii_whitespace() {
        let value = match token.strip_prefix(key) {
            Some(rest) => rest,
            None => continue,
        };
        let list = match value.find(',') {
            Some(pos) if !value[..pos].is_empty() && !value.as_bytes()[0].is_ascii_digit() => {
                &value[pos + 1..]
            }
            _ => value,
        };
        if list.is_empty() || list == "all" || list == "none" {
            continue;
        }
        mask |= parse_cpu_list(list)?;
    }
    Ok(mask)
}

/// lscpu -e=CPU,CORE,SOCKET,ONLINE,... 解析结果（固定数组，无堆分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LscpuSnapshot {
    /// 出现的 CPU 行数。
    pub cpu_count: u8,
    /// 在线 CPU bitmask。
    pub online_mask: u64,
    /// 出现的 CPU bitmask（possible 口径）。
    pub possible_mask: u64,
    /// 每 CPU 物理 core id（CPU_UNKNOWN_CORE = 未知）。
    pub core_of_cpu: [i8; MAX_CPU],
}

impl LscpuSnapshot {
    /// 空结果。
    pub const fn new() -> Self {
        Self {
            cpu_count: 0,
            online_mask: 0,
            possible_mask: 0,
            core_of_cpu: [CPU_UNKNOWN_CORE; MAX_CPU],
        }
    }
}

impl Default for LscpuSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 lscpu -e=CPU,CORE,SOCKET,ONLINE,MAXMHZ,MINMHZ 输出（S0-03 实测格式）。
///
/// 规则：首个非空行为表头，要求含 CPU/CORE/ONLINE 列（列序按表头）；数据行按空白切分，
/// CPU/CORE/SOCKET/ONLINE 在 MAXMHZ 之前，故其后空列不影响前四列。ONLINE 接受 yes/no/1/0；
/// CPU 重复/越界、CORE 非法、必需列缺失 = ERR_ADMISSION_SCHED。
pub fn parse_lscpu_e(text: &str) -> Result<LscpuSnapshot, RtStatus> {
    let mut snapshot = LscpuSnapshot::new();
    let mut header: Option<[usize; 3]> = None; // (cpu, core, online) 列下标
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        match header {
            None => header = Some(parse_lscpu_header(line)?),
            Some(columns) => parse_lscpu_row(line, columns, &mut snapshot)?,
        }
    }
    match header {
        Some(_) => Ok(snapshot),
        None => Err(RtStatus::ERR_ADMISSION_SCHED),
    }
}

fn parse_lscpu_header(line: &str) -> Result<[usize; 3], RtStatus> {
    let mut cpu_col: Option<usize> = None;
    let mut core_col: Option<usize> = None;
    let mut online_col: Option<usize> = None;
    let mut index = 0usize;
    for name in line.split_ascii_whitespace() {
        if name.eq_ignore_ascii_case("CPU") {
            cpu_col = Some(index);
        } else if name.eq_ignore_ascii_case("CORE") {
            core_col = Some(index);
        } else if name.eq_ignore_ascii_case("ONLINE") {
            online_col = Some(index);
        }
        index += 1;
    }
    match (cpu_col, core_col, online_col) {
        (Some(cpu), Some(core), Some(online)) => Ok([cpu, core, online]),
        _ => Err(RtStatus::ERR_ADMISSION_SCHED),
    }
}

fn parse_lscpu_row(
    line: &str,
    columns: [usize; 3],
    snapshot: &mut LscpuSnapshot,
) -> Result<(), RtStatus> {
    let mut fields = [0u16; 3];
    let max_column = columns[0].max(columns[1]).max(columns[2]);
    let mut seen = [false; 3];
    let mut index = 0usize;
    for value in line.split_ascii_whitespace() {
        if index == columns[0] {
            fields[0] = parse_dec_cpu(value.as_bytes())?;
            seen[0] = true;
        } else if index == columns[1] {
            fields[1] = parse_dec_cpu(value.as_bytes())?;
            seen[1] = true;
        } else if index == columns[2] {
            fields[2] = parse_lscpu_online(value)?;
            seen[2] = true;
        }
        index += 1;
        if index > max_column {
            break;
        }
    }
    if !seen[0] || !seen[1] || !seen[2] {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    let cpu = fields[0];
    let bit = 1u64 << cpu;
    if snapshot.possible_mask & bit != 0 {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    snapshot.possible_mask |= bit;
    snapshot.core_of_cpu[cpu as usize] = fields[1] as i8;
    if fields[2] == 1 {
        snapshot.online_mask |= bit;
    }
    snapshot.cpu_count = snapshot.cpu_count.saturating_add(1);
    Ok(())
}

fn parse_lscpu_online(value: &str) -> Result<u16, RtStatus> {
    if value == "1" || value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("true") {
        Ok(1)
    } else if value == "0"
        || value.eq_ignore_ascii_case("no")
        || value.eq_ignore_ascii_case("false")
    {
        Ok(0)
    } else {
        Err(RtStatus::ERR_ADMISSION_SCHED)
    }
}

/// 解析 /proc/irq/N/smp_affinity_list（格式同 CPU 列表）。
pub fn parse_irq_affinity_list(text: &str) -> Result<u64, RtStatus> {
    parse_cpu_list(text)
}

/// 掩码中最低 CPU 编号（空掩码 = None）。
pub const fn first_cpu_in_mask(mask: u64) -> Option<u16> {
    if mask == 0 {
        None
    } else {
        Some(mask.trailing_zeros() as u16)
    }
}

// ---------------------------------------------------------------------------
// 调度准入请求（实测拓扑 + profile + FIFO + IRQ 依赖 + tick/spin/cutoff 标定参数）
// ---------------------------------------------------------------------------

/// 调度准入请求（NONRT，Copy，needs_drop == false）。
///
/// 字段全部由 T26/T45 从**实测**与已标定值填入：dsp_cpus 来自实测 DSP_CPUS 选择（不是假核号），
/// irq_dependency 来自实测依赖链，tick_freq_hz 来自 T19 标定（0 = 未标定 ⇒ 准入拒绝），
/// spin_max_iters/cutoff_drain_iters 来自 T18/T09 标定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedRequest {
    /// 目标 worker 数：4（生产）或 2（**独立**降级 profile）。
    pub workers: u8,
    /// 逐 worker 实测 DSP_CPUS；只使用前 workers 槽，其余必须 CPU_UNUSED。
    pub dsp_cpus: [u16; DSP_CPU_MAX],
    /// DSP worker FIFO 优先级（初值 70；实测校正后仍须 dsp < io <= irq）。
    pub dsp_priority: u8,
    /// ALSA I/O owner FIFO 优先级（初值 80）。
    pub io_priority: u8,
    /// IRQ 线程 FIFO 优先级（初值 90）。
    pub irq_priority: u8,
    /// 逐 worker 依赖 IRQ 位图（D-03 位义）；只使用前 workers 槽。
    pub irq_dependency: [u64; DSP_CPU_MAX],
    /// tick→ns 标定频率（T19）；0 = 未标定，准入拒绝。
    pub tick_freq_hz: u32,
    /// master 每 block 预算内 tick 读取上限（1..=3）。
    pub tick_read_budget: u32,
    /// bounded spin 上限（T18/T09 标定；> 0）。
    pub spin_max_iters: u32,
    /// cutoff 后 drain 上限（T18/T09 标定；> 0）。
    pub cutoff_drain_iters: u32,
    /// 计划版本（进程内单调；不参与 affinity_hash）。
    pub plan_version: u32,
}

impl SchedRequest {
    /// 保守默认：4 worker、FIFO 70/80/90、DSP_CPUS 未配置、无 IRQ、tick 未标定、T09 默认 spin/cutoff。
    pub const fn new() -> Self {
        Self {
            workers: RT_WORKERS,
            dsp_cpus: [CPU_UNUSED; DSP_CPU_MAX],
            dsp_priority: DSP_PRIORITY_INITIAL,
            io_priority: IO_PRIORITY_INITIAL,
            irq_priority: IRQ_PRIORITY_INITIAL,
            irq_dependency: [IRQ_DEPENDENCY_NONE; DSP_CPU_MAX],
            tick_freq_hz: 0,
            tick_read_budget: 1,
            spin_max_iters: WORKER_DEFAULT_SPIN_MAX_ITERS,
            cutoff_drain_iters: WORKER_DEFAULT_CUTOFF_DRAIN_ITERS,
            plan_version: 1,
        }
    }

    /// 设置 profile worker 数（4/2）。
    pub const fn with_workers(mut self, workers: u8) -> Self {
        self.workers = workers;
        self
    }

    /// 设置第 index 个 worker 的实测 DSP CPU。
    pub const fn with_dsp_cpu(mut self, index: usize, cpu: u16) -> Self {
        if index < DSP_CPU_MAX {
            self.dsp_cpus[index] = cpu;
        }
        self
    }

    /// 设置第 worker 个 worker 的依赖 IRQ 位图。
    pub const fn with_irq_dependency(mut self, worker: usize, mask: u64) -> Self {
        if worker < DSP_CPU_MAX {
            self.irq_dependency[worker] = mask & IRQ_BIT_KNOWN_MASK;
        }
        self
    }

    /// 设置 FIFO 优先级三元组（DSP/I/O/IRQ）。
    pub const fn with_priorities(mut self, dsp: u8, io: u8, irq: u8) -> Self {
        self.dsp_priority = dsp;
        self.io_priority = io;
        self.irq_priority = irq;
        self
    }

    /// 设置 T19 tick 标定频率与预算。
    pub const fn with_tick(mut self, freq_hz: u32, read_budget: u32) -> Self {
        self.tick_freq_hz = freq_hz;
        self.tick_read_budget = read_budget;
        self
    }

    /// 设置 T18/T09 spin/cutoff 标定值。
    pub const fn with_calibration(mut self, spin_max_iters: u32, cutoff_drain_iters: u32) -> Self {
        self.spin_max_iters = spin_max_iters;
        self.cutoff_drain_iters = cutoff_drain_iters;
        self
    }

    /// 设置计划版本。
    pub const fn with_plan_version(mut self, plan_version: u32) -> Self {
        self.plan_version = plan_version;
        self
    }

    /// 请求自校验（不查拓扑）：profile/优先级排序/标定值/CPU 槽/IRQ 位。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if self.workers != RT_WORKERS && self.workers != RT_WORKERS_FALLBACK {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.dsp_priority < FIFO_PRIORITY_MIN || self.dsp_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.io_priority < FIFO_PRIORITY_MIN || self.io_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.irq_priority < FIFO_PRIORITY_MIN || self.irq_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        // DSP FIFO 忙等不得压住供数 I/O 与 IRQ（DSP-HOST-DECISION §4/§5）：dsp < io <= irq。
        if !(self.dsp_priority < self.io_priority && self.io_priority <= self.irq_priority) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.tick_freq_hz == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.tick_read_budget == 0 || self.tick_read_budget > TICK_READ_BUDGET_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.spin_max_iters == 0 || self.cutoff_drain_iters == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut seen = [CPU_UNUSED; DSP_CPU_MAX];
        let mut k = 0usize;
        while k < DSP_CPU_MAX {
            if k < self.workers as usize {
                let cpu = self.dsp_cpus[k];
                if cpu == CPU_UNUSED || cpu as usize >= MAX_CPU {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                if seen.contains(&cpu) {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                seen[k] = cpu;
                if self.irq_dependency[k] & !IRQ_BIT_KNOWN_MASK != 0 {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
            } else if self.dsp_cpus[k] != CPU_UNUSED
                || self.irq_dependency[k] != IRQ_DEPENDENCY_NONE
            {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            k += 1;
        }
        Ok(())
    }
}

impl Default for SchedRequest {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 调度计划（POD；profile + affinity 表 + 标定参数 + IRQ 依赖 + hash）
// ---------------------------------------------------------------------------

/// 参数化 RT 调度计划（NONRT 构建；RT 只读；Copy，needs_drop == false）。
///
/// 2 worker 与 4 worker 是独立 profile：workers/affinity 有效条目/affinity_hash/ack_mask() 全部
/// 不同；4→2 只能经 two_worker_re_admission 生成新计划 + 新 epoch（T09 语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedPlan {
    /// plan 版本（不参与 affinity_hash）。
    pub plan_version: u32,
    /// 本 profile worker 数：4（生产）或 2（独立降级 profile）。
    pub workers: u8,
    /// 逐 worker（前 workers 条有效）affinity 表；未用槽全零。
    pub affinity: [TspiWorkerAffinityV1; WORKER_AFFINITY_COUNT],
    /// DSP worker FIFO 优先级（与表项一致）。
    pub dsp_priority: u8,
    /// ALSA I/O owner FIFO 优先级（计划参数，供 T26 应用）。
    pub io_priority: u8,
    /// IRQ 线程 FIFO 优先级（计划参数，供 T26 应用）。
    pub irq_priority: u8,
    /// tick 标定频率（T19）。
    pub tick_freq_hz: u32,
    /// master 每 block tick 读取预算。
    pub tick_read_budget: u32,
    /// bounded spin 标定值（T18）。
    pub spin_max_iters: u32,
    /// cutoff drain 标定值（T18）。
    pub cutoff_drain_iters: u32,
    /// affinity 表规范化 hash（8 B，D-04）。
    pub affinity_hash: [u8; AFFINITY_HASH_BYTES],
}

impl SchedPlan {
    /// 本 profile 的 expected_workers（= workers）。
    pub const fn expected_workers(&self) -> u8 {
        self.workers
    }

    /// ACK_MASK(expected) = (1 << expected) - 1（§3.4；禁止硬写 0xF）。
    pub const fn ack_mask(&self) -> u32 {
        ack_mask(self.workers)
    }

    /// 是否 2 worker 独立降级 profile。
    pub const fn is_two_worker_profile(&self) -> bool {
        self.workers == RT_WORKERS_FALLBACK
    }

    /// affinity 表有效条目数（= workers；§11.10 affinity_count）。
    pub const fn affinity_count(&self) -> u16 {
        self.workers as u16
    }

    /// affinity 表有效字节（workers × 32）。
    pub const fn affinity_table_len(&self) -> usize {
        self.workers as usize * AFFINITY_ENTRY_BYTES
    }

    /// worker k 的 affinity 表项（k >= workers 返回 None）。
    pub fn affinity_entry(&self, worker: usize) -> Option<TspiWorkerAffinityV1> {
        if worker >= self.workers as usize || worker >= WORKER_AFFINITY_COUNT {
            None
        } else {
            Some(self.affinity[worker])
        }
    }

    /// worker k 的实测 CPU。
    pub fn worker_cpu(&self, worker: usize) -> Option<u16> {
        self.affinity_entry(worker).map(|entry| entry.cpu_index)
    }

    /// worker k 的 IRQ 依赖位图。
    pub fn worker_irq_mask(&self, worker: usize) -> Option<u64> {
        self.affinity_entry(worker)
            .map(|entry| entry.irq_dependency_mask)
    }

    /// 全部有效 worker 的 IRQ 依赖并集（供 §12.3 irq_dependency_mask 报告字段）。
    pub fn union_irq_dependency(&self) -> u64 {
        let mut mask = 0u64;
        let mut k = 0usize;
        while k < self.workers as usize {
            mask |= self.affinity[k].irq_dependency_mask;
            k += 1;
        }
        mask
    }

    /// CPU 是否被本计划的某个 worker 占用。
    pub fn cpu_in_plan(&self, cpu: u16) -> bool {
        let mut k = 0usize;
        while k < self.workers as usize {
            if self.affinity[k].cpu_index == cpu {
                return true;
            }
            k += 1;
        }
        false
    }

    /// 计划自校验（不查拓扑）：profile/表项/优先/标定/hash 自洽。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if self.workers != RT_WORKERS && self.workers != RT_WORKERS_FALLBACK {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.dsp_priority < FIFO_PRIORITY_MIN || self.dsp_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.io_priority < FIFO_PRIORITY_MIN || self.io_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.irq_priority < FIFO_PRIORITY_MIN || self.irq_priority > FIFO_PRIORITY_MAX {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if !(self.dsp_priority < self.io_priority && self.io_priority <= self.irq_priority) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.tick_freq_hz == 0
            || self.tick_read_budget == 0
            || self.tick_read_budget > TICK_READ_BUDGET_MAX
            || self.spin_max_iters == 0
            || self.cutoff_drain_iters == 0
        {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut cpu_seen = [CPU_UNUSED; DSP_CPU_MAX];
        let mut k = 0usize;
        while k < WORKER_AFFINITY_COUNT {
            let entry = self.affinity[k];
            if k < self.workers as usize {
                entry.validate()?;
                if entry.worker_id != k as u8 || entry.sched_priority != self.dsp_priority {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                if cpu_seen[..k].contains(&entry.cpu_index) {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                cpu_seen[k] = entry.cpu_index;
            } else if entry != TspiWorkerAffinityV1::ZERO {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            k += 1;
        }
        if self.affinity_hash != compute_affinity_hash(self.workers, &self.affinity) {
            return Err(RtStatus::ERR_HASH);
        }
        Ok(())
    }

    /// 对当前实测拓扑重校验（admission/启动前；拓扑变化 = stale 拒绝）。
    pub fn validate_against(&self, topo: &TopologySnapshot) -> Result<(), RtStatus> {
        self.validate()?;
        topo.validate()?;
        let mut k = 0usize;
        while k < self.workers as usize {
            let entry = self.affinity[k];
            let cpu = entry.cpu_index;
            if !topo.cpu_online(cpu) || !topo.cpu_allowed(cpu) {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            let expected_isolated = topo.cpu_isolated(cpu);
            if entry.is_isolated() != expected_isolated {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            let core = topo.cpu_core(cpu);
            if core != CPU_UNKNOWN_CORE {
                let mut j = 0usize;
                while j < k {
                    if topo.cpu_core(self.affinity[j].cpu_index) == core {
                        return Err(RtStatus::ERR_ADMISSION_SCHED);
                    }
                    j += 1;
                }
            }
            let mut deps = entry.irq_dependency_mask;
            while deps != 0 {
                let bit = deps & deps.wrapping_neg();
                let irq_cpu = match topo.irq_cpu_for_bit(bit) {
                    Some(value) => value,
                    None => return Err(RtStatus::ERR_ADMISSION_SCHED),
                };
                if !topo.cpu_online(irq_cpu) || !topo.cpu_allowed(irq_cpu) {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                if self.cpu_in_plan(irq_cpu) {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                deps ^= bit;
            }
            k += 1;
        }
        Ok(())
    }

    /// AdmissionEnv.cpu_set_ok 的 T45 判定（T18 消费；拓扑/计划任一不合格 = false）。
    pub fn cpu_set_ok(&self, topo: &TopologySnapshot) -> bool {
        self.validate_against(topo).is_ok()
    }

    /// 计划中 CPU 已失（离线/不允许）的 worker 位图（bit k = worker k）。
    pub fn lost_worker_mask(&self, topo: &TopologySnapshot) -> u8 {
        let mut mask = 0u8;
        let mut k = 0usize;
        while k < self.workers as usize {
            let cpu = self.affinity[k].cpu_index;
            if !topo.cpu_online(cpu) || !topo.cpu_allowed(cpu) {
                mask |= 1u8 << k;
            }
            k += 1;
        }
        mask
    }

    /// block/流入口的整块判决（**失核整块/整图拒收**；绝不返回部分 worker 可继续）。
    pub fn gate_block(&self, topo: &TopologySnapshot) -> SchedGate {
        if topo.validate().is_err() {
            return SchedGate::RejectWholeBlock {
                reason: SchedRejectReason::TopologyMissing,
            };
        }
        if self.validate().is_err() {
            return SchedGate::RejectWholeBlock {
                reason: SchedRejectReason::PlanInvalid,
            };
        }
        if self.lost_worker_mask(topo) != 0 {
            return SchedGate::RejectWholeBlock {
                reason: SchedRejectReason::LostWorkerCpu,
            };
        }
        let mut deps = self.union_irq_dependency();
        while deps != 0 {
            let bit = deps & deps.wrapping_neg();
            match topo.irq_cpu_for_bit(bit) {
                Some(irq_cpu) => {
                    if !topo.cpu_online(irq_cpu) || !topo.cpu_allowed(irq_cpu) {
                        return SchedGate::RejectWholeBlock {
                            reason: SchedRejectReason::LostWakeupIrq,
                        };
                    }
                    if self.cpu_in_plan(irq_cpu) {
                        return SchedGate::RejectWholeBlock {
                            reason: SchedRejectReason::IrqOnDspCpu,
                        };
                    }
                }
                None => {
                    return SchedGate::RejectWholeBlock {
                        reason: SchedRejectReason::LostWakeupIrq,
                    }
                }
            }
            deps ^= bit;
        }
        if self.validate_against(topo).is_err() {
            return SchedGate::RejectWholeBlock {
                reason: SchedRejectReason::StaleTopology,
            };
        }
        SchedGate::Admit
    }

    /// 失核后的**独立 2 worker profile 重新准入**（唯一允许的降级路径）。
    ///
    /// 前置：self 是 4 worker 计划且确有失核；候选 CPU 取当前计划中仍 online+allowed 的前两个
    /// （保持 worker 0/1 的实测顺序），并沿用 req 的前两条 IRQ 依赖。成功返回**新计划**（新
    /// expected_workers=2 / ACK_MASK(2)=0x3 / 不同 affinity_hash，供新 epoch 使用）；不足 2 个
    /// 健康 CPU 或拓扑不满足 = Err(ERR_ADMISSION_SCHED)，调用方只能 last-good/mute，禁止旧图
    /// 半播。无失核或非 4 worker 计划返回 ERR_SWITCH_BUSY（不是降级路径）。
    pub fn two_worker_re_admission(
        &self,
        req: &SchedRequest,
        topo: &TopologySnapshot,
    ) -> Result<Self, RtStatus> {
        self.validate()?;
        if self.workers != RT_WORKERS {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if self.lost_worker_mask(topo) == 0 {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        let mut healthy = [CPU_UNUSED; DSP_CPU_MAX];
        let mut count = 0usize;
        let mut k = 0usize;
        while k < self.workers as usize {
            let cpu = self.affinity[k].cpu_index;
            if topo.cpu_online(cpu) && topo.cpu_allowed(cpu) && count < DSP_CPU_MAX {
                healthy[count] = cpu;
                count += 1;
            }
            k += 1;
        }
        if count < RT_WORKERS_FALLBACK as usize {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut degraded = *req;
        degraded.workers = RT_WORKERS_FALLBACK;
        degraded.dsp_cpus = [healthy[0], healthy[1], CPU_UNUSED, CPU_UNUSED];
        degraded.irq_dependency = [
            req.irq_dependency[0],
            req.irq_dependency[1],
            IRQ_DEPENDENCY_NONE,
            IRQ_DEPENDENCY_NONE,
        ];
        let plan = plan(&degraded, topo)?;
        if plan.affinity_hash == self.affinity_hash {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        Ok(plan)
    }

    /// §12.3 sched 报告字段子集（供 T26 合并 AdmissionReport；crossfade 归 T20）。
    pub fn report(&self) -> SchedAdmissionReport {
        let mut actual_cpu = [CPU_UNUSED; DSP_CPU_MAX];
        let mut sched_priority = [0u8; DSP_CPU_MAX];
        let mut k = 0usize;
        while k < self.workers as usize {
            actual_cpu[k] = self.affinity[k].cpu_index;
            sched_priority[k] = self.affinity[k].sched_priority;
            k += 1;
        }
        SchedAdmissionReport {
            plan_version: self.plan_version,
            workers: self.workers,
            expected_workers: self.expected_workers(),
            ack_mask: self.ack_mask(),
            actual_cpu,
            sched_priority,
            irq_dependency_mask: self.union_irq_dependency(),
            tick_freq_hz: self.tick_freq_hz,
            spin_max_iters: self.spin_max_iters,
            cutoff_drain_iters: self.cutoff_drain_iters,
            affinity_hash: self.affinity_hash,
            two_worker_profile: self.is_two_worker_profile(),
            crossfade_accepted: false,
            kernel_writes_performed: KERNEL_WRITES_PERFORMED,
        }
    }

    /// 按 §11.10/§11.13 打包 affinity 表（workers × 32 B，worker_id 升序）到 out；
    /// 返回写入字节数。out 不足 = ERR_CAPACITY。
    pub fn pack_affinity_table(&self, out: &mut [u8]) -> Result<usize, RtStatus> {
        let needed = self.affinity_table_len();
        if out.len() < needed {
            return Err(RtStatus::ERR_CAPACITY);
        }
        let mut k = 0usize;
        while k < self.workers as usize {
            let bytes = self.affinity[k].to_bytes();
            let offset = k * AFFINITY_ENTRY_BYTES;
            out[offset..offset + AFFINITY_ENTRY_BYTES].copy_from_slice(&bytes);
            k += 1;
        }
        Ok(needed)
    }

    /// 与 plan 头（v1.1.2 errata E1：affinity_table_offset u32@96、affinity_count u16@100）
    /// 的 workers/affinity_count 及 §12.3 affinity_hash 一致性（T12/T26 打包校验用）。
    pub fn matches_plan_header(
        &self,
        workers: u8,
        affinity_count: u16,
        affinity_hash: [u8; AFFINITY_HASH_BYTES],
    ) -> bool {
        self.workers == workers
            && self.affinity_count() == affinity_count
            && self.affinity_hash == affinity_hash
    }
}

/// 整块入口判决（RejectWholeBlock = playback 不得读任何 partial；整图停流重准入/mute）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedGate {
    /// 本块可按本计划执行（CPU/IRQ 依赖仍在位）。
    Admit,
    /// 整块/整图拒收：不得输出半图或部分 worker 结果。
    RejectWholeBlock {
        /// 拒收原因（全部映射到 ERR_ADMISSION_SCHED=115）。
        reason: SchedRejectReason,
    },
}

impl SchedGate {
    /// 是否允许执行本块。
    pub const fn is_admit(self) -> bool {
        match self {
            SchedGate::Admit => true,
            SchedGate::RejectWholeBlock { .. } => false,
        }
    }

    /// 拒收理由（Admit 时返回 None）。
    pub const fn reject_reason(self) -> Option<SchedRejectReason> {
        match self {
            SchedGate::Admit => None,
            SchedGate::RejectWholeBlock { reason } => Some(reason),
        }
    }

    /// 判决对应的冻结错误码（Admit = OK）。
    pub const fn status(self) -> RtStatus {
        match self {
            SchedGate::Admit => RtStatus::OK,
            SchedGate::RejectWholeBlock { .. } => RtStatus::ERR_ADMISSION_SCHED,
        }
    }
}

/// 整块拒收原因（冻结枚举；不新增 RtStatus 码）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedRejectReason {
    /// 拓扑缺实测证据（allowed/online 等未采集或互相矛盾）。
    TopologyMissing,
    /// 计划自身不变量损坏（hash/表项/标定）。
    PlanInvalid,
    /// 计划内 worker 的 CPU 已离线/不在允许集合。
    LostWorkerCpu,
    /// 依赖 IRQ 的实测 CPU 缺失/离线/不在允许集合。
    LostWakeupIrq,
    /// 依赖 IRQ 与 DSP CPU 同核（FIFO 忙等可能饿死供数 IRQ）。
    IrqOnDspCpu,
    /// 拓扑相对准入时变化（隔离状态/core 映射/集合不一致）。
    StaleTopology,
}

impl SchedRejectReason {
    /// 冻结错误码（全部 ERR_ADMISSION_SCHED=115；不新增码）。
    pub const fn status(self) -> RtStatus {
        RtStatus::ERR_ADMISSION_SCHED
    }
}

/// §12.3 AdmissionReport 的 sched 字段子集（T45 产出；T18/T26 合并时不得少算）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedAdmissionReport {
    /// plan 版本。
    pub plan_version: u32,
    /// worker 数。
    pub workers: u8,
    /// expected_workers（2/4）。
    pub expected_workers: u8,
    /// ACK_MASK(expected_workers)。
    pub ack_mask: u32,
    /// 逐 worker 实际 CPU（未用槽 CPU_UNUSED）。
    pub actual_cpu: [u16; DSP_CPU_MAX],
    /// 逐 worker FIFO 优先级（未用槽 0）。
    pub sched_priority: [u8; DSP_CPU_MAX],
    /// 依赖 IRQ 并集（§11.13 位义，D-03）。
    pub irq_dependency_mask: u64,
    /// tick 标定频率（T19）。
    pub tick_freq_hz: u32,
    /// spin 标定值（T18）。
    pub spin_max_iters: u32,
    /// cutoff drain 标定值（T18）。
    pub cutoff_drain_iters: u32,
    /// affinity 表 hash（D-04）。
    pub affinity_hash: [u8; AFFINITY_HASH_BYTES],
    /// 是否 2 worker 独立 profile。
    pub two_worker_profile: bool,
    /// crossfade 是否准入（归 T20/T26；T45 恒 false）。
    pub crossfade_accepted: bool,
    /// 本任务是否执行过内核写入（恒 false）。
    pub kernel_writes_performed: bool,
}

// ---------------------------------------------------------------------------
// 计划构建（唯一入口：实测拓扑 + 请求 → 参数化计划）
// ---------------------------------------------------------------------------

/// 由实测拓扑与准入请求构建调度计划（NONRT；不写内核）。
///
/// 失败语义（全部冻结码，不 warn 降级）：profile 非 4/2、优先级排序破坏、tick/spin/cutoff 未
/// 标定、DSP CPU 未配置/离线/不允许/重复/SMT 同核、IRQ 依赖位未知/缺实测亲和/与 DSP CPU 同核
/// → ERR_ADMISSION_SCHED(115)。成功返回自校验通过的计划。
pub fn plan(req: &SchedRequest, topo: &TopologySnapshot) -> Result<SchedPlan, RtStatus> {
    req.validate()?;
    topo.validate()?;
    let workers = req.workers as usize;
    let mut affinity = [TspiWorkerAffinityV1::ZERO; WORKER_AFFINITY_COUNT];
    let mut k = 0usize;
    while k < workers {
        let cpu = req.dsp_cpus[k];
        if !topo.cpu_online(cpu) || !topo.cpu_allowed(cpu) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let core = topo.cpu_core(cpu);
        if core != CPU_UNKNOWN_CORE {
            let mut j = 0usize;
            while j < k {
                if topo.cpu_core(affinity[j].cpu_index) == core {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                j += 1;
            }
        }
        let deps = req.irq_dependency[k];
        if !topo.has_irq_evidence(deps) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        let mut pending = deps;
        while pending != 0 {
            let bit = pending & pending.wrapping_neg();
            let irq_cpu = match topo.irq_cpu_for_bit(bit) {
                Some(value) => value,
                None => return Err(RtStatus::ERR_ADMISSION_SCHED),
            };
            if !topo.cpu_online(irq_cpu) || !topo.cpu_allowed(irq_cpu) {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            let mut dsp_index = 0usize;
            while dsp_index < workers {
                if req.dsp_cpus[dsp_index] == irq_cpu {
                    return Err(RtStatus::ERR_ADMISSION_SCHED);
                }
                dsp_index += 1;
            }
            pending ^= bit;
        }
        let mut flags = AFFINITY_FLAG_RT;
        if topo.cpu_isolated(cpu) {
            flags |= AFFINITY_FLAG_ISOLATED_CPU;
        }
        affinity[k] = TspiWorkerAffinityV1::new(k as u8, req.dsp_priority, flags, cpu, deps);
        k += 1;
    }
    let mut built = SchedPlan {
        plan_version: req.plan_version,
        workers: req.workers,
        affinity,
        dsp_priority: req.dsp_priority,
        io_priority: req.io_priority,
        irq_priority: req.irq_priority,
        tick_freq_hz: req.tick_freq_hz,
        tick_read_budget: req.tick_read_budget,
        spin_max_iters: req.spin_max_iters,
        cutoff_drain_iters: req.cutoff_drain_iters,
        affinity_hash: [0u8; AFFINITY_HASH_BYTES],
    };
    built.affinity_hash = compute_affinity_hash(built.workers, &built.affinity);
    built.validate()?;
    Ok(built)
}

/// §12.3 同名入口的 leaf 适配版：返回 [TspiWorkerAffinityV1;4] 亲和表（D-01 说明签名差异）。
pub fn plan_affinity(
    req: &SchedRequest,
    topo: &TopologySnapshot,
) -> Result<[TspiWorkerAffinityV1; WORKER_AFFINITY_COUNT], RtStatus> {
    let built = plan(req, topo)?;
    Ok(built.affinity)
}

/// T45 明确**不写内核**：任何“运行时自动应用计划”的调用都被拒绝（ERR_UNSUPPORTED=122）。
///
/// 亲和/优先级/IRQ 亲和/rt_runtime 的真实应用只在 T26 host 启动期一次性执行（WBS T45 DoD），
/// 本模块不提供也永远不自动触发该动作。
pub fn apply_plan_to_kernel(_plan: &SchedPlan) -> Result<(), RtStatus> {
    Err(RtStatus::ERR_UNSUPPORTED)
}

// ---------------------------------------------------------------------------
// affinity hash（D-04：域分隔 FNV-1a-64，覆盖 workers 数 + 有效表项字节）
// ---------------------------------------------------------------------------

fn fnv1a64_byte(hash: u64, byte: u8) -> u64 {
    (hash ^ byte as u64).wrapping_mul(FNV1A64_PRIME)
}

/// 计算 affinity 表规范化 hash（8 B LE；含 workers 数，2/4 profile 必不同）。
pub fn compute_affinity_hash(
    workers: u8,
    entries: &[TspiWorkerAffinityV1; WORKER_AFFINITY_COUNT],
) -> [u8; AFFINITY_HASH_BYTES] {
    let mut hash = FNV1A64_OFFSET_BASIS;
    let mut index = 0usize;
    while index < AFFINITY_HASH_DOMAIN.len() {
        hash = fnv1a64_byte(hash, AFFINITY_HASH_DOMAIN[index]);
        index += 1;
    }
    hash = fnv1a64_byte(hash, workers);
    let mut k = 0usize;
    while k < workers as usize && k < WORKER_AFFINITY_COUNT {
        let bytes = entries[k].to_bytes();
        let mut i = 0usize;
        while i < AFFINITY_ENTRY_BYTES {
            hash = fnv1a64_byte(hash, bytes[i]);
            i += 1;
        }
        k += 1;
    }
    hash.to_le_bytes()
}

// ---------------------------------------------------------------------------
// 编译期断言（§11.13 布局 / §13.2 needs_drop / 容量）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(size_of::<TspiWorkerAffinityV1>() == AFFINITY_ENTRY_BYTES);
    assert!(align_of::<TspiWorkerAffinityV1>() == 8);
    assert!(!needs_drop::<TspiWorkerAffinityV1>());
    assert!(!needs_drop::<TopologySnapshot>());
    assert!(!needs_drop::<LscpuSnapshot>());
    assert!(!needs_drop::<SchedRequest>());
    assert!(!needs_drop::<SchedPlan>());
    assert!(!needs_drop::<SchedGate>());
    assert!(!needs_drop::<SchedRejectReason>());
    assert!(!needs_drop::<SchedAdmissionReport>());
    assert!(DSP_CPU_MAX == 4);
    assert!(WORKER_AFFINITY_COUNT == 4);
    assert!(AFFINITY_TABLE_MAX_BYTES == 128);
    assert!(IRQ_BIT_KNOWN_MASK == 0x1F);
    assert!(TICK_READ_BUDGET_MAX == 3);
};

// ---------------------------------------------------------------------------
// 私有测试（T45；文件在 crates/rt-host/tests/T45/，经 include! 进 lib 单测构建；
// 公共注册属 integrator，勿写入 tests/public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scheduler_fake_tests {
    include!("../tests/T45/scheduler_fake.rs");
}
