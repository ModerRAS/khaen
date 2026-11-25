//! [WD]/[NONRT] fault —— 有界直方图 / 另核 watchdog / fallback 图预分配准入 / 安全 mute + quarantine —— WBS T21 写域。
//!
//! 权威契约：rt-stack/spec/core-v1.md（下称 core-v1）§5.5（SafetyMode 与 fallback）、§7.5/§7.6
//! （retire gate、quarantine 与 ack 槽耗尽）、§8.3/§8.4（事件环、计数、有界直方图）、§9.2
//! （错误码→动作）、§11.13（RT 共享控制结构）、§13（无 RT 析构/D-08/D-09）、附录 T21 行；
//! WBS §6 的 RT-first 故障行；决策 DSP-HOST-DECISION “故障”行（有界 histogram、独立核 watchdog、
//! 过载/失效切 G identity 仍保 P 最终保护、无法安全输出则 mute、保隔离内存不回收、fallback graph
//! 预分配且 deadline 准入、绝不 partial/绕保护）。
//!
//! ## 分区标注（core-v1 §12.2）
//!
//! | 区 | 本文件内容 | 纪律 |
//! | --- | --- | --- |
//! | [WD] watchdog | Watchdog：只读 heartbeat/epoch/ack/worker 原子计数，只置位 safety_request；不释放、不分配、不 panic | 独立核 SCHED_OTHER（§4.1） |
//! | [RT] 记录路径 | FaultCounters/HistogramTable/QuarantinePool/EventRingLane::push：固定数组 + 原子，无分配/无锁/无 syscall/无 panic | 只写 Relaxed，发布 Release |
//! | [NONRT] | admit_fallback、SafetyState（control 串行迁移）、FaultSupervisor 的 mute/quarantine 释放/恢复、write_export 导出 | 可格式化，不得把 Drop 带进 RT |
//!
//! 本文件**不是** RT-MODULE 纯模块（同时含 [WD]/[RT]/[NONRT]）：rt-check 的 forbid 文件级扫描不覆盖，
//! 由代码评审 + drop_audit 类型清单（全部类型 needs_drop == false）+ 本文件测试覆盖（§13.6 T21）。
//!
//! ## T21 DoD → 实现映射（WBS T21 / task-108）
//!
//! | DoD 项 | 实现 |
//! | --- | --- |
//! | bounded histogram（§8.4：6 metric × 4 worker × 32 桶，溢出桶 = 第 32 桶） | HistMetric/HistogramConfig/HistogramTable/HistogramSnapshot；固定内存 6*4*32*4 = 3072 B（与 admission ControlPlane 计费一致），桶边界 prepare 写死、导出带 ticks→ns |
//! | 另核 watchdog（§4.1/§3.2） | WatchdogConfig 校验观测核 ≠ 任一 DSP worker 核（ERR_ADMISSION_SCHED），WATCHDOG_PERIOD_MS/WATCHDOG_STALE_BLOCKS；只置位 safety_request，latch 后不自动恢复 |
//! | fallback graph 预分配且准入（§5.5） | admit_fallback：第二 PlanArena（GraphPending）容量、预分配安全静音 buffer、identity G + 保留 P_post、worst deadline ≤ T_block−io_tail 全部准入；失败即拒绝，不在超时后重算昂贵 P |
//! | 未 ack / 失核安全 mute + quarantine 不可释放（§7.5/§7.6/D-08/D-09） | QuarantinePool：all-ack 前 release 拒绝（RT_E_SLOT_STATE）；worker 失联/失核 latch 后 release 永久返回 RT_E_WORKER_LOST、retained 不减、无强制释放 API；SafetyState 进入 SilenceMute/Faulted 后 block_start_allowed()==false |
//! | 非 RT 导出（§8.4 导出规则） | FaultSupervisor::write_export + drain_events：带 graph_id/plan_version/epoch_id/采样窗口、桶边界与单位；RT 内不写文件/网络 |
//! | 与 swap/admission 接口对齐 | admission：AdmissionEnv/AdmissionReport/MemoryPlan/RegionId；swap/T20：plan_version、WatchdogSample.swap_ack_mask（§11.13 ack 位图）、note_swap/note_retire 计数、retire gate 卡住→禁止释放 quarantine |
//!
//! ## 失败语义（fail closed）
//!
//! - admission/fallback 准入/参数校验任一失败 → 冻结 RtStatus，不产生 FallbackAdmission；
//! - watchdog 观测到心跳/worker/ack/设备异常 → 只置位 safety_request 并返回动作，由 control 串行执行迁移；
//! - SafetyState 只允许升级（Normal→ProtectionFallback→SilenceMute→Faulted）；恢复只接受
//!   ReAdmissionTicket（只能由一次成功的 AdmissionReport 生成），禁止自动恢复 Running（§5.1 硬规则）；
//! - quarantine 在 worker 失联/失核后永久保留（mlock 不回收），等进程重启（§7.6/INV-L5）。
//!
//! ## 测试注册（同 T16/T17/T18）
//!
//! cargo 只自动发现 tests/*.rs；tests/T21/ 子目录经本文件末尾 #[cfg(test)] mod fault_tests {
//! include!(...) } 进入单测构建。integrator **不要**再把 tests/T21 写进 public_registration.rs（会重复注册）。
//! 运行：cargo test -p khaen-rt-host fault（executed test_count > 0）。
//!
//! 状态：leaf 实现 + Windows 本地真实 cargo test；真实 watchdog 线程/独立核亲和/板端 RT 实测 NOT_RUN（evidence/T21）。

use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub use khaen_core::abi::TspiEventV1;
pub use khaen_core::events::{
    EV_ACK_SLOT_EXHAUSTED, EV_FALLBACK_ENTER, EV_FALLBACK_EXIT, EV_FAULT, EV_QUARANTINE_ENTER,
    EV_QUARANTINE_EXHAUSTED, EV_QUARANTINE_RELEASE, EV_SAFETY_MUTE_ENTER, EV_SAFETY_MUTE_EXIT,
    EV_WATCHDOG_MUTE, EV_WORKER_LOST, LANE_CAPTURE, LANE_CONTROL, LANE_PLAYBACK, LANE_WATCHDOG,
    LANE_WORKER0, LANE_WORKER1, LANE_WORKER2, LANE_WORKER3, SEVERITY_ERROR, SEVERITY_FATAL,
    SEVERITY_INFO, SEVERITY_WARNING,
};
pub use khaen_core::rt::{
    gate_ns_floor, t_block_ns_ceil, PlanVersion, RtControl, RtStatus, ACK_MASK_ALL, BLOCK_SIZES,
    EVENT_LANES, EVENT_LANE_SLOTS, GRAPH_ID_BYTES, HIST_BUCKETS, IO_TAIL_ALLOWANCE_NS_DEFAULT,
    MAX_OUTPUT_CHANNELS, QUARANTINE_SLOTS, RT_WORKERS, RT_WORKERS_FALLBACK, SAMPLE_RATES_HZ,
    WATCHDOG_PERIOD_MS, WATCHDOG_STALE_BLOCKS,
};
use khaen_core::workers::{ack_mask, TspiRtControlV1, RT_E_ACK_SLOT_EXHAUSTED};

use crate::admission::{AdmissionEnv, AdmissionReport, MemoryPlan, PLAN_HEADER_BYTES};

/// 生产 worker 数（数组长度常量；= core-v1 §3.3 RT_WORKERS）。
pub const HIST_WORKERS: usize = 4;
/// 直方图 metric 数（core-v1 §8.4：block_tick/spin/io read/io write/queue/phase_gap 共 6）。
pub const HIST_METRIC_COUNT: usize = 6;
/// 直方图固定字节数（core-v1 §8.4 布局；= admission ControlPlane 计费口径 6*4*32*4）。
pub const HISTOGRAM_BYTES: usize = HIST_METRIC_COUNT * HIST_WORKERS * HIST_BUCKETS * 4;
/// 每 worker 事件 lane 数（§8.3：8 lane）。
pub const EVENT_RING_LANES: usize = EVENT_LANES;
/// worker 失联判定的连续采样次数默认值（watchdog 10 ms 采样，>= WATCHDOG_STALE_BLOCKS）。
pub const WATCHDOG_WORKER_LOST_SAMPLES_DEFAULT: u32 = WATCHDOG_STALE_BLOCKS;
/// retire gate swap-ack 未完成的连续采样次数默认值（未 ack 不允许释放/复用）。
pub const WATCHDOG_RETIRE_ACK_SAMPLES_DEFAULT: u32 = WATCHDOG_STALE_BLOCKS;
/// 安全静音样本字节（f32，§5.5 预分配全零输出块）。
pub const SAFETY_SAMPLE_BYTES: u64 = 4;

// ---------------------------------------------------------------------------
// 安全动作 / 安全状态（§5.5；T06 scaffold 类型名与语义保持）
// ---------------------------------------------------------------------------

/// 安全动作（repr(u8) POD，无 payload/Drop；§5.5）。
///
/// 输出侧据此选择：Normal 正常工作图；ProtectionFallback 预分配 fallback 图（G=I 且保留
/// P_post 最终保护）；SilenceMute 预分配全零输出（保持 block 节奏与设备时钟）。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyAction {
    /// 正常工作图。
    Normal = 0,
    /// G=I + 保留 P_post 最终保护。
    ProtectionFallback = 1,
    /// 预分配全零输出（保持 block 节奏）。
    SilenceMute = 2,
}

/// 安全模式（§5.5 SafetyMode + §5.1 Faulted；数值单调升级）。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SafetyMode {
    /// 正常工作图。
    Normal = 0,
    /// 保护 fallback（G=I，仍执行 P_post）。
    ProtectionFallback = 1,
    /// 安全静音（只输出预分配全零块；不处理新块）。
    SilenceMute = 2,
    /// 不可恢复故障（worker 失联/ABI/不变量/内存/quarantine 耗尽/ack 槽耗尽）。
    Faulted = 3,
}

impl SafetyMode {
    /// 稳定名称（事件/导出用；不含分配）。
    pub const fn name(self) -> &'static str {
        match self {
            SafetyMode::Normal => "Normal",
            SafetyMode::ProtectionFallback => "ProtectionFallback",
            SafetyMode::SilenceMute => "SilenceMute",
            SafetyMode::Faulted => "Faulted",
        }
    }

    /// 是否处于需要安全输出（mute/fallback）的降级态。
    pub const fn is_degraded(self) -> bool {
        !matches!(self, SafetyMode::Normal)
    }

    /// SafetyAction 映射（输出侧选择预分配图/静音块）。
    pub const fn action(self) -> SafetyAction {
        match self {
            SafetyMode::Normal => SafetyAction::Normal,
            SafetyMode::ProtectionFallback => SafetyAction::ProtectionFallback,
            SafetyMode::SilenceMute | SafetyMode::Faulted => SafetyAction::SilenceMute,
        }
    }
}

/// 安全迁移请求者（§5.1/§9.3：只有 master/control/watchdog 可请求 SafetyMute；
/// worker 只能报本 worker 局部错误，不得直接改全局安全状态）。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardRequester {
    /// master（worker0）。
    Master = 0,
    /// control（REST/WS/prepare/retire）。
    Control = 1,
    /// 独立核 watchdog。
    Watchdog = 2,
    /// DSP worker（只允许上报，不允许请求全局 mute/fault）。
    Worker = 3,
}

impl GuardRequester {
    /// 是否允许请求全局安全动作（§9.3：worker 不允许）。
    pub const fn can_request_global_safety(self) -> bool {
        !matches!(self, GuardRequester::Worker)
    }

    /// 稳定名称（导出用）。
    pub const fn name(self) -> &'static str {
        match self {
            GuardRequester::Master => "master",
            GuardRequester::Control => "control",
            GuardRequester::Watchdog => "watchdog",
            GuardRequester::Worker => "worker",
        }
    }
}

/// watchdog 观察结果（只置位请求；具体迁移由 control/master 执行，§5.1/§12.2 [WD]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchdogAction {
    /// 无动作。
    None,
    /// 请求安全 mute（预分配静音/保护 fallback）。
    RequestMute(RtStatus),
    /// 请求 fault（不自动恢复；quarantine 不回收）。
    Fault(RtStatus),
}

impl WatchdogAction {
    /// 无动作常量。
    pub const NONE: Self = Self::None;

    /// 动作携带的原因码（None → OK）。
    pub const fn reason(self) -> RtStatus {
        match self {
            WatchdogAction::None => RtStatus::OK,
            WatchdogAction::RequestMute(status) | WatchdogAction::Fault(status) => status,
        }
    }

    /// 是否要求 fault（不可自动恢复）。
    pub const fn is_fault(self) -> bool {
        matches!(self, WatchdogAction::Fault(_))
    }
}

/// 安全迁移请求（control 串行执行；§5.1 迁移表 + 优先级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyRequest {
    /// 进入 ProtectionFallback（需 fallback 图已预分配且准入）。
    ProtectionFallback {
        /// 原因码（如 RT_E_LATE_STREAK）。
        reason: RtStatus,
        /// 请求者。
        requester: GuardRequester,
    },
    /// 进入 SilenceMute（需预分配安全静音 buffer）。
    SilenceMute {
        /// 原因码（如 RT_E_WATCHDOG / RT_E_QUARANTINE_FULL）。
        reason: RtStatus,
        /// 请求者。
        requester: GuardRequester,
    },
    /// 进入 Faulted（不可恢复；quarantine 永久保留）。
    Fault {
        /// 原因码（如 RT_E_WORKER_LOST）。
        reason: RtStatus,
        /// 请求者。
        requester: GuardRequester,
    },
}

impl SafetyRequest {
    /// 请求者。
    pub const fn requester(self) -> GuardRequester {
        match self {
            SafetyRequest::ProtectionFallback { requester, .. }
            | SafetyRequest::SilenceMute { requester, .. }
            | SafetyRequest::Fault { requester, .. } => requester,
        }
    }

    /// 请求原因码。
    pub const fn reason(self) -> RtStatus {
        match self {
            SafetyRequest::ProtectionFallback { reason, .. }
            | SafetyRequest::SilenceMute { reason, .. }
            | SafetyRequest::Fault { reason, .. } => reason,
        }
    }
}

/// 安全输出可用性（admission 预分配结果；决定 fallback/mute 可达性，§5.5/§9.2 307）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafetyContext {
    /// fallback 图已预分配且通过 worst deadline/容量准入。
    pub fallback_ready: bool,
    /// 预分配全零静音块可用（admission 的 Arena.safety_silence_bytes 足够）。
    pub silence_ready: bool,
}

impl SafetyContext {
    /// 无任何安全输出的最保守上下文（mute 会升级为 Faulted）。
    pub const NONE: Self = Self {
        fallback_ready: false,
        silence_ready: false,
    };
}

/// 安全迁移 key（§5.1 幂等/去重：(from,to,reason,epoch_id,plan_version)）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionKey {
    /// 迁移前模式。
    pub from: SafetyMode,
    /// 迁移后模式。
    pub to: SafetyMode,
    /// 原因码（RtStatus u32）。
    pub reason: u32,
    /// epoch。
    pub epoch_id: u64,
    /// plan 版本。
    pub plan_version: PlanVersion,
}

// ---------------------------------------------------------------------------
// 事件环（§8.3：固定 128 槽 + 原子序号；lane 内单写者）
// ---------------------------------------------------------------------------

/// 单 lane 事件环（固定 128 槽 + 原子序号；lane 内单写者）。
///
/// 单写者纪律：push(&mut self, ..) 只能由该 lane 的写者线程调用（worker k / capture /
/// playback / watchdog / control）；非 RT 读者用 try_pop(&self)/EventLaneReader。生产上 lane
/// header 与记录数组位于 RtArena（§8.3/§11.13），T26 在 RT 边界以独占指针切分；本 leaf 用
/// &mut/& 切分在模型/测试中表达同一纪律。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct EventRingLane {
    /// lane 编号（0..7）。
    pub lane: u8,
    /// 必须为 0。
    pub reserved: [u8; 7],
    /// 已写入序号（本 lane 写者）。
    pub write_seq: AtomicU64,
    /// 已读序号（非 RT 读端）。
    pub read_seq: AtomicU64,
    /// 丢事件计数。
    pub dropped: AtomicU64,
    /// 固定槽（RT 只做按值拷贝）。
    pub slots: [TspiEventV1; EVENT_LANE_SLOTS],
}

impl EventRingLane {
    /// 构造空 lane（admission 前/测试）。
    pub const fn new(lane: u8) -> Self {
        Self {
            lane,
            reserved: [0; 7],
            write_seq: AtomicU64::new(0),
            read_seq: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            slots: [TspiEventV1::ZERO; EVENT_LANE_SLOTS],
        }
    }

    /// 已发布（写入）的事件序号（下一槽）。
    pub fn next_seq(&self) -> u64 {
        self.write_seq.load(Ordering::Acquire)
    }

    /// 当前可读事件数（非 RT 诊断）。
    pub fn readable(&self) -> u64 {
        let write = self.write_seq.load(Ordering::Acquire);
        let read = self.read_seq.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }

    /// [RT/IO/WD] 写入一条事件（§8.3 配方：满则 dropped++ 并丢弃；先写记录后 Release 发布序号）。
    ///
    /// 返回 false 表示丢弃（满）。单写者：本方法独占 &mut self。
    pub fn push(&mut self, event: &TspiEventV1) -> bool {
        let seq = self.write_seq.load(Ordering::Relaxed);
        let read = self.read_seq.load(Ordering::Acquire);
        if seq.wrapping_sub(read) >= EVENT_LANE_SLOTS as u64 {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let index = (seq as usize) % EVENT_LANE_SLOTS;
        let record = TspiEventV1 {
            seq,
            lane: self.lane,
            ..*event
        };
        match self.slots.get_mut(index) {
            Some(slot) => *slot = record,
            None => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        self.write_seq.store(seq.wrapping_add(1), Ordering::Release);
        true
    }

    /// [NONRT] 读一条事件（Acquire 读 write_seq、Release 推进 read_seq；未读槽不会被写者复用）。
    pub fn try_pop(&self) -> Option<TspiEventV1> {
        let write = self.write_seq.load(Ordering::Acquire);
        let read = self.read_seq.load(Ordering::Relaxed);
        if write.wrapping_sub(read) == 0 {
            return None;
        }
        let index = (read as usize) % EVENT_LANE_SLOTS;
        let record = match self.slots.get(index) {
            Some(event) => *event,
            None => return None,
        };
        self.read_seq.store(read.wrapping_add(1), Ordering::Release);
        Some(record)
    }

    /// [NONRT] 当前丢弃事件数。
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// 8 个事件 lane 的固定集合（§8.3：8 × 128 槽 = 1024 × 64 B）。
///
/// 尺寸与 admission ControlPlane 计费一致：8×(64 B header + 128×64 B 记录) = 66 048 B
/// （见编译期断言）。EventLanes 只用 &mut lane / & lane 切分写者/读者。
pub struct EventLanes {
    lanes: [EventRingLane; EVENT_RING_LANES],
}

impl EventLanes {
    /// 构造 8 个空 lane（admission 前/测试）。
    pub fn new() -> Self {
        Self {
            lanes: core::array::from_fn(|index| EventRingLane::new(index as u8)),
        }
    }

    /// 读 lane 头（诊断/导出）。
    pub fn lane(&self, lane: u8) -> Option<&EventRingLane> {
        self.lanes.get(lane as usize)
    }

    /// 写者取独占 lane（RT 写者只拿自己 lane）。
    pub fn lane_mut(&mut self, lane: u8) -> Option<&mut EventRingLane> {
        self.lanes.get_mut(lane as usize)
    }

    /// [RT/IO/WD] 写一条事件到其 event.lane；返回 false=丢弃（lane 满/编号非法）。
    pub fn push(&mut self, event: &TspiEventV1) -> bool {
        match self.lane_mut(event.lane) {
            Some(lane) => lane.push(event),
            None => false,
        }
    }

    /// [NONRT] 汇总 8 lane 的丢事件计数。
    pub fn dropped_total(&self) -> u64 {
        let mut total = 0u64;
        for lane in self.lanes.iter() {
            total = total.saturating_add(lane.dropped());
        }
        total
    }
}

impl Default for EventLanes {
    fn default() -> Self {
        Self::new()
    }
}

/// [NONRT] 单 lane 读者（导出用；与写者单写者纪律一致）。
#[derive(Clone, Copy)]
pub struct EventLaneReader<'a> {
    lane: &'a EventRingLane,
}

impl<'a> EventLaneReader<'a> {
    /// 由 lane 共享引用构造读者。
    pub const fn new(lane: &'a EventRingLane) -> Self {
        Self { lane }
    }

    /// 读一条事件。
    pub fn next(&self) -> Option<TspiEventV1> {
        self.lane.try_pop()
    }

    /// lane 编号。
    pub const fn lane_id(&self) -> u8 {
        self.lane.lane
    }
}

/// [NONRT] 构造一条事件（lane 由写者头字段决定）。
fn make_event(
    lane: u8,
    kind: u16,
    severity: u8,
    epoch_id: u64,
    block_seq: u64,
    value_a: u32,
    value_b: u32,
) -> TspiEventV1 {
    let mut event = TspiEventV1::ZERO;
    event.lane = lane;
    event.kind = kind;
    event.severity = severity;
    event.epoch_id = epoch_id;
    event.block_seq = block_seq;
    event.value_a = value_a;
    event.value_b = value_b;
    event
}

/// [NONRT] 向指定 lane 写一条事件（内部 helper；调用者持有 &mut EventLanes）。
fn emit(
    events: &mut EventLanes,
    lane: u8,
    kind: u16,
    severity: u8,
    epoch_id: u64,
    block_seq: u64,
    value_a: u32,
    value_b: u32,
) -> bool {
    let event = make_event(lane, kind, severity, epoch_id, block_seq, value_a, value_b);
    match events.lane_mut(lane) {
        Some(writer) => writer.push(&event),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// 有界直方图（§3.3 HIST_BUCKETS=32、§8.4 hist[metric][worker][32]）
// ---------------------------------------------------------------------------

/// 直方图 metric（core-v1 §8.4：6 个 metric × 4 worker × 32 桶）。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistMetric {
    /// 每 block master 计算 tick（block_ticks）。
    BlockTicks = 0,
    /// 有界等待/自旋 tick（spin_ticks）。
    SpinTicks = 1,
    /// I/O 读预算 tick（io_read_ticks）。
    IoReadTicks = 2,
    /// I/O 写预算 tick（io_write_ticks）。
    IoWriteTicks = 3,
    /// 环队列水位（queue_depth，单位 slots）。
    QueueDepth = 4,
    /// phase 间隔 tick（phase_gap_ticks）。
    PhaseGapTicks = 5,
}

impl HistMetric {
    /// metric 数（= HIST_METRIC_COUNT）。
    pub const COUNT: usize = HIST_METRIC_COUNT;
    /// 全部 metric（固定顺序 = §8.4 表顺序）。
    pub const ALL: [HistMetric; HIST_METRIC_COUNT] = [
        HistMetric::BlockTicks,
        HistMetric::SpinTicks,
        HistMetric::IoReadTicks,
        HistMetric::IoWriteTicks,
        HistMetric::QueueDepth,
        HistMetric::PhaseGapTicks,
    ];

    /// 数组下标。
    pub const fn index(self) -> usize {
        self as usize
    }

    /// 稳定名称（导出用；不含分配）。
    pub const fn name(self) -> &'static str {
        match self {
            HistMetric::BlockTicks => "block_ticks",
            HistMetric::SpinTicks => "spin_ticks",
            HistMetric::IoReadTicks => "io_read_ticks",
            HistMetric::IoWriteTicks => "io_write_ticks",
            HistMetric::QueueDepth => "queue_depth",
            HistMetric::PhaseGapTicks => "phase_gap_ticks",
        }
    }

    /// 单位（导出必须写清，§8.4：避免“每秒/每块”混用）。
    pub const fn unit(self) -> &'static str {
        match self {
            HistMetric::QueueDepth => "slots",
            _ => "ticks",
        }
    }
}

/// 直方图桶边界（prepare 写死；每 metric 31 个上界，值 < bound 落入该桶，否则进第 32 桶=溢出桶）。
///
/// 与 HistogramTable 分离存放：admission ControlPlane 只计 3072 B 的桶计数数组，
/// 边界属 prepare 元数据（非原子、RT 只读），不重复计费。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistogramConfig {
    bounds: [[u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT],
}

impl HistogramConfig {
    /// 均匀桶：bucket i 覆盖 [i*step, (i+1)*step)（i=0..30），>= 31*step 进溢出桶。
    pub fn uniform(step: u64) -> Result<Self, RtStatus> {
        if step == 0 {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        let mut bounds = [[0u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT];
        let mut metric = 0usize;
        while metric < HIST_METRIC_COUNT {
            let mut index = 0usize;
            while index < HIST_BUCKETS - 1 {
                let value = step
                    .checked_mul(index as u64 + 1)
                    .ok_or(RtStatus::ERR_CAPACITY)?;
                bounds[metric][index] = value;
                index += 1;
            }
            metric += 1;
        }
        Ok(Self { bounds })
    }

    /// 显式桶边界（每 metric 严格递增且非零；否则 ERR_BAD_LAYOUT）。
    pub fn from_bounds(
        bounds: [[u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT],
    ) -> Result<Self, RtStatus> {
        let mut metric = 0usize;
        while metric < HIST_METRIC_COUNT {
            let mut index = 0usize;
            while index < HIST_BUCKETS - 1 {
                let value = bounds[metric][index];
                if value == 0 {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                if index > 0 && value <= bounds[metric][index - 1] {
                    return Err(RtStatus::ERR_BAD_LAYOUT);
                }
                index += 1;
            }
            metric += 1;
        }
        Ok(Self { bounds })
    }

    /// 某 metric 的 31 个桶上界（只读）。
    pub const fn bounds(&self, metric: HistMetric) -> &[u64; HIST_BUCKETS - 1] {
        &self.bounds[metric.index()]
    }

    /// 全部 metric 的桶边界数组（快照/导出用）。
    pub const fn bounds_all(&self) -> &[[u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT] {
        &self.bounds
    }

    /// 值 → 桶下标（0..31；31 = 溢出桶）。
    pub fn bucket_of(&self, metric: HistMetric, value: u64) -> usize {
        let bounds = &self.bounds[metric.index()];
        let mut index = 0usize;
        while index < bounds.len() {
            if value < bounds[index] {
                return index;
            }
            index += 1;
        }
        HIST_BUCKETS - 1
    }
}

/// 有界直方图计数表（固定 HISTOGRAM_BYTES；RT Relaxed 写、非 RT Acquire 快照）。
#[repr(C, align(64))]
pub struct HistogramTable {
    buckets: [[[AtomicU32; HIST_BUCKETS]; HIST_WORKERS]; HIST_METRIC_COUNT],
}

impl HistogramTable {
    /// 全零表（admission 前/测试）。
    pub fn new() -> Self {
        Self {
            buckets: core::array::from_fn(|_metric| {
                core::array::from_fn(|_worker| core::array::from_fn(|_bucket| AtomicU32::new(0)))
            }),
        }
    }

    /// [RT] 记录一个样本（worker 必须 < RT_WORKERS；值必须已按 prepare 口径换算成 ticks/slots）。
    pub fn record(
        &self,
        config: &HistogramConfig,
        metric: HistMetric,
        worker: u8,
        value: u64,
    ) -> RtStatus {
        if worker as usize >= HIST_WORKERS {
            return RtStatus::RT_E_INVARIANT;
        }
        let bucket = config.bucket_of(metric, value);
        match self.buckets.get(metric.index()) {
            Some(by_worker) => match by_worker.get(worker as usize) {
                Some(counters) => match counters.get(bucket) {
                    Some(counter) => {
                        counter.fetch_add(1, Ordering::Relaxed);
                        RtStatus::OK
                    }
                    None => RtStatus::RT_E_INVARIANT,
                },
                None => RtStatus::RT_E_INVARIANT,
            },
            None => RtStatus::RT_E_INVARIANT,
        }
    }

    /// [NONRT] 单桶计数（诊断）。
    pub fn count(&self, metric: HistMetric, worker: u8, bucket: usize) -> u32 {
        if worker as usize >= HIST_WORKERS || bucket >= HIST_BUCKETS {
            return 0;
        }
        match self.buckets.get(metric.index()) {
            Some(by_worker) => match by_worker.get(worker as usize) {
                Some(counters) => match counters.get(bucket) {
                    Some(counter) => counter.load(Ordering::Acquire),
                    None => 0,
                },
                None => 0,
            },
            None => 0,
        }
    }

    /// [NONRT] Acquire 快照（允许轻微不一致；导出必须带 metadata/窗口）。
    pub fn snapshot(&self, config: &HistogramConfig) -> HistogramSnapshot {
        let mut buckets = [[[0u32; HIST_BUCKETS]; HIST_WORKERS]; HIST_METRIC_COUNT];
        let mut metric = 0usize;
        while metric < HIST_METRIC_COUNT {
            let mut worker = 0usize;
            while worker < HIST_WORKERS {
                let mut bucket = 0usize;
                while bucket < HIST_BUCKETS {
                    buckets[metric][worker][bucket] =
                        self.count(HistMetric::ALL[metric], worker as u8, bucket);
                    bucket += 1;
                }
                worker += 1;
            }
            metric += 1;
        }
        HistogramSnapshot {
            buckets,
            bounds: *config.bounds_all(),
        }
    }
}

impl Default for HistogramTable {
    fn default() -> Self {
        Self::new()
    }
}

/// 直方图快照（Copy POD；非 RT 导出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistogramSnapshot {
    /// [metric][worker][bucket] 计数。
    pub buckets: [[[u32; HIST_BUCKETS]; HIST_WORKERS]; HIST_METRIC_COUNT],
    /// [metric][31] 桶上界（ticks/slots）。
    pub bounds: [[u64; HIST_BUCKETS - 1]; HIST_METRIC_COUNT],
}

impl HistogramSnapshot {
    /// 某 metric/worker 的总样本数。
    pub fn total(&self, metric: HistMetric, worker: u8) -> u64 {
        if worker as usize >= HIST_WORKERS {
            return 0;
        }
        let mut total = 0u64;
        let mut bucket = 0usize;
        while bucket < HIST_BUCKETS {
            total =
                total.saturating_add(self.buckets[metric.index()][worker as usize][bucket] as u64);
            bucket += 1;
        }
        total
    }

    /// [NONRT] 导出直方图（含边界、单位与 ns 换算；§8.4 导出规则）。
    pub fn write_text<W: fmt::Write>(&self, tick_freq_hz: u32, out: &mut W) -> fmt::Result {
        for metric in HistMetric::ALL.iter() {
            write!(
                out,
                "hist.metric={} unit={} buckets={}\n",
                metric.name(),
                metric.unit(),
                HIST_BUCKETS
            )?;
            write!(out, "hist.bounds.{}=[", metric.name())?;
            let bounds = &self.bounds[metric.index()];
            let mut index = 0usize;
            while index < bounds.len() {
                if index > 0 {
                    write!(out, ",")?;
                }
                write!(out, "{}", bounds[index])?;
                index += 1;
            }
            write!(out, "] unit={}\n", metric.unit())?;
            let mut worker = 0usize;
            while worker < HIST_WORKERS {
                let mut bucket = 0usize;
                while bucket < HIST_BUCKETS {
                    let count = self.buckets[metric.index()][worker][bucket];
                    if count > 0 {
                        let lower = if bucket == 0 { 0 } else { bounds[bucket - 1] };
                        let upper = if bucket < bounds.len() {
                            bounds[bucket]
                        } else {
                            u64::MAX
                        };
                        write!(
                            out,
                            "hist.count metric={} worker={} bucket={} count={} range_{}=[{},{}",
                            metric.name(),
                            worker,
                            bucket,
                            count,
                            metric.unit(),
                            lower,
                            upper
                        )?;
                        if upper != u64::MAX {
                            write!(out, ")")?;
                        } else {
                            write!(out, "]")?;
                        }
                        if metric.unit() == "ticks" {
                            write!(
                                out,
                                " range_ns=[{},{}",
                                ticks_to_ns(lower, tick_freq_hz),
                                if upper == u64::MAX {
                                    u64::MAX
                                } else {
                                    ticks_to_ns(upper, tick_freq_hz)
                                }
                            )?;
                            if upper != u64::MAX {
                                write!(out, ")")?;
                            } else {
                                write!(out, "]")?;
                            }
                        }
                        write!(out, "\n")?;
                    }
                    bucket += 1;
                }
                worker += 1;
            }
        }
        Ok(())
    }
}

/// tick → ns 换算（§3.2 tick_freq_hz；tick_freq_hz=0 视为未标定，返回 0 并在报告注明）。
pub const fn ticks_to_ns(ticks: u64, tick_freq_hz: u32) -> u64 {
    if tick_freq_hz == 0 {
        return 0;
    }
    ((ticks as u128) * 1_000_000_000u128 / (tick_freq_hz as u128)) as u64
}

// ---------------------------------------------------------------------------
// 计数（§8.4 固定名字；RT Relaxed 写、非 RT Acquire 快照）
// ---------------------------------------------------------------------------

/// 故障/流统计计数（§8.4）。
#[derive(Debug)]
pub struct FaultCounters {
    blocks_done: AtomicU64,
    late_blocks: AtomicU64,
    cutoff_blocks: AtomicU64,
    deadline_gate_miss: AtomicU64,
    hard_late: AtomicU64,
    input_overruns: AtomicU64,
    output_underruns: AtomicU64,
    nonfinite_in: AtomicU64,
    nonfinite_out: AtomicU64,
    clipped_samples: AtomicU64,
    quarantine_entries: AtomicU64,
    quarantine_retained: AtomicU64,
    swap_count: AtomicU64,
    retire_count: AtomicU64,
    duplicate_transition_requests: AtomicU64,
    fallback_entries: AtomicU64,
    safety_mute_entries: AtomicU64,
    safety_mute_exits: AtomicU64,
    fault_entries: AtomicU64,
    ack_slot_exhausted: AtomicU64,
    quarantine_exhausted: AtomicU64,
    worker_lost: AtomicU64,
    watchdog_mutes: AtomicU64,
}

impl FaultCounters {
    /// 全零计数。
    pub const fn new() -> Self {
        Self {
            blocks_done: AtomicU64::new(0),
            late_blocks: AtomicU64::new(0),
            cutoff_blocks: AtomicU64::new(0),
            deadline_gate_miss: AtomicU64::new(0),
            hard_late: AtomicU64::new(0),
            input_overruns: AtomicU64::new(0),
            output_underruns: AtomicU64::new(0),
            nonfinite_in: AtomicU64::new(0),
            nonfinite_out: AtomicU64::new(0),
            clipped_samples: AtomicU64::new(0),
            quarantine_entries: AtomicU64::new(0),
            quarantine_retained: AtomicU64::new(0),
            swap_count: AtomicU64::new(0),
            retire_count: AtomicU64::new(0),
            duplicate_transition_requests: AtomicU64::new(0),
            fallback_entries: AtomicU64::new(0),
            safety_mute_entries: AtomicU64::new(0),
            safety_mute_exits: AtomicU64::new(0),
            fault_entries: AtomicU64::new(0),
            ack_slot_exhausted: AtomicU64::new(0),
            quarantine_exhausted: AtomicU64::new(0),
            worker_lost: AtomicU64::new(0),
            watchdog_mutes: AtomicU64::new(0),
        }
    }

    /// [RT] 完成 ack 的 block 数。
    pub fn record_block_done(&self) {
        self.blocks_done.fetch_add(1, Ordering::Relaxed);
    }

    /// [RT] worker 迟到导致 invalid 的块。
    pub fn record_late_block(&self) {
        self.late_blocks.fetch_add(1, Ordering::Relaxed);
    }

    /// [RT] cutoff=1 的块。
    pub fn record_cutoff_block(&self) {
        self.cutoff_blocks.fetch_add(1, Ordering::Relaxed);
    }

    /// [RT] compute_ticks 超 gate（验收门，不停流）。
    pub fn record_deadline_gate_miss(&self) {
        self.deadline_gate_miss.fetch_add(1, Ordering::Relaxed);
    }

    /// [RT] 超过 hard_cutoff_ns 的块。
    pub fn record_hard_late(&self) {
        self.hard_late.fetch_add(1, Ordering::Relaxed);
    }

    /// [IO] 环满丢块。
    pub fn record_input_overrun(&self) {
        self.input_overruns.fetch_add(1, Ordering::Relaxed);
    }

    /// [IO] 环空补静音。
    pub fn record_output_underrun(&self) {
        self.output_underruns.fetch_add(1, Ordering::Relaxed);
    }

    /// [IO] 输入 nonfinite 样本数（已 sanitize）。
    pub fn record_nonfinite_in(&self, samples: u64) {
        self.nonfinite_in.fetch_add(samples, Ordering::Relaxed);
    }

    /// [RT] 输出 nonfinite 样本数（应为 0，>0 即 fault）。
    pub fn record_nonfinite_out(&self, samples: u64) {
        self.nonfinite_out.fetch_add(samples, Ordering::Relaxed);
    }

    /// [IO] clip 样本数。
    pub fn record_clipped(&self, samples: u64) {
        self.clipped_samples.fetch_add(samples, Ordering::Relaxed);
    }

    /// quarantine 进入计数（与 QuarantinePool::entries 同步）。
    pub fn record_quarantine_enter(&self) {
        self.quarantine_entries.fetch_add(1, Ordering::Relaxed);
    }

    /// quarantine 进入/保留数同步（retained 由 pool 决定，不得由调用方自减）。
    pub fn set_quarantine_counts(&self, entries: u64, retained: u64) {
        self.quarantine_entries.store(entries, Ordering::Relaxed);
        self.quarantine_retained.store(retained, Ordering::Relaxed);
    }

    /// quarantine 池耗尽计数。
    pub fn set_quarantine_exhausted(&self, exhausted: u64) {
        self.quarantine_exhausted
            .store(exhausted, Ordering::Relaxed);
    }

    /// [NONRT] 成功 swap 计数（T20 调用）。
    pub fn record_swap(&self) {
        self.swap_count.fetch_add(1, Ordering::Relaxed);
    }

    /// [NONRT] 成功 retire 计数（T20 调用）。
    pub fn record_retire(&self) {
        self.retire_count.fetch_add(1, Ordering::Relaxed);
    }

    /// [NONRT] 重复 transition_key 请求（被去重）。
    pub fn record_duplicate_transition(&self) {
        self.duplicate_transition_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    /// fallback 进入计数。
    pub fn record_fallback_enter(&self) {
        self.fallback_entries.fetch_add(1, Ordering::Relaxed);
    }

    /// SafetyMute 进入计数。
    pub fn record_safety_mute_enter(&self) {
        self.safety_mute_entries.fetch_add(1, Ordering::Relaxed);
    }

    /// SafetyMute 退出（完整重新 admission 后）计数。
    pub fn record_safety_mute_exit(&self) {
        self.safety_mute_exits.fetch_add(1, Ordering::Relaxed);
    }

    /// Faulted 进入计数。
    pub fn record_fault_enter(&self) {
        self.fault_entries.fetch_add(1, Ordering::Relaxed);
    }

    /// ack 槽耗尽计数（§7.6）。
    pub fn record_ack_slot_exhausted(&self) {
        self.ack_slot_exhausted.fetch_add(1, Ordering::Relaxed);
    }

    /// worker 失联/失核计数。
    pub fn record_worker_lost(&self) {
        self.worker_lost.fetch_add(1, Ordering::Relaxed);
    }

    /// watchdog 触发 mute 计数。
    pub fn record_watchdog_mute(&self) {
        self.watchdog_mutes.fetch_add(1, Ordering::Relaxed);
    }

    /// [NONRT] Acquire 快照。
    pub fn snapshot(&self) -> CountersSnapshot {
        CountersSnapshot {
            blocks_done: self.blocks_done.load(Ordering::Acquire),
            late_blocks: self.late_blocks.load(Ordering::Acquire),
            cutoff_blocks: self.cutoff_blocks.load(Ordering::Acquire),
            deadline_gate_miss: self.deadline_gate_miss.load(Ordering::Acquire),
            hard_late: self.hard_late.load(Ordering::Acquire),
            input_overruns: self.input_overruns.load(Ordering::Acquire),
            output_underruns: self.output_underruns.load(Ordering::Acquire),
            nonfinite_in: self.nonfinite_in.load(Ordering::Acquire),
            nonfinite_out: self.nonfinite_out.load(Ordering::Acquire),
            clipped_samples: self.clipped_samples.load(Ordering::Acquire),
            quarantine_entries: self.quarantine_entries.load(Ordering::Acquire),
            quarantine_retained: self.quarantine_retained.load(Ordering::Acquire),
            swap_count: self.swap_count.load(Ordering::Acquire),
            retire_count: self.retire_count.load(Ordering::Acquire),
            duplicate_transition_requests: self
                .duplicate_transition_requests
                .load(Ordering::Acquire),
            fallback_entries: self.fallback_entries.load(Ordering::Acquire),
            safety_mute_entries: self.safety_mute_entries.load(Ordering::Acquire),
            safety_mute_exits: self.safety_mute_exits.load(Ordering::Acquire),
            fault_entries: self.fault_entries.load(Ordering::Acquire),
            ack_slot_exhausted: self.ack_slot_exhausted.load(Ordering::Acquire),
            quarantine_exhausted: self.quarantine_exhausted.load(Ordering::Acquire),
            worker_lost: self.worker_lost.load(Ordering::Acquire),
            watchdog_mutes: self.watchdog_mutes.load(Ordering::Acquire),
        }
    }
}

impl Default for FaultCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// 计数快照（Copy POD）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountersSnapshot {
    /// 完成 ack 的 block 数。
    pub blocks_done: u64,
    /// 迟到块数。
    pub late_blocks: u64,
    /// cutoff 块数。
    pub cutoff_blocks: u64,
    /// gate miss 块数。
    pub deadline_gate_miss: u64,
    /// hard cutoff 超时块数。
    pub hard_late: u64,
    /// 输入环丢块。
    pub input_overruns: u64,
    /// 输出欠载。
    pub output_underruns: u64,
    /// 输入 nonfinite 样本。
    pub nonfinite_in: u64,
    /// 输出 nonfinite 样本（应 0）。
    pub nonfinite_out: u64,
    /// clip 样本。
    pub clipped_samples: u64,
    /// quarantine 进入计数。
    pub quarantine_entries: u64,
    /// quarantine 仍保留计数（禁止释放时本值不下降）。
    pub quarantine_retained: u64,
    /// swap 成功计数。
    pub swap_count: u64,
    /// retire 成功计数。
    pub retire_count: u64,
    /// 重复迁移请求。
    pub duplicate_transition_requests: u64,
    /// fallback 进入次数。
    pub fallback_entries: u64,
    /// mute 进入次数。
    pub safety_mute_entries: u64,
    /// mute 退出次数。
    pub safety_mute_exits: u64,
    /// fault 进入次数。
    pub fault_entries: u64,
    /// ack 槽耗尽次数。
    pub ack_slot_exhausted: u64,
    /// quarantine 耗尽次数。
    pub quarantine_exhausted: u64,
    /// worker 失联次数。
    pub worker_lost: u64,
    /// watchdog 触发 mute 次数。
    pub watchdog_mutes: u64,
}

impl CountersSnapshot {
    /// [NONRT] 导出计数（每行 counters.NAME=VALUE）。
    pub fn write_text<W: fmt::Write>(&self, out: &mut W) -> fmt::Result {
        write!(out, "counters.blocks_done={}\n", self.blocks_done)?;
        write!(out, "counters.late_blocks={}\n", self.late_blocks)?;
        write!(out, "counters.cutoff_blocks={}\n", self.cutoff_blocks)?;
        write!(
            out,
            "counters.deadline_gate_miss={}\n",
            self.deadline_gate_miss
        )?;
        write!(out, "counters.hard_late={}\n", self.hard_late)?;
        write!(out, "counters.input_overruns={}\n", self.input_overruns)?;
        write!(out, "counters.output_underruns={}\n", self.output_underruns)?;
        write!(out, "counters.nonfinite_in={}\n", self.nonfinite_in)?;
        write!(out, "counters.nonfinite_out={}\n", self.nonfinite_out)?;
        write!(out, "counters.clipped_samples={}\n", self.clipped_samples)?;
        write!(
            out,
            "counters.quarantine_entries={}\n",
            self.quarantine_entries
        )?;
        write!(
            out,
            "counters.quarantine_retained={}\n",
            self.quarantine_retained
        )?;
        write!(out, "counters.swap_count={}\n", self.swap_count)?;
        write!(out, "counters.retire_count={}\n", self.retire_count)?;
        write!(
            out,
            "counters.duplicate_transition_requests={}\n",
            self.duplicate_transition_requests
        )?;
        write!(out, "counters.fallback_entries={}\n", self.fallback_entries)?;
        write!(
            out,
            "counters.safety_mute_entries={}\n",
            self.safety_mute_entries
        )?;
        write!(
            out,
            "counters.safety_mute_exits={}\n",
            self.safety_mute_exits
        )?;
        write!(out, "counters.fault_entries={}\n", self.fault_entries)?;
        write!(
            out,
            "counters.ack_slot_exhausted={}\n",
            self.ack_slot_exhausted
        )?;
        write!(
            out,
            "counters.quarantine_exhausted={}\n",
            self.quarantine_exhausted
        )?;
        write!(out, "counters.worker_lost={}\n", self.worker_lost)?;
        write!(out, "counters.watchdog_mutes={}\n", self.watchdog_mutes)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// quarantine 池（§7.6/INV-L5/D-08/D-09：all-ack 前禁止复用/释放；失联/失核永不回收）
// ---------------------------------------------------------------------------

// quarantine 槽状态（compare_exchange 认领；写者=master，释放者=control）。
const QSLOT_FREE: u32 = 0;
const QSLOT_WRITING: u32 = 1;
const QSLOT_OCCUPIED: u32 = 2;

/// quarantine 槽句柄（Copy POD；只在池生命周期内有效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuarantineHandle {
    index: u8,
}

impl QuarantineHandle {
    /// 槽下标（0..QUARANTINE_SLOTS）。
    pub const fn index(self) -> usize {
        self.index as usize
    }

    /// 原始编号（事件/导出用）。
    pub const fn raw(self) -> u8 {
        self.index
    }
}

/// quarantine 槽的原子状态（§7.6：可能迟到 worker 集合 + 标签；禁止覆盖/复用）。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct QuarantineSlot {
    state: AtomicU32,
    flags: AtomicU32,
    plan_version: AtomicU32,
    pending_ack_mask: AtomicU32,
    epoch_id: AtomicU64,
    block_seq: AtomicU64,
}

impl QuarantineSlot {
    /// 空槽。
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(QSLOT_FREE),
            flags: AtomicU32::new(0),
            plan_version: AtomicU32::new(0),
            pending_ack_mask: AtomicU32::new(0),
            epoch_id: AtomicU64::new(0),
            block_seq: AtomicU64::new(0),
        }
    }
}

impl Default for QuarantineSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// 单个 quarantine 槽快照（Copy POD；导出用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuarantineSlotSnapshot {
    /// 是否占用（OCCUPIED）。
    pub occupied: bool,
    /// block epoch。
    pub epoch_id: u64,
    /// plan 版本。
    pub plan_version: PlanVersion,
    /// block 序号。
    pub block_seq: u64,
    /// 仍可能迟到/未 ack 的 worker 位图。
    pub pending_ack_mask: u32,
}

/// quarantine 池快照（Copy POD）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuarantineSnapshot {
    /// 8 个槽。
    pub slots: [QuarantineSlotSnapshot; QUARANTINE_SLOTS],
    /// 失联 worker 位图。
    pub lost_workers: u32,
    /// 1 = 禁止释放（worker 失联/失核/ack 槽耗尽等）。
    pub release_forbidden: u32,
    /// 禁止释放原因码（RtStatus）。
    pub forbid_reason: u32,
    /// 1 = 失核 latch。
    pub core_lost: u32,
    /// 累计进入次数。
    pub entries: u64,
    /// 仍保留槽数（禁止释放时不得下降）。
    pub retained: u64,
    /// 累计释放次数。
    pub released: u64,
    /// 池耗尽次数。
    pub exhausted: u64,
}

/// quarantine 池（固定 QUARANTINE_SLOTS=8；RT/control 两侧都用原子方法，无 Drop）。
///
/// 规则（core-v1 §7.6/§13 D-08/D-09）：
/// - 进入后只有“全部可能迟到 worker 都 ack”且未 latch 失联/失核时才允许 control 释放；
/// - 池满 → RT_E_QUARANTINE_FULL，禁止临时分配/覆盖，请求 SafetyMute；
/// - worker 失联/失核 → release 永久返回 RT_E_WORKER_LOST，retained 不减，等进程重启；
/// - 没有强制释放 API（D-09：安全优先，不允许“省内存”回收）。
#[repr(C, align(64))]
pub struct QuarantinePool {
    slots: [QuarantineSlot; QUARANTINE_SLOTS],
    lost_workers: AtomicU32,
    release_forbidden: AtomicU32,
    forbid_reason: AtomicU32,
    core_lost: AtomicU32,
    entries: AtomicU64,
    retained: AtomicU64,
    released: AtomicU64,
    exhausted: AtomicU64,
}

impl QuarantinePool {
    /// 空池。
    pub fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| QuarantineSlot::new()),
            lost_workers: AtomicU32::new(0),
            release_forbidden: AtomicU32::new(0),
            forbid_reason: AtomicU32::new(0),
            core_lost: AtomicU32::new(0),
            entries: AtomicU64::new(0),
            retained: AtomicU64::new(0),
            released: AtomicU64::new(0),
            exhausted: AtomicU64::new(0),
        }
    }

    /// 池总槽数（= QUARANTINE_SLOTS）。
    pub const fn capacity() -> usize {
        QUARANTINE_SLOTS
    }

    /// 累计进入次数。
    pub fn entries(&self) -> u64 {
        self.entries.load(Ordering::Acquire)
    }

    /// 仍保留槽数（禁止释放时不得下降）。
    pub fn retained(&self) -> u64 {
        self.retained.load(Ordering::Acquire)
    }

    /// 累计释放次数。
    pub fn released(&self) -> u64 {
        self.released.load(Ordering::Acquire)
    }

    /// 池耗尽次数。
    pub fn exhausted(&self) -> u64 {
        self.exhausted.load(Ordering::Acquire)
    }

    /// 是否禁止释放（worker 失联/失核/ack 槽耗尽）。
    pub fn release_forbidden(&self) -> bool {
        self.release_forbidden.load(Ordering::Acquire) != 0
    }

    /// 当前占用槽数（扫描）。
    pub fn in_use(&self) -> u32 {
        let mut count = 0u32;
        let mut index = 0usize;
        while index < QUARANTINE_SLOTS {
            if self.slots[index].state.load(Ordering::Acquire) == QSLOT_OCCUPIED {
                count += 1;
            }
            index += 1;
        }
        count
    }

    /// [RT master] 隔离一个 invalid 块（epoch/version/block_seq/可能迟到 worker 集合）。
    pub fn enter(
        &self,
        epoch_id: u64,
        plan_version: PlanVersion,
        block_seq: u64,
        pending_ack_mask: u32,
    ) -> Result<QuarantineHandle, RtStatus> {
        if pending_ack_mask == 0 || (pending_ack_mask & !ACK_MASK_ALL) != 0 {
            return Err(RtStatus::RT_E_ABI_RUNTIME);
        }
        let mut index = 0usize;
        while index < QUARANTINE_SLOTS {
            let slot = &self.slots[index];
            if slot
                .state
                .compare_exchange(
                    QSLOT_FREE,
                    QSLOT_WRITING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                slot.epoch_id.store(epoch_id, Ordering::Relaxed);
                slot.plan_version.store(plan_version, Ordering::Relaxed);
                slot.block_seq.store(block_seq, Ordering::Relaxed);
                slot.pending_ack_mask
                    .store(pending_ack_mask, Ordering::Relaxed);
                slot.flags.store(0, Ordering::Relaxed);
                slot.state.store(QSLOT_OCCUPIED, Ordering::Release);
                self.entries.fetch_add(1, Ordering::Relaxed);
                self.retained.fetch_add(1, Ordering::Relaxed);
                return Ok(QuarantineHandle { index: index as u8 });
            }
            index += 1;
        }
        self.exhausted.fetch_add(1, Ordering::Relaxed);
        Err(RtStatus::RT_E_QUARANTINE_FULL)
    }

    /// [RT worker] 迟到 worker 对本块 ack（§7.6：先校验标签，不匹配不得写坏槽）。
    pub fn note_ack(
        &self,
        handle: QuarantineHandle,
        worker: u8,
        epoch_id: u64,
        plan_version: PlanVersion,
        block_seq: u64,
    ) -> RtStatus {
        let index = handle.index();
        if index >= QUARANTINE_SLOTS {
            return RtStatus::RT_E_SLOT_STATE;
        }
        if worker as usize >= HIST_WORKERS {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        let slot = &self.slots[index];
        if slot.state.load(Ordering::Acquire) != QSLOT_OCCUPIED {
            return RtStatus::RT_E_SLOT_STATE;
        }
        if slot.epoch_id.load(Ordering::Acquire) != epoch_id {
            return RtStatus::RT_E_EPOCH_MISMATCH;
        }
        if slot.plan_version.load(Ordering::Acquire) != plan_version {
            return RtStatus::RT_E_PLAN_MISMATCH;
        }
        if slot.block_seq.load(Ordering::Acquire) != block_seq {
            return RtStatus::RT_E_SLOT_STATE;
        }
        let bit = 1u32 << worker;
        let pending = slot.pending_ack_mask.load(Ordering::Acquire);
        if (pending & bit) == 0 {
            return RtStatus::RT_E_SLOT_STATE;
        }
        slot.pending_ack_mask.fetch_and(!bit, Ordering::AcqRel);
        RtStatus::OK
    }

    /// [NONRT control] 尝试释放（仅当 all-ack 且未 latch 失联/失核）。
    pub fn release(&self, handle: QuarantineHandle) -> RtStatus {
        let index = handle.index();
        if index >= QUARANTINE_SLOTS {
            return RtStatus::RT_E_SLOT_STATE;
        }
        let slot = &self.slots[index];
        if slot.state.load(Ordering::Acquire) != QSLOT_OCCUPIED {
            return RtStatus::RT_E_SLOT_STATE;
        }
        if self.release_forbidden.load(Ordering::Acquire) != 0 {
            return RtStatus::RT_E_WORKER_LOST;
        }
        if slot.pending_ack_mask.load(Ordering::Acquire) != 0 {
            return RtStatus::RT_E_SLOT_STATE;
        }
        slot.state.store(QSLOT_FREE, Ordering::Release);
        slot.pending_ack_mask.store(0, Ordering::Relaxed);
        self.retained.fetch_sub(1, Ordering::Relaxed);
        self.released.fetch_add(1, Ordering::Relaxed);
        RtStatus::OK
    }

    /// [NONRT/RT] worker 失联：此后所有 quarantine 槽禁止释放（D-09，等进程重启）。
    pub fn pin_worker_lost(&self, worker: u8) -> RtStatus {
        if worker as usize >= HIST_WORKERS {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        self.lost_workers.fetch_or(1u32 << worker, Ordering::AcqRel);
        self.forbid_release(RtStatus::RT_E_WORKER_LOST)
    }

    /// [NONRT/RT] 失核：所有 quarantine 槽禁止释放（§7.5 RG-2/RG-3 未满足 + 2 核重准入）。
    pub fn pin_core_lost(&self) -> RtStatus {
        self.core_lost.store(1, Ordering::Release);
        self.forbid_release(RtStatus::RT_E_WORKER_LOST)
    }

    /// 禁止释放（幂等；原因码只记录第一次）。
    pub fn forbid_release(&self, reason: RtStatus) -> RtStatus {
        let previous = self.forbid_reason.load(Ordering::Acquire);
        if previous == 0 {
            self.forbid_reason.store(reason.0, Ordering::Release);
        }
        self.release_forbidden.store(1, Ordering::Release);
        RtStatus::OK
    }

    /// 某槽仍待 ack 的 worker 位图（诊断）。
    pub fn pending_ack_mask(&self, handle: QuarantineHandle) -> u32 {
        match self.slots.get(handle.index()) {
            Some(slot) => slot.pending_ack_mask.load(Ordering::Acquire),
            None => 0,
        }
    }

    /// 某槽是否占用。
    pub fn is_occupied(&self, handle: QuarantineHandle) -> bool {
        match self.slots.get(handle.index()) {
            Some(slot) => slot.state.load(Ordering::Acquire) == QSLOT_OCCUPIED,
            None => false,
        }
    }

    /// [NONRT] Acquire 快照。
    pub fn snapshot(&self) -> QuarantineSnapshot {
        let slots = core::array::from_fn(|index| {
            let slot = &self.slots[index];
            QuarantineSlotSnapshot {
                occupied: slot.state.load(Ordering::Acquire) == QSLOT_OCCUPIED,
                epoch_id: slot.epoch_id.load(Ordering::Acquire),
                plan_version: slot.plan_version.load(Ordering::Acquire),
                block_seq: slot.block_seq.load(Ordering::Acquire),
                pending_ack_mask: slot.pending_ack_mask.load(Ordering::Acquire),
            }
        });
        QuarantineSnapshot {
            slots,
            lost_workers: self.lost_workers.load(Ordering::Acquire),
            release_forbidden: self.release_forbidden.load(Ordering::Acquire),
            forbid_reason: self.forbid_reason.load(Ordering::Acquire),
            core_lost: self.core_lost.load(Ordering::Acquire),
            entries: self.entries.load(Ordering::Acquire),
            retained: self.retained.load(Ordering::Acquire),
            released: self.released.load(Ordering::Acquire),
            exhausted: self.exhausted.load(Ordering::Acquire),
        }
    }
}

impl Default for QuarantinePool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 另核 watchdog（§4.1 独立核 SCHED_OTHER；§3.2 10 ms 采样/4 block stale）
// ---------------------------------------------------------------------------

/// watchdog 配置（观测核拓扑 + 采样阈值；admission/T45 提供 DSP 核集合）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogConfig {
    /// watchdog 线程所在 CPU（必须与任一 DSP worker 核不同，§4.1/§6 RT-first 故障行）。
    pub observation_core: u16,
    /// DSP worker 所在 CPU（前 dsp_core_count 项有效；admission/T45 实测）。
    pub dsp_cores: [u16; HIST_WORKERS],
    /// 有效 DSP 核数（2 或 4；2 核是独立 profile）。
    pub dsp_core_count: u8,
    /// 采样周期（ms；= core-v1 §3.2 WATCHDOG_PERIOD_MS，只读心跳，不做 RT 动作）。
    pub sample_period_ms: u32,
    /// 连续多少采样未见 heartbeat 递增 → 请求 mute。
    pub stale_samples: u32,
    /// 连续多少采样 worker 失联/失核 → 请求 fault。
    pub worker_lost_samples: u32,
    /// 连续多少采样 swap ack 未 all-ack（retire gate 卡住）→ 请求 fault。
    pub retire_ack_samples: u32,
}

impl WatchdogConfig {
    /// 生产配置（WATCHDOG_PERIOD_MS/WATCHDOG_STALE_BLOCKS 默认阈值）。
    pub const fn new(
        observation_core: u16,
        dsp_cores: [u16; HIST_WORKERS],
        dsp_core_count: u8,
    ) -> Self {
        Self {
            observation_core,
            dsp_cores,
            dsp_core_count,
            sample_period_ms: WATCHDOG_PERIOD_MS,
            stale_samples: WATCHDOG_STALE_BLOCKS,
            worker_lost_samples: WATCHDOG_WORKER_LOST_SAMPLES_DEFAULT,
            retire_ack_samples: WATCHDOG_RETIRE_ACK_SAMPLES_DEFAULT,
        }
    }

    /// 未绑定核的模型配置（仅单测/离线模型；生产必须用 new 并过 with_config 核校验）。
    pub const fn unverified() -> Self {
        Self {
            observation_core: u16::MAX,
            dsp_cores: [0; HIST_WORKERS],
            dsp_core_count: 0,
            sample_period_ms: WATCHDOG_PERIOD_MS,
            stale_samples: WATCHDOG_STALE_BLOCKS,
            worker_lost_samples: WATCHDOG_WORKER_LOST_SAMPLES_DEFAULT,
            retire_ack_samples: WATCHDOG_RETIRE_ACK_SAMPLES_DEFAULT,
        }
    }
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self::new(4, [0, 1, 2, 3], RT_WORKERS)
    }
}

/// watchdog 一次采样的观测输入（POD；由 T26 watchdog 线程从原子字段装配，安全）。
///
/// 字段与 §11.13 TspiSwapAckV1/TspiWorkerStatsV1 对齐：expected_workers/ack_mask（all-ack 用
/// ACK_MASK(expected)，禁止硬写 0xF）、worker alive 位、DSP 核丢失位、设备状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogSample {
    /// master heartbeat 序号（§11.13 heartbeat_seq）。
    pub heartbeat_seq: u64,
    /// 当前 epoch（epoch 变化重置 stale 计数）。
    pub epoch_id: u64,
    /// expected workers（2/4；2 核是独立 profile）。
    pub expected_workers: u8,
    /// swap ack 位图（§11.13 TspiSwapAckV1.ack_mask）。
    pub swap_ack_mask: u32,
    /// worker alive 位图（bit k = worker k 活着）。
    pub worker_alive_mask: u32,
    /// 设备丢失（T17 状态机上报）。
    pub device_lost: bool,
    /// DSP 核丢失位图（bit k = worker k 所在核丢失；失核=整块/整图拒收）。
    pub lost_core_mask: u32,
}

impl WatchdogSample {
    /// 正常样本。
    pub const fn ok(
        heartbeat_seq: u64,
        epoch_id: u64,
        expected_workers: u8,
        swap_ack_mask: u32,
        worker_alive_mask: u32,
    ) -> Self {
        Self {
            heartbeat_seq,
            epoch_id,
            expected_workers,
            swap_ack_mask,
            worker_alive_mask,
            device_lost: false,
            lost_core_mask: 0,
        }
    }

    /// 字段校验（expected∈{2,4}、位图不越界；否则 RT_E_ABI_RUNTIME，§9.2/404）。
    pub fn validate(&self) -> RtStatus {
        if self.expected_workers != RT_WORKERS && self.expected_workers != RT_WORKERS_FALLBACK {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        let mask = ack_mask(self.expected_workers);
        if (self.swap_ack_mask & !mask) != 0
            || (self.worker_alive_mask & !mask) != 0
            || (self.lost_core_mask & !mask) != 0
        {
            return RtStatus::RT_E_ABI_RUNTIME;
        }
        RtStatus::OK
    }
}

/// 独立核 watchdog（[WD]：只读原子 heartbeat/epoch/ack/worker 计数，只置位 safety_request）。
///
/// 硬规则（§4.1/§5.1/§12.2）：
/// - 观测核必须与任一 DSP worker 核不同（构造期 ERR_ADMISSION_SCHED，不允许“另核”造假）；
/// - 只置位 ctl.safety_request（1=mute、2=fault，fetch_max 只升级）并返回动作，绝不自行迁移/恢复；
/// - latch 后即使 heartbeat 恢复也不自动清除，只能 reset_after_readmission（control + 完整 admission）。
#[derive(Debug)]
pub struct Watchdog {
    config: WatchdogConfig,
    affinity_verified: bool,
    samples_total: u64,
    stale_count: u32,
    worker_lost_count: u32,
    ack_stall_count: u32,
    last_heartbeat_seq: u64,
    last_epoch_id: u64,
    epoch_seen: bool,
    latched: u32,
    latched_reason: u32,
    last_action: WatchdogAction,
}

impl Watchdog {
    /// T06 scaffold 构造（未绑定核；生产必须用 with_config 过另核校验）。
    pub const fn new() -> Self {
        Self {
            config: WatchdogConfig::unverified(),
            affinity_verified: false,
            samples_total: 0,
            stale_count: 0,
            worker_lost_count: 0,
            ack_stall_count: 0,
            last_heartbeat_seq: 0,
            last_epoch_id: 0,
            epoch_seen: false,
            latched: 0,
            latched_reason: 0,
            last_action: WatchdogAction::None,
        }
    }

    /// 生产构造：校验观测核与 DSP worker 核互斥（另核），阈值参数合法。
    pub fn with_config(config: WatchdogConfig) -> Result<Self, RtStatus> {
        if config.dsp_core_count != RT_WORKERS && config.dsp_core_count != RT_WORKERS_FALLBACK {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if config.sample_period_ms == 0
            || config.stale_samples == 0
            || config.worker_lost_samples == 0
            || config.retire_ack_samples == 0
        {
            return Err(RtStatus::ERR_BAD_LAYOUT);
        }
        let mut index = 0usize;
        while index < config.dsp_core_count as usize {
            if config.observation_core == config.dsp_cores[index] {
                return Err(RtStatus::ERR_ADMISSION_SCHED);
            }
            index += 1;
        }
        Ok(Self {
            config,
            affinity_verified: true,
            ..Self::new()
        })
    }

    /// 配置。
    pub const fn config(&self) -> WatchdogConfig {
        self.config
    }

    /// 是否已通过“另核”校验（T26 生产必须为 true）。
    pub const fn affinity_verified(&self) -> bool {
        self.affinity_verified
    }

    /// 累计采样次数。
    pub const fn samples_total(&self) -> u64 {
        self.samples_total
    }

    /// 是否已 latch（只置位，不自动恢复）。
    pub const fn latched(&self) -> bool {
        self.latched != 0
    }

    /// latch 后最近一次动作（未 latch → None）。
    pub const fn last_action(&self) -> WatchdogAction {
        self.last_action
    }

    /// latch 原因码。
    pub const fn latched_reason(&self) -> RtStatus {
        RtStatus::from_code(self.latched_reason)
    }

    /// 当前连续 stale 采样数。
    pub const fn stale_samples(&self) -> u32 {
        self.stale_count
    }

    /// 内部：心跳/epoch 推进 → 是否达到 stale 阈值（epoch 变化重置）。
    fn track_heartbeat(&mut self, heartbeat_seq: u64, epoch_id: u64) -> bool {
        self.samples_total = self.samples_total.saturating_add(1);
        if !self.epoch_seen {
            // 首次采样只建立基线（不把“从没心跳”误计成 stale）。
            self.epoch_seen = true;
            self.last_epoch_id = epoch_id;
            self.last_heartbeat_seq = heartbeat_seq;
            self.stale_count = 0;
            return false;
        } else if epoch_id != self.last_epoch_id {
            // 新 epoch：新时间轴，旧心跳计数不再可比。
            self.last_epoch_id = epoch_id;
            self.stale_count = 0;
            self.last_heartbeat_seq = heartbeat_seq;
            return false;
        }
        if heartbeat_seq == self.last_heartbeat_seq {
            self.stale_count = self.stale_count.saturating_add(1);
        } else {
            self.stale_count = 0;
        }
        self.last_heartbeat_seq = heartbeat_seq;
        self.stale_count >= self.config.stale_samples
    }

    /// 内部：置位 mute（只升级，不覆盖 fault）。
    fn latch_mute(&mut self, ctl: &TspiRtControlV1, reason: RtStatus) -> WatchdogAction {
        if self.latched == 0 {
            self.latched = 1;
            self.latched_reason = reason.0;
            // 只置位：1=mute；不写 safety_reason（plain 字段只由 control 写）。
            let _ = ctl.safety_request.fetch_max(1, Ordering::Release);
        }
        self.last_action = WatchdogAction::RequestMute(reason);
        self.last_action
    }

    /// 内部：置位 fault（不可恢复；fetch_max 只升级）。
    fn latch_fault(&mut self, ctl: &TspiRtControlV1, reason: RtStatus) -> WatchdogAction {
        self.latched = 2;
        self.latched_reason = reason.0;
        let _ = ctl.safety_request.fetch_max(2, Ordering::Release);
        self.last_action = WatchdogAction::Fault(reason);
        self.last_action
    }

    /// [WD] 每 10 ms 采样一次 heartbeat/epoch（T06 scaffold 冻结签名保持）。
    ///
    /// 只置位安全请求，不做迁移/恢复；latch 后恒返回同一动作（不自动清除）。
    pub fn observe(&mut self, ctl: &TspiRtControlV1) -> WatchdogAction {
        if self.latched != 0 {
            return self.last_action;
        }
        let heartbeat = ctl.heartbeat_seq.load(Ordering::Acquire);
        let epoch = ctl.block_start_epoch.load(Ordering::Acquire);
        if self.track_heartbeat(heartbeat, epoch) {
            return self.latch_mute(ctl, RtStatus::RT_E_WATCHDOG);
        }
        WatchdogAction::None
    }

    /// [WD] 完整采样（heartbeat/epoch/worker alive/swap ack/设备/失核）。
    ///
    /// 判定顺序（安全优先）：字段非法 → ABI fault；已 latch → 原动作；stale → mute；
    /// worker 失联/失核持续超阈值 → fault(401)；swap ack 未 all-ack 超阈值（retire gate 卡住）
    /// → fault(401)（§7.5 硬规则：禁止释放、SafetyMute、等进程重启）；设备丢失 → mute(407)。
    pub fn observe_sample(
        &mut self,
        sample: &WatchdogSample,
        ctl: &TspiRtControlV1,
    ) -> WatchdogAction {
        let validation = sample.validate();
        if !validation.is_ok() {
            return self.latch_fault(ctl, validation);
        }
        // §5.1：Faulted 后不再接受更弱动作；SafetyMute 期间若再发生 fatal fault 允许升级 Faulted。
        if self.latched == 2 {
            return self.last_action;
        }
        let stale = self.track_heartbeat(sample.heartbeat_seq, sample.epoch_id);
        let expected = sample.expected_workers;
        let mask = ack_mask(expected);
        let lost = ((!sample.worker_alive_mask) & mask) | (sample.lost_core_mask & mask);
        if lost != 0 {
            self.worker_lost_count = self.worker_lost_count.saturating_add(1);
            if self.worker_lost_count >= self.config.worker_lost_samples {
                // §7.5/§9.2 401：worker 失联 → SafetyMute/Faulted；不释放 quarantine。
                return self.latch_fault(ctl, RtStatus::RT_E_WORKER_LOST);
            }
        } else {
            self.worker_lost_count = 0;
        }
        if (sample.swap_ack_mask & mask) != mask {
            self.ack_stall_count = self.ack_stall_count.saturating_add(1);
            if self.ack_stall_count >= self.config.retire_ack_samples {
                // RG-2/RG-3 未满足（含 worker 永不 ack）：禁止释放 + SafetyMute/Faulted。
                return self.latch_fault(ctl, RtStatus::RT_E_WORKER_LOST);
            }
        } else {
            self.ack_stall_count = 0;
        }
        if self.latched == 1 {
            // 已处于 mute latch：保持（不重复置位），除非上面已升级 fault。
            return self.last_action;
        }
        if stale {
            return self.latch_mute(ctl, RtStatus::RT_E_WATCHDOG);
        }
        if sample.device_lost {
            // 设备丢失：T17 恢复状态机期间静音；恢复需新 epoch（§9.2/407）。
            return self.latch_mute(ctl, RtStatus::RT_E_DEVICE_LOST);
        }
        WatchdogAction::None
    }

    /// [WD] 设备丢失单点（T17 状态机直接上报）。
    pub fn observe_device_lost(&mut self, ctl: &TspiRtControlV1) -> WatchdogAction {
        if self.latched != 0 {
            return self.last_action;
        }
        self.latch_mute(ctl, RtStatus::RT_E_DEVICE_LOST)
    }

    /// [NONRT control] 只在完整重新 admission 后清除 latch（禁止自动恢复）。
    pub fn reset_after_readmission(&mut self, ticket: &ReAdmissionTicket) -> RtStatus {
        // ticket 是“刚完成一次完整 admission”的凭证；此处只记录其版本供诊断。
        if ticket.plan_version() == 0 {
            return RtStatus::ERR_PLAN_STALE;
        }
        self.latched = 0;
        self.latched_reason = 0;
        self.last_action = WatchdogAction::None;
        self.stale_count = 0;
        self.worker_lost_count = 0;
        self.ack_stall_count = 0;
        self.epoch_seen = false;
        RtStatus::OK
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// fallback 图预分配 + 准入（§5.5：G=I 保留 P_post；预分配 + worst deadline；不得超时重算昂贵 P）
// ---------------------------------------------------------------------------

/// 保护 fallback 图规格（由 T56/T66/T26 从已校验 plan 派生；admission 只做准入，不编译图）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackGraphSpec {
    /// fallback plan 版本（必须非 0，来自 swap/T20 的 PlanVersion）。
    pub plan_version: PlanVersion,
    /// 图 hash（§11.10 graph_id；全 0 视为未校验 → ERR_HASH）。
    pub graph_id: [u8; GRAPH_ID_BYTES],
    /// 输出通道数（安全静音 buffer 尺寸核算）。
    pub output_channels: u16,
    /// G 必须是 identity（禁止把昂贵 P/G 重算当 fallback，§5.5）。
    pub identity_g: bool,
    /// 必须保留 P_post 最终保护（bypass 不等于绕保护，§5.5）。
    pub p_post_present: bool,
    /// identity 图的非零矩阵路径数（必须 0；对角 baseline/P_post 不计入）。
    pub matrix_routes: u16,
    /// 已验证的 fallback worst block 时间（ticks→ns 或实测 ns；0 = 未实测 → 拒绝）。
    pub worst_block_ns: u64,
}

/// fallback 准入结果（Copy POD；只由 admit_fallback 成功路径产生）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackAdmission {
    /// fallback plan 版本。
    pub plan_version: PlanVersion,
    /// 图 hash。
    pub graph_id: [u8; GRAPH_ID_BYTES],
    /// 第二 PlanArena（GraphPending）已预分配字节。
    pub graph_bytes: u64,
    /// 预分配安全静音 buffer 字节。
    pub safety_silence_bytes: u64,
    /// 实测 worst block 时间。
    pub worst_block_ns: u64,
    /// 允许的 hard cutoff 预算（T_block − io_tail）。
    pub budget_ns: u64,
    /// G=I。
    pub identity_g: bool,
    /// 保留 P_post。
    pub p_post_present: bool,
    /// 是否准入成功（失败不产生本结构；仍保留字段供导出/断言）。
    pub accepted: bool,
}

impl FallbackAdmission {
    /// 是否可作为安全输出（进入 ProtectionFallback 的前置）。
    pub const fn is_ready(&self) -> bool {
        self.accepted
            && self.identity_g
            && self.p_post_present
            && self.worst_block_ns > 0
            && self.worst_block_ns <= self.budget_ns
            && self.graph_bytes > 0
            && self.safety_silence_bytes > 0
    }

    /// worst deadline 余量（0 = 边界通过）。
    pub const fn headroom_ns(&self) -> u64 {
        self.budget_ns.saturating_sub(self.worst_block_ns)
    }
}

/// fallback 的 hard cutoff 预算：T_block − IO_TAIL_ALLOWANCE_NS_DEFAULT（§3.2/§3.5）。
///
/// 同时校验 fs∈{48k,96k}、B∈{64,128}、io_tail < 25%×T_block（§3.2 硬上限）。
pub fn fallback_deadline_budget_ns(env: &AdmissionEnv) -> Result<u64, RtStatus> {
    let rate_ok =
        env.sample_rate_hz == SAMPLE_RATES_HZ[0] || env.sample_rate_hz == SAMPLE_RATES_HZ[1];
    if !rate_ok {
        return Err(RtStatus::ERR_RATE);
    }
    let block_ok = env.block_size == BLOCK_SIZES[0] || env.block_size == BLOCK_SIZES[1];
    if !block_ok {
        return Err(RtStatus::ERR_BLOCK_SIZE);
    }
    let t_block = t_block_ns_ceil(env.sample_rate_hz, env.block_size);
    let tail_quarter = IO_TAIL_ALLOWANCE_NS_DEFAULT
        .checked_mul(4)
        .ok_or(RtStatus::ERR_CAPACITY)?;
    if tail_quarter >= t_block {
        return Err(RtStatus::ERR_CAPACITY);
    }
    t_block
        .checked_sub(IO_TAIL_ALLOWANCE_NS_DEFAULT)
        .ok_or(RtStatus::ERR_CAPACITY)
}

/// [NONRT] fallback 图预分配 + deadline/容量准入（§5.5；失败即拒绝，不产生 fallback）。
///
/// 准入项（任一失败 → 冻结 RtStatus，不降级）：
/// 1. plan_version != 0 / graph_id != 0（未校验图拒绝：ERR_BAD_LAYOUT/ERR_HASH）；
/// 2. identity_g == true（禁止超时后临时重算昂贵 P，§5.5）且 p_post_present == true（不绕保护）；
/// 3. matrix_routes == 0（identity 图不得有非零矩阵路径：ERR_PATH_TABLE）；
/// 4. output_channels 合法（ERR_CHANNELS）；
/// 5. 第二 PlanArena（GraphPending）已预分配且 >= 当前图容量（§4.4 始终双图）；
/// 6. 预分配安全静音 buffer >= B×O×f32（§5.5）；
/// 7. worst_block_ns > 0 且 <= T_block − io_tail（无法保证安全输出则拒绝，绝不在超时后重算）。
pub fn admit_fallback(
    spec: &FallbackGraphSpec,
    env: &AdmissionEnv,
    memory: &MemoryPlan,
) -> Result<FallbackAdmission, RtStatus> {
    if spec.plan_version == 0 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    let mut graph_id_nonzero = false;
    let mut index = 0usize;
    while index < GRAPH_ID_BYTES {
        if spec.graph_id[index] != 0 {
            graph_id_nonzero = true;
        }
        index += 1;
    }
    if !graph_id_nonzero {
        return Err(RtStatus::ERR_HASH);
    }
    if !spec.identity_g {
        return Err(RtStatus::ERR_UNSUPPORTED);
    }
    if !spec.p_post_present {
        return Err(RtStatus::ERR_UNSUPPORTED);
    }
    if spec.matrix_routes != 0 {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    if spec.output_channels == 0 || spec.output_channels as usize > MAX_OUTPUT_CHANNELS {
        return Err(RtStatus::ERR_CHANNELS);
    }
    if memory.graph_pending_bytes == 0
        || memory.graph_pending_bytes < memory.graph_current_bytes
        || memory.graph_current_bytes < PLAN_HEADER_BYTES
    {
        return Err(RtStatus::ERR_CAPACITY);
    }
    let required_silence = (env.block_size as u64)
        .checked_mul(spec.output_channels as u64)
        .and_then(|bytes| bytes.checked_mul(SAFETY_SAMPLE_BYTES))
        .ok_or(RtStatus::ERR_CAPACITY)?;
    if memory.safety_silence_bytes < required_silence {
        return Err(RtStatus::ERR_CAPACITY);
    }
    let budget_ns = fallback_deadline_budget_ns(env)?;
    if spec.worst_block_ns == 0 || spec.worst_block_ns > budget_ns {
        return Err(RtStatus::ERR_CAPACITY);
    }
    Ok(FallbackAdmission {
        plan_version: spec.plan_version,
        graph_id: spec.graph_id,
        graph_bytes: memory.graph_pending_bytes,
        safety_silence_bytes: memory.safety_silence_bytes,
        worst_block_ns: spec.worst_block_ns,
        budget_ns,
        identity_g: spec.identity_g,
        p_post_present: spec.p_post_present,
        accepted: true,
    })
}

// ---------------------------------------------------------------------------
// 完整重新 admission 凭证 + SafetyState（§5.1/§5.5：只升级、恢复只经完整 admission）
// ---------------------------------------------------------------------------

/// 完整重新 admission 凭证（只能由一次成功的 AdmissionReport 生成；§5.1 硬规则）。
///
/// 语义：SafetyMute/Faulted 不得自动恢复 Running；只有 control 在非 RT 完成完整 admission
/// （拓扑/mlock/容量/计划校验全过）后，用本 ticket 调 SafetyState::recover 才能回 Normal。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReAdmissionTicket {
    plan_version: PlanVersion,
    workers: u8,
    ack_mask: u32,
    mlocked_bytes: u64,
}

impl ReAdmissionTicket {
    /// 由 admission 成功报告生成（校验 expected/ack_mask/容量；伪造需构造完整报告）。
    pub fn new(report: &AdmissionReport, env: &AdmissionEnv) -> Result<Self, RtStatus> {
        if env.expected_workers != 0 && env.expected_workers != env.rt_workers {
            return Err(RtStatus::ERR_PLAN_STALE);
        }
        let expected = if env.expected_workers == 0 {
            env.rt_workers
        } else {
            env.expected_workers
        };
        if expected != RT_WORKERS && expected != RT_WORKERS_FALLBACK {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if report.workers != expected || report.expected_workers != expected {
            return Err(RtStatus::ERR_PLAN_STALE);
        }
        if report.ack_mask != ack_mask(expected) {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if report.arena_bytes == 0 || report.mlocked_bytes == 0 {
            return Err(RtStatus::ERR_ADMISSION_MEMORY);
        }
        Ok(Self {
            plan_version: report.plan_version,
            workers: expected,
            ack_mask: report.ack_mask,
            mlocked_bytes: report.mlocked_bytes,
        })
    }

    /// 新 plan 版本。
    pub const fn plan_version(&self) -> PlanVersion {
        self.plan_version
    }

    /// expected workers。
    pub const fn workers(&self) -> u8 {
        self.workers
    }

    /// 参数化 ack 位图（ACK_MASK(expected)）。
    pub const fn ack_mask(&self) -> u32 {
        self.ack_mask
    }

    /// 已锁页字节（admission 证据）。
    pub const fn mlocked_bytes(&self) -> u64 {
        self.mlocked_bytes
    }
}

/// 安全状态机（control 串行迁移；§5.1 迁移表 + 幂等 transition_key + 只升级）。
///
/// 硬规则：
/// - 只有 master/control/watchdog 可请求全局安全动作；worker 请求 → RT_E_INVARIANT(403)；
/// - 只允许升级 Normal→ProtectionFallback→SilenceMute→Faulted（de-escalation 被忽略，
///   恢复只能走 ReAdmissionTicket）；
/// - 相同 transition_key 的重复请求只计 duplicate，不重复写事件（§5.1 幂等）；
/// - 进入 SafetyMute 后 block_start_allowed() == false（不处理新块，§5.5）。
#[derive(Debug)]
pub struct SafetyState {
    mode: SafetyMode,
    key: Option<TransitionKey>,
    reason: Option<u32>,
    revision: u64,
    fallback_entries: u64,
    mute_entries: u64,
    fault_entries: u64,
    duplicated_requests: u64,
    ignored_requests: u64,
    mute_exits: u64,
}

impl SafetyState {
    /// 初始 Normal。
    pub const fn new() -> Self {
        Self {
            mode: SafetyMode::Normal,
            key: None,
            reason: None,
            revision: 0,
            fallback_entries: 0,
            mute_entries: 0,
            fault_entries: 0,
            duplicated_requests: 0,
            ignored_requests: 0,
            mute_exits: 0,
        }
    }

    /// 当前模式。
    pub const fn mode(&self) -> SafetyMode {
        self.mode
    }

    /// 输出动作（SafetyAction）。
    pub const fn action(&self) -> SafetyAction {
        self.mode.action()
    }

    /// 是否允许启动新 block（§5.5：SafetyMute/Faulted 不处理新块）。
    pub const fn block_start_allowed(&self) -> bool {
        matches!(
            self.mode,
            SafetyMode::Normal | SafetyMode::ProtectionFallback
        )
    }

    /// 最近一次迁移原因码。
    pub const fn reason(&self) -> Option<RtStatus> {
        match self.reason {
            Some(code) => Some(RtStatus::from_code(code)),
            None => None,
        }
    }

    /// 最近一次迁移 key（diagnostics/幂等）。
    pub const fn transition_key(&self) -> Option<TransitionKey> {
        self.key
    }

    /// 迁移代数（每次状态变化 +1；同目标重复请求也 +1）。
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// 被去重的重复请求数。
    pub const fn duplicated_requests(&self) -> u64 {
        self.duplicated_requests
    }

    /// 因降级/不允许被忽略的请求数。
    pub const fn ignored_requests(&self) -> u64 {
        self.ignored_requests
    }

    /// fallback 进入次数。
    pub const fn fallback_entries(&self) -> u64 {
        self.fallback_entries
    }

    /// mute 进入次数。
    pub const fn mute_entries(&self) -> u64 {
        self.mute_entries
    }

    /// fault 进入次数。
    pub const fn fault_entries(&self) -> u64 {
        self.fault_entries
    }

    /// mute 退出次数（仅经 ReAdmissionTicket）。
    pub const fn mute_exits(&self) -> u64 {
        self.mute_exits
    }

    /// 内部：有效目标（fallback 不可用或静音 buffer 缺失时按 §9.2/307、§5.5 升级/降级）。
    fn effective_target(request: SafetyRequest, context: SafetyContext) -> (SafetyMode, RtStatus) {
        match request {
            SafetyRequest::ProtectionFallback { reason, .. } => {
                if context.fallback_ready {
                    (SafetyMode::ProtectionFallback, reason)
                } else if context.silence_ready {
                    (SafetyMode::SilenceMute, RtStatus::RT_E_FALLBACK_UNAVAILABLE)
                } else {
                    (SafetyMode::Faulted, RtStatus::RT_E_FALLBACK_UNAVAILABLE)
                }
            }
            SafetyRequest::SilenceMute { reason, .. } => {
                if context.silence_ready {
                    (SafetyMode::SilenceMute, reason)
                } else {
                    // 没有预分配安全静音就无法进入安全输出 → Faulted（§5.5 硬规则）。
                    (SafetyMode::Faulted, reason)
                }
            }
            SafetyRequest::Fault { reason, .. } => (SafetyMode::Faulted, reason),
        }
    }

    /// 内部：只允许升级（同目标也算允许）。
    fn transition_allowed(from: SafetyMode, to: SafetyMode) -> bool {
        if from == to {
            return true;
        }
        to > from
    }

    /// [NONRT/control] 执行一次安全迁移请求（control 串行；返回 OK=应用/忽略/去重）。
    pub fn request(
        &mut self,
        request: SafetyRequest,
        epoch_id: u64,
        plan_version: PlanVersion,
        context: SafetyContext,
        events: &mut EventLanes,
        counters: &FaultCounters,
    ) -> RtStatus {
        if !request.requester().can_request_global_safety() {
            // §9.3：worker 不得直接改全局安全状态。
            return RtStatus::RT_E_INVARIANT;
        }
        let (target, reason) = Self::effective_target(request, context);
        let key = TransitionKey {
            from: self.mode,
            to: target,
            reason: reason.0,
            epoch_id,
            plan_version,
        };
        if let Some(last) = self.key {
            // §5.1 幂等：同一迁移请求（to/reason/epoch/plan_version 相同）重复到达只计 duplicate，
            // 不重复写事件（from 随当前状态变化，不作为判据）。
            if last.to == target
                && last.reason == reason.0
                && last.epoch_id == epoch_id
                && last.plan_version == plan_version
            {
                self.duplicated_requests = self.duplicated_requests.saturating_add(1);
                counters.record_duplicate_transition();
                return RtStatus::OK;
            }
        }
        if target == self.mode {
            // 同目标、不同 key（新的原因/epoch）：更新 key/原因，不重复写进入事件。
            self.key = Some(key);
            self.reason = Some(reason.0);
            self.revision = self.revision.saturating_add(1);
            return RtStatus::OK;
        }
        if !Self::transition_allowed(self.mode, target) {
            // 降级请求（如 Faulted→SilenceMute）：忽略，不写事件。
            self.ignored_requests = self.ignored_requests.saturating_add(1);
            return RtStatus::OK;
        }
        self.mode = target;
        self.key = Some(key);
        self.reason = Some(reason.0);
        self.revision = self.revision.saturating_add(1);
        match target {
            SafetyMode::ProtectionFallback => {
                self.fallback_entries = self.fallback_entries.saturating_add(1);
                counters.record_fallback_enter();
                let _ = emit(
                    events,
                    LANE_CONTROL,
                    EV_FALLBACK_ENTER,
                    SEVERITY_ERROR,
                    epoch_id,
                    0,
                    reason.0,
                    plan_version,
                );
            }
            SafetyMode::SilenceMute => {
                self.mute_entries = self.mute_entries.saturating_add(1);
                counters.record_safety_mute_enter();
                let _ = emit(
                    events,
                    LANE_CONTROL,
                    EV_SAFETY_MUTE_ENTER,
                    SEVERITY_FATAL,
                    epoch_id,
                    0,
                    reason.0,
                    plan_version,
                );
            }
            SafetyMode::Faulted => {
                self.fault_entries = self.fault_entries.saturating_add(1);
                counters.record_fault_enter();
                let _ = emit(
                    events,
                    LANE_CONTROL,
                    EV_FAULT,
                    SEVERITY_FATAL,
                    epoch_id,
                    0,
                    reason.0,
                    plan_version,
                );
            }
            SafetyMode::Normal => {}
        }
        RtStatus::OK
    }

    /// [NONRT/control] 完整重新 admission 后恢复 Normal（唯一退出路径；不存在自动恢复）。
    pub fn recover(
        &mut self,
        ticket: &ReAdmissionTicket,
        epoch_id: u64,
        events: &mut EventLanes,
        counters: &FaultCounters,
    ) -> RtStatus {
        if !self.mode.is_degraded() {
            return RtStatus::RT_E_SLOT_STATE;
        }
        let previous_reason = self.reason.unwrap_or(0);
        self.mode = SafetyMode::Normal;
        self.key = None;
        self.reason = None;
        self.revision = self.revision.saturating_add(1);
        self.mute_exits = self.mute_exits.saturating_add(1);
        counters.record_safety_mute_exit();
        let _ = emit(
            events,
            LANE_CONTROL,
            EV_SAFETY_MUTE_EXIT,
            SEVERITY_INFO,
            epoch_id,
            0,
            previous_reason,
            ticket.plan_version(),
        );
        RtStatus::OK
    }

    /// [NONRT/control] 消费 ctl.safety_request（watchdog/master 置位的请求，§11.13/§5.1）。
    ///
    /// request=1 → SafetyMute；request=2 → Faulted。reason 取 ctl.safety_reason（plain 字段只由
    /// control 写；0 时按 406/403 兜底）。本方法必须在 control 线程调用。
    pub fn service_control_request(
        &mut self,
        ctl: &TspiRtControlV1,
        epoch_id: u64,
        plan_version: PlanVersion,
        context: SafetyContext,
        events: &mut EventLanes,
        counters: &FaultCounters,
    ) -> RtStatus {
        let request = ctl.safety_request.load(Ordering::Acquire);
        if request == 0 {
            return RtStatus::OK;
        }
        let code = ctl.safety_reason;
        let reason = if code != 0 {
            RtStatus::from_code(code)
        } else if request >= 2 {
            RtStatus::RT_E_INVARIANT
        } else {
            RtStatus::RT_E_WATCHDOG
        };
        let safety_request = if request >= 2 {
            SafetyRequest::Fault {
                reason,
                requester: GuardRequester::Control,
            }
        } else {
            SafetyRequest::SilenceMute {
                reason,
                requester: GuardRequester::Control,
            }
        };
        self.request(
            safety_request,
            epoch_id,
            plan_version,
            context,
            events,
            counters,
        )
    }
}

impl Default for SafetyState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// FaultSupervisor：组合 RT 记录路径 + control 侧安全迁移/quarantine 释放 + 非 RT 导出
// ---------------------------------------------------------------------------

/// 非 RT 导出元数据（§8.4 导出规则：必须带 graph_id/plan_version/epoch_id 与采样窗口）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportMeta {
    /// plan 图 hash（SHA-256）。
    pub graph_id: [u8; GRAPH_ID_BYTES],
    /// 当前 plan 版本。
    pub plan_version: PlanVersion,
    /// 当前 epoch。
    pub epoch_id: u64,
    /// 采样窗口起点（block_start_seq，含）。
    pub window_start_seq: u64,
    /// 采样窗口终点（block_start_seq，含）。
    pub window_end_seq: u64,
    /// tick→ns 换算频率（admission 标定；0=未标定）。
    pub tick_freq_hz: u32,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// block 帧数。
    pub block_size: u16,
    /// watchdog 观测核。
    pub watchdog_core: u16,
    /// watchdog 连续 stale 采样阈值。
    pub watchdog_stale_samples: u32,
    /// watchdog 是否已 latch。
    pub watchdog_latched: bool,
}

/// 故障管理组合体（admission 时构造一次；RT 记录路径 &self，control 迁移 &mut self）。
///
/// 用法（T26）：
/// - master/I/O/worker 只调 &self 的记录方法（counters/hist/事件由各自 lane 写）；
/// - control 线程串行调 &mut self 的迁移/quarantine 释放/恢复方法；
/// - watchdog 线程持有自己的 Watchdog，动作交给 control 的 service_watchdog；
/// - metrics 线程用 write_export + drain_events 导出（非 RT）。
pub struct FaultSupervisor {
    /// §8.4 固定计数。
    pub counters: FaultCounters,
    /// 有界直方图（固定 3072 B）。
    pub histogram: HistogramTable,
    /// 8 lane 事件环。
    pub events: EventLanes,
    /// quarantine 池（8 槽）。
    pub quarantine: QuarantinePool,
    /// 安全状态机（control 串行）。
    pub safety: SafetyState,
    config: HistogramConfig,
    fallback: Option<FallbackAdmission>,
    silence_ready: bool,
}

impl FaultSupervisor {
    /// 无 fallback/无安全静音的最保守构造（mute 请求会升级为 Faulted）。
    pub fn new(config: HistogramConfig) -> Self {
        Self {
            counters: FaultCounters::new(),
            histogram: HistogramTable::new(),
            events: EventLanes::new(),
            quarantine: QuarantinePool::new(),
            safety: SafetyState::new(),
            config,
            fallback: None,
            silence_ready: false,
        }
    }

    /// 带已准入 fallback 的构造（fallback 带来预分配安全静音 → silence_ready）。
    pub fn with_fallback(config: HistogramConfig, fallback: FallbackAdmission) -> Self {
        Self {
            counters: FaultCounters::new(),
            histogram: HistogramTable::new(),
            events: EventLanes::new(),
            quarantine: QuarantinePool::new(),
            safety: SafetyState::new(),
            config,
            silence_ready: fallback.is_ready() && fallback.safety_silence_bytes > 0,
            fallback: Some(fallback),
        }
    }

    /// 直方图配置。
    pub const fn histogram_config(&self) -> &HistogramConfig {
        &self.config
    }

    /// 已准入 fallback（None = 未准入，不允许进入 ProtectionFallback）。
    pub const fn fallback(&self) -> Option<&FallbackAdmission> {
        self.fallback.as_ref()
    }

    /// 设置安全静音 buffer 是否可用（T26 在 admission 后按 MemoryPlan.safety_silence_bytes 校验）。
    pub fn set_silence_ready(&mut self, ready: bool) {
        self.silence_ready = ready;
    }

    /// 安全输出可用性上下文。
    pub fn safety_context(&self) -> SafetyContext {
        SafetyContext {
            fallback_ready: self.fallback.map_or(false, |fallback| fallback.is_ready()),
            silence_ready: self.silence_ready,
        }
    }

    /// 输出侧安全动作（§5.5）。
    pub const fn safe_action(&self) -> SafetyAction {
        self.safety.action()
    }

    /// 是否允许启动新 block（SafetyMute/Faulted → false；§5.5/§7.6）。
    pub const fn block_start_allowed(&self) -> bool {
        self.safety.block_start_allowed()
    }

    /// quarantine 计数同步到 §8.4 counters。
    fn sync_quarantine(&self) {
        self.counters
            .set_quarantine_counts(self.quarantine.entries(), self.quarantine.retained());
        self.counters
            .set_quarantine_exhausted(self.quarantine.exhausted());
    }

    // ---- [RT] 记录路径（&self；固定数组 + 原子） --------------------------

    /// [RT] 记一个直方图样本。
    pub fn record_hist(&self, metric: HistMetric, worker: u8, value: u64) -> RtStatus {
        self.histogram.record(&self.config, metric, worker, value)
    }

    /// [RT] 有界等待/自旋 tick。
    pub fn record_spin_ticks(&self, worker: u8, ticks: u64) -> RtStatus {
        self.record_hist(HistMetric::SpinTicks, worker, ticks)
    }

    /// [IO] 读预算 tick。
    pub fn record_io_read_ticks(&self, worker: u8, ticks: u64) -> RtStatus {
        self.record_hist(HistMetric::IoReadTicks, worker, ticks)
    }

    /// [IO] 写预算 tick。
    pub fn record_io_write_ticks(&self, worker: u8, ticks: u64) -> RtStatus {
        self.record_hist(HistMetric::IoWriteTicks, worker, ticks)
    }

    /// [IO] 队列水位（slots）。
    pub fn record_queue_depth(&self, worker: u8, slots: u64) -> RtStatus {
        self.record_hist(HistMetric::QueueDepth, worker, slots)
    }

    /// [RT] phase 间隔 tick。
    pub fn record_phase_gap_ticks(&self, worker: u8, ticks: u64) -> RtStatus {
        self.record_hist(HistMetric::PhaseGapTicks, worker, ticks)
    }

    /// [RT] 完成 ack 的 block（计数 + block tick 直方图）。
    pub fn on_block_done(&self, worker: u8, block_ticks: u64) {
        self.counters.record_block_done();
        let _ = self.record_hist(HistMetric::BlockTicks, worker, block_ticks);
    }

    /// [RT] gate miss（验收门，不停流）。
    pub fn on_deadline_miss(&self) {
        self.counters.record_deadline_gate_miss();
    }

    /// [RT] worker 迟到导致本块 invalid。
    pub fn on_late_block(&self) {
        self.counters.record_late_block();
    }

    /// [RT] cutoff 块。
    pub fn on_cutoff_block(&self) {
        self.counters.record_cutoff_block();
    }

    /// [RT] hard cutoff 超时块。
    pub fn on_hard_late(&self) {
        self.counters.record_hard_late();
    }

    /// [IO] 输入环满丢块。
    pub fn on_input_overrun(&self) {
        self.counters.record_input_overrun();
    }

    /// [IO] 输出环空补静音。
    pub fn on_output_underrun(&self) {
        self.counters.record_output_underrun();
    }

    /// [IO/RT] 输入 nonfinite（已 sanitize）。
    pub fn on_nonfinite_in(&self, samples: u64) {
        self.counters.record_nonfinite_in(samples);
    }

    /// [RT] 输出 nonfinite（>0 即 fault 级）。
    pub fn on_nonfinite_out(&self, samples: u64) {
        self.counters.record_nonfinite_out(samples);
    }

    /// [IO] clip 样本。
    pub fn on_clipped(&self, samples: u64) {
        self.counters.record_clipped(samples);
    }

    /// [NONRT] swap 成功（T20 调用；对齐 swap_count 计数）。
    pub fn note_swap(&self) {
        self.counters.record_swap();
    }

    /// [NONRT] retire 成功（T20 调用；对齐 retire_count 计数）。
    pub fn note_retire(&self) {
        self.counters.record_retire();
    }

    /// [RT master] invalid 块进入 quarantine（§7.6）。
    ///
    /// 失败（池满）→ 计数 + 禁止释放 + 请求 SafetyMute（RT_E_QUARANTINE_FULL）。
    pub fn quarantine_block(
        &mut self,
        epoch_id: u64,
        plan_version: PlanVersion,
        block_seq: u64,
        pending_ack_mask: u32,
    ) -> Result<QuarantineHandle, RtStatus> {
        match self
            .quarantine
            .enter(epoch_id, plan_version, block_seq, pending_ack_mask)
        {
            Ok(handle) => {
                self.counters.record_quarantine_enter();
                self.sync_quarantine();
                let _ = emit(
                    &mut self.events,
                    LANE_WORKER0,
                    EV_QUARANTINE_ENTER,
                    SEVERITY_ERROR,
                    epoch_id,
                    block_seq,
                    (block_seq & 0xFFFF_FFFF) as u32,
                    plan_version,
                );
                Ok(handle)
            }
            Err(status) => {
                let _ = self.on_quarantine_full(epoch_id, plan_version);
                Err(status)
            }
        }
    }

    /// [RT worker] 迟到 worker ack 一个 quarantine 块（不释放；释放归 control）。
    pub fn note_late_ack(
        &self,
        handle: QuarantineHandle,
        worker: u8,
        epoch_id: u64,
        plan_version: PlanVersion,
        block_seq: u64,
    ) -> RtStatus {
        let status = self
            .quarantine
            .note_ack(handle, worker, epoch_id, plan_version, block_seq);
        self.sync_quarantine();
        status
    }

    // ---- [NONRT control] 安全迁移 / quarantine 释放 / 恢复 ----------------

    /// [NONRT control] 池满：禁止释放 + SafetyMute(RT_E_QUARANTINE_FULL)。
    pub fn on_quarantine_full(&mut self, epoch_id: u64, plan_version: PlanVersion) -> RtStatus {
        self.sync_quarantine();
        let _ = self
            .quarantine
            .forbid_release(RtStatus::RT_E_QUARANTINE_FULL);
        let _ = emit(
            &mut self.events,
            LANE_WORKER0,
            EV_QUARANTINE_EXHAUSTED,
            SEVERITY_FATAL,
            epoch_id,
            0,
            self.quarantine.in_use(),
            (epoch_id & 0xFFFF_FFFF) as u32,
        );
        self.enter_silence_mute(
            RtStatus::RT_E_QUARANTINE_FULL,
            GuardRequester::Master,
            epoch_id,
            plan_version,
        )
    }

    /// [NONRT control] 释放 all-ack 且未 latch 的 quarantine 槽（§7.6；失败保持占用）。
    pub fn release_quarantine(&mut self, handle: QuarantineHandle, epoch_id: u64) -> RtStatus {
        let status = self.quarantine.release(handle);
        if status.is_ok() {
            self.sync_quarantine();
            let _ = emit(
                &mut self.events,
                LANE_CONTROL,
                EV_QUARANTINE_RELEASE,
                SEVERITY_INFO,
                epoch_id,
                0,
                handle.raw() as u32,
                0,
            );
        }
        status
    }

    /// [NONRT control] worker 失联：pin 全部 quarantine（永久不释放）+ Faulted(401)。
    pub fn on_worker_lost(
        &mut self,
        worker: u8,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        let status = self.quarantine.pin_worker_lost(worker);
        if !status.is_ok() {
            return status;
        }
        self.counters.record_worker_lost();
        self.sync_quarantine();
        let _ = emit(
            &mut self.events,
            LANE_CONTROL,
            EV_WORKER_LOST,
            SEVERITY_FATAL,
            epoch_id,
            0,
            worker as u32,
            (epoch_id & 0xFFFF_FFFF) as u32,
        );
        self.enter_fault(
            RtStatus::RT_E_WORKER_LOST,
            GuardRequester::Control,
            epoch_id,
            plan_version,
        )
    }

    /// [NONRT control] 失核：pin 全部 quarantine + 安全 mute/Faulted（失核=整图拒收，§7.4）。
    ///
    /// DSP 核丢失 → RT_E_WORKER_LOST(401)；非 DSP（control/master/IO）核丢失 → RT_E_MASTER_LOST(402)。
    pub fn on_core_lost(
        &mut self,
        cpu: u16,
        dsp_cores: &[u16],
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        self.quarantine.pin_core_lost();
        self.counters.record_worker_lost();
        self.sync_quarantine();
        let mut dsp_lost = false;
        for core in dsp_cores.iter() {
            if *core == cpu {
                dsp_lost = true;
            }
        }
        let reason = if dsp_lost {
            RtStatus::RT_E_WORKER_LOST
        } else {
            RtStatus::RT_E_MASTER_LOST
        };
        let _ = emit(
            &mut self.events,
            LANE_CONTROL,
            EV_WORKER_LOST,
            SEVERITY_FATAL,
            epoch_id,
            0,
            cpu as u32,
            reason.0,
        );
        self.enter_fault(reason, GuardRequester::Control, epoch_id, plan_version)
    }

    /// [NONRT control] ack 槽耗尽（§7.6）：停止启动新块 + SafetyMute(308) + quarantine 不回收。
    pub fn on_ack_slot_exhausted(
        &mut self,
        occupied_slots: u32,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        self.counters.record_ack_slot_exhausted();
        let _ = self.quarantine.forbid_release(RT_E_ACK_SLOT_EXHAUSTED);
        self.sync_quarantine();
        let _ = emit(
            &mut self.events,
            LANE_CONTROL,
            EV_ACK_SLOT_EXHAUSTED,
            SEVERITY_FATAL,
            epoch_id,
            0,
            occupied_slots,
            (epoch_id & 0xFFFF_FFFF) as u32,
        );
        self.enter_silence_mute(
            RT_E_ACK_SLOT_EXHAUSTED,
            GuardRequester::Master,
            epoch_id,
            plan_version,
        )
    }

    /// [NONRT control] 连续迟到超阈值 → ProtectionFallback（fallback 未准入则 mute/fault）。
    pub fn on_late_streak(&mut self, epoch_id: u64, plan_version: PlanVersion) -> RtStatus {
        self.enter_fallback(
            RtStatus::RT_E_LATE_STREAK,
            GuardRequester::Master,
            epoch_id,
            plan_version,
        )
    }

    /// [NONRT control] 进入 ProtectionFallback（需 fallback 已准入；否则按 307 升级）。
    pub fn enter_fallback(
        &mut self,
        reason: RtStatus,
        requester: GuardRequester,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        self.apply_request(
            SafetyRequest::ProtectionFallback { reason, requester },
            epoch_id,
            plan_version,
        )
    }

    /// [NONRT control] 进入 SilenceMute（无预分配静音则升级 Faulted）。
    pub fn enter_silence_mute(
        &mut self,
        reason: RtStatus,
        requester: GuardRequester,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        self.apply_request(
            SafetyRequest::SilenceMute { reason, requester },
            epoch_id,
            plan_version,
        )
    }

    /// [NONRT control] 进入 Faulted（不可恢复；quarantine 永久保留）。
    pub fn enter_fault(
        &mut self,
        reason: RtStatus,
        requester: GuardRequester,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        self.apply_request(
            SafetyRequest::Fault { reason, requester },
            epoch_id,
            plan_version,
        )
    }

    /// 内部：执行迁移请求（split borrow，避免与 events/counters 借冲突）。
    fn apply_request(
        &mut self,
        request: SafetyRequest,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        let context = self.safety_context();
        let Self {
            safety,
            events,
            counters,
            ..
        } = self;
        safety.request(request, epoch_id, plan_version, context, events, counters)
    }

    /// [NONRT control] 把 watchdog 动作转成安全迁移（watchdog 只置位/返回动作）。
    pub fn service_watchdog(
        &mut self,
        action: WatchdogAction,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        match action {
            WatchdogAction::None => RtStatus::OK,
            WatchdogAction::RequestMute(reason) => {
                self.counters.record_watchdog_mute();
                let _ = emit(
                    &mut self.events,
                    LANE_WATCHDOG,
                    EV_WATCHDOG_MUTE,
                    SEVERITY_FATAL,
                    epoch_id,
                    0,
                    reason.0,
                    (epoch_id & 0xFFFF_FFFF) as u32,
                );
                self.enter_silence_mute(reason, GuardRequester::Watchdog, epoch_id, plan_version)
            }
            WatchdogAction::Fault(reason) => {
                self.counters.record_worker_lost();
                self.enter_fault(reason, GuardRequester::Watchdog, epoch_id, plan_version)
            }
        }
    }

    /// [NONRT control] 消费 ctl.safety_request（watchdog/master 置位，§11.13）。
    pub fn service_control_request(
        &mut self,
        ctl: &TspiRtControlV1,
        epoch_id: u64,
        plan_version: PlanVersion,
    ) -> RtStatus {
        let context = self.safety_context();
        let Self {
            safety,
            events,
            counters,
            ..
        } = self;
        safety.service_control_request(ctl, epoch_id, plan_version, context, events, counters)
    }

    /// [NONRT control] 完整重新 admission 后恢复 Normal（唯一退出路径）。
    pub fn recover(&mut self, ticket: &ReAdmissionTicket, epoch_id: u64) -> RtStatus {
        let Self {
            safety,
            events,
            counters,
            ..
        } = self;
        safety.recover(ticket, epoch_id, events, counters)
    }

    // ---- [NONRT] 事件导出 / 指标导出 ---------------------------------------

    /// [NONRT] 按 lane 顺序排空最多 out.len() 条事件；返回实际条数。
    pub fn drain_events(&self, out: &mut [TspiEventV1]) -> usize {
        let mut written = 0usize;
        let mut lane = 0u8;
        while lane < EVENT_LANES as u8 && written < out.len() {
            if let Some(head) = self.events.lane(lane) {
                let reader = EventLaneReader::new(head);
                while written < out.len() {
                    match reader.next() {
                        Some(event) => {
                            out[written] = event;
                            written += 1;
                        }
                        None => break,
                    }
                }
            }
            lane += 1;
        }
        written
    }

    /// [NONRT] 写完整故障导出报告（§8.4：带 graph_id/plan_version/epoch_id/采样窗口、
    /// 直方图边界与单位/ns、计数、quarantine、安全状态、fallback 准入）。
    ///
    /// RT 内禁止调用（不格式化、不写文件/网络；文件/HTTP 由 T26 metrics 线程负责）。
    pub fn write_export<W: fmt::Write>(&self, meta: &ExportMeta, out: &mut W) -> fmt::Result {
        write!(out, "t21-fault-export/v1\n")?;
        write!(out, "graph_id=")?;
        for byte in meta.graph_id.iter() {
            write!(out, "{:02x}", byte)?;
        }
        write!(out, "\n")?;
        write!(out, "plan_version={}\n", meta.plan_version)?;
        write!(out, "epoch_id={}\n", meta.epoch_id)?;
        write!(
            out,
            "sample_window.start_seq={} end_seq={} scope=block_start_seq\n",
            meta.window_start_seq, meta.window_end_seq
        )?;
        write!(out, "sample_rate_hz={}\n", meta.sample_rate_hz)?;
        write!(out, "block_size={}\n", meta.block_size)?;
        write!(out, "tick_freq_hz={}\n", meta.tick_freq_hz)?;
        write!(
            out,
            "histogram.unit_note=ticks are tick_freq_hz ticks; ns=ticks*1e9/tick_freq_hz\n"
        )?;
        write!(out, "watchdog.core={}\n", meta.watchdog_core)?;
        write!(
            out,
            "watchdog.stale_samples={}\n",
            meta.watchdog_stale_samples
        )?;
        write!(
            out,
            "watchdog.latched={}\n",
            if meta.watchdog_latched { 1 } else { 0 }
        )?;
        write!(
            out,
            "events.dropped_total={}\n",
            self.events.dropped_total()
        )?;
        self.counters.snapshot().write_text(out)?;
        self.histogram
            .snapshot(&self.config)
            .write_text(meta.tick_freq_hz, out)?;
        let snapshot = self.quarantine.snapshot();
        write!(out, "quarantine.entries={}\n", snapshot.entries)?;
        write!(out, "quarantine.retained={}\n", snapshot.retained)?;
        write!(out, "quarantine.released={}\n", snapshot.released)?;
        write!(out, "quarantine.exhausted={}\n", snapshot.exhausted)?;
        write!(
            out,
            "quarantine.release_forbidden={}\n",
            snapshot.release_forbidden
        )?;
        write!(out, "quarantine.forbid_reason={}\n", snapshot.forbid_reason)?;
        write!(out, "quarantine.core_lost={}\n", snapshot.core_lost)?;
        write!(out, "quarantine.lost_workers={}\n", snapshot.lost_workers)?;
        let mut index = 0usize;
        while index < QUARANTINE_SLOTS {
            let slot = snapshot.slots[index];
            write!(
                out,
                "quarantine.slot.index={} occupied={} epoch_id={} plan_version={} block_seq={} pending_ack_mask=0x{:x}\n",
                index,
                if slot.occupied { 1 } else { 0 },
                slot.epoch_id,
                slot.plan_version,
                slot.block_seq,
                slot.pending_ack_mask
            )?;
            index += 1;
        }
        write!(out, "safety.mode={}\n", self.safety.mode().name())?;
        let reason = match self.safety.reason() {
            Some(status) => status.0,
            None => 0,
        };
        write!(out, "safety.reason={}\n", reason)?;
        write!(
            out,
            "safety.block_start_allowed={}\n",
            if self.safety.block_start_allowed() {
                1
            } else {
                0
            }
        )?;
        write!(out, "safety.revision={}\n", self.safety.revision())?;
        match self.safety.transition_key() {
            Some(key) => write!(
                out,
                "safety.transition_key={}->{} reason={} epoch_id={} plan_version={}\n",
                key.from.name(),
                key.to.name(),
                key.reason,
                key.epoch_id,
                key.plan_version
            )?,
            None => write!(out, "safety.transition_key=none\n")?,
        }
        write!(
            out,
            "safety.duplicated_requests={}\n",
            self.safety.duplicated_requests()
        )?;
        write!(
            out,
            "safety.ignored_requests={}\n",
            self.safety.ignored_requests()
        )?;
        write!(
            out,
            "safety.fallback_entries={}\n",
            self.safety.fallback_entries()
        )?;
        write!(out, "safety.mute_entries={}\n", self.safety.mute_entries())?;
        write!(
            out,
            "safety.fault_entries={}\n",
            self.safety.fault_entries()
        )?;
        write!(out, "safety.mute_exits={}\n", self.safety.mute_exits())?;
        match self.fallback {
            Some(fallback) => {
                write!(
                    out,
                    "fallback.ready={}\n",
                    if fallback.is_ready() { 1 } else { 0 }
                )?;
                write!(out, "fallback.plan_version={}\n", fallback.plan_version)?;
                write!(out, "fallback.graph_bytes={}\n", fallback.graph_bytes)?;
                write!(
                    out,
                    "fallback.safety_silence_bytes={}\n",
                    fallback.safety_silence_bytes
                )?;
                write!(out, "fallback.worst_block_ns={}\n", fallback.worst_block_ns)?;
                write!(out, "fallback.budget_ns={}\n", fallback.budget_ns)?;
                write!(out, "fallback.headroom_ns={}\n", fallback.headroom_ns())?;
                write!(
                    out,
                    "fallback.identity_g={}\n",
                    if fallback.identity_g { 1 } else { 0 }
                )?;
                write!(
                    out,
                    "fallback.p_post_present={}\n",
                    if fallback.p_post_present { 1 } else { 0 }
                )?;
            }
            None => {
                write!(out, "fallback.ready=0\n")?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 编译期断言（core-v1 §3.3/§8.4/§13；T06 S1 静态断言口径）
// ---------------------------------------------------------------------------

const _: () = {
    // 固定容量（§3.3）。
    assert!(HIST_WORKERS as u8 == RT_WORKERS);
    assert!(HIST_METRIC_COUNT == HistMetric::COUNT);
    assert!(HIST_BUCKETS == 32);
    assert!(QUARANTINE_SLOTS == 8);
    assert!(EVENT_LANES == 8 && EVENT_LANE_SLOTS == 128);
    // 直方图固定内存 = admission ControlPlane 计费口径（6*4*32*4 = 3072 B，align 64）。
    assert!(HISTOGRAM_BYTES == 3072);
    assert!(core::mem::size_of::<HistogramTable>() == HISTOGRAM_BYTES);
    assert!(core::mem::align_of::<HistogramTable>() == 64);
    // 事件 lane 集合尺寸 = admission 计费口径（8×(64 B header + 128×64 B 记录) = 66 048 B）。
    assert!(core::mem::size_of::<EventLanes>() == 66_048);
    // 错误码冻结值抽查（§9.2）。
    assert!(RtStatus::RT_E_WATCHDOG.0 == 406);
    assert!(RtStatus::RT_E_WORKER_LOST.0 == 401);
    assert!(RT_E_ACK_SLOT_EXHAUSTED.0 == 308);
    assert!(RtStatus::RT_E_QUARANTINE_FULL.0 == 306);
    assert!(RtStatus::RT_E_FALLBACK_UNAVAILABLE.0 == 307);
    // §13：WD/RT 可见类型无 Drop。
    assert!(!core::mem::needs_drop::<SafetyAction>());
    assert!(!core::mem::needs_drop::<SafetyMode>());
    assert!(!core::mem::needs_drop::<GuardRequester>());
    assert!(!core::mem::needs_drop::<WatchdogAction>());
    assert!(!core::mem::needs_drop::<SafetyRequest>());
    assert!(!core::mem::needs_drop::<SafetyContext>());
    assert!(!core::mem::needs_drop::<TransitionKey>());
    assert!(!core::mem::needs_drop::<EventRingLane>());
    assert!(!core::mem::needs_drop::<EventLanes>());
    assert!(!core::mem::needs_drop::<HistMetric>());
    assert!(!core::mem::needs_drop::<HistogramConfig>());
    assert!(!core::mem::needs_drop::<HistogramTable>());
    assert!(!core::mem::needs_drop::<HistogramSnapshot>());
    assert!(!core::mem::needs_drop::<FaultCounters>());
    assert!(!core::mem::needs_drop::<CountersSnapshot>());
    assert!(!core::mem::needs_drop::<QuarantineHandle>());
    assert!(!core::mem::needs_drop::<QuarantineSlot>());
    assert!(!core::mem::needs_drop::<QuarantineSlotSnapshot>());
    assert!(!core::mem::needs_drop::<QuarantineSnapshot>());
    assert!(!core::mem::needs_drop::<QuarantinePool>());
    assert!(!core::mem::needs_drop::<WatchdogConfig>());
    assert!(!core::mem::needs_drop::<WatchdogSample>());
    assert!(!core::mem::needs_drop::<Watchdog>());
    assert!(!core::mem::needs_drop::<FallbackGraphSpec>());
    assert!(!core::mem::needs_drop::<FallbackAdmission>());
    assert!(!core::mem::needs_drop::<ReAdmissionTicket>());
    assert!(!core::mem::needs_drop::<SafetyState>());
};

// ---------------------------------------------------------------------------
// 测试注册（同 T16/T17/T18：cargo 不自动发现 tests/T21/；不在 public_registration.rs 注册）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fault_tests {
    include!("../tests/T21/fault_models.rs");
}
