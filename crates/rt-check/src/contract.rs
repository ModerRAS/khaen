//! 本地 ABI / RT 契约镜像（TODO(core)：等 khaen-core 提供 ABI/RT 类型后替换并删除本模块）。
//!
//! rt-check 的 `abi-layout` 与 `drop-audit` 按 core-v1 **v1.1.2** 的期望布局（§11.2 常量表、
//! 附录 B.1..B.4）检查**真实** core / rt-host 公共类型。v2 重写期 `khaen-core` 只提供方案文件
//! ABI v3（docs/CONTRACTS.md A），尚未提供 RT 可见类型，因此这里按 core-v1 冻结定义保留最小
//! 镜像，让离线检查工具可以独立编译、独立跑门禁：
//!
//! - [`abi`]：§11.5..§11.10 的 POD 结构（字段/顺序/对齐与 v1.1.2 冻结版一致）；
//! - [`rt`]：§3.5 时限/长度公式 + §13.2 的 RT 可见类型；
//! - [`rt_host`]：§13.2 中 rt-host 的 5 个 RT 可见类型。
//!
//! **TODO(core)**：core v2 的 ABI/RT 模块落地后，`abi_layout` / `drop_audit` 应改回
//! `khaen_core::abi` / `khaen_core::rt` / `khaen_rt_host::*` 并同步 spec 的检查项数
//! （v2 方案文件 ABI v3 与 v1 plan 结构不同，预期值需要按 v3 重写）；本模块届时删除。
//! 在替换之前，本模块是 rt-check 唯一允许的"契约替身"，不改动检查规则。

/// core-v1 §11 ABI 结构镜像（size/align/offset 由 [`crate::abi_layout`] 校验）。
pub mod abi {
    use core::sync::atomic::{AtomicU32, AtomicU64};

    /// block 身份（64 B，align 64；core-v1 §11.4）。
    #[repr(C, align(64))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenBlockDescV1 {
        /// = ABI_VERSION。
        pub abi_version: u16,
        /// 块大小 B（64/128）。
        pub block_size: u16,
        /// 本块输入通道数。
        pub n_in: u16,
        /// 本块输出通道数。
        pub n_out: u16,
        /// 时间轴 ID。
        pub epoch_id: u64,
        /// epoch 内块序号。
        pub block_seq: u64,
        /// 输入绝对帧号。
        pub frame_index: u64,
        /// plan 版本。
        pub plan_version: u32,
        /// 状态位。
        pub flags: u32,
        /// 有效帧数。
        pub valid_frames: u32,
        /// payload 槽号。
        pub slot_index: u32,
        /// capture 发布时刻。
        pub mono_publish_ns: u64,
        /// 本块截止时刻。
        pub play_deadline_ns: u64,
    }

    /// epoch 元数据（64 B，align 64；core-v1 §11.4）。
    #[repr(C, align(64))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenEpochInfoV1 {
        /// 时间轴 ID。
        pub epoch_id: u64,
        /// 启动本 epoch 的 plan。
        pub plan_version: u32,
        /// 48000 / 96000。
        pub sample_rate: u32,
        /// 64 / 128。
        pub block_size: u16,
        /// 输入通道数。
        pub n_in: u16,
        /// 输出通道数。
        pub n_out: u16,
        /// 设备模式（诊断用）。
        pub mode: u16,
        /// 状态位。
        pub flags: u64,
        /// frame 0 的单调时刻。
        pub start_mono_ns: u64,
        /// 必须为 0。
        pub reserved: [u64; 3],
    }

    /// 事件记录（64 B，align 64；core-v1 §11.5）。
    #[repr(C, align(64))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenEventV1 {
        /// lane 内单调序号。
        pub seq: u64,
        /// 事件时刻（仅 I/O/CTL/WD 填写）。
        pub mono_ns: u64,
        /// 关联 epoch。
        pub epoch_id: u64,
        /// 关联块。
        pub block_seq: u64,
        /// 事件类型。
        pub kind: u16,
        /// lane 编号。
        pub lane: u8,
        /// 严重度。
        pub severity: u8,
        /// kind 语义。
        pub value_a: u32,
        /// kind 语义。
        pub value_b: u32,
        /// 必须为 0。
        pub reserved0: u32,
        /// kind 专用。
        pub payload: [u64; 2],
    }

    /// 硬件参数（64 B，align 64；core-v1 §11.9）。
    #[repr(C, align(64))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenHwParamsV1 {
        /// = ABI_VERSION。
        pub abi_version: u16,
        /// sample_format 枚举值。
        pub sample_format: u16,
        /// 设备输入通道。
        pub n_in: u16,
        /// 设备输出通道。
        pub n_out: u16,
        /// 48000 / 96000。
        pub sample_rate: u32,
        /// ALSA period/actual chunk。
        pub period_frames: u32,
        /// ALSA buffer。
        pub buffer_frames: u32,
        /// 输入跨通道字节步长。
        pub in_stride_bytes: u32,
        /// 输出跨通道字节步长。
        pub out_stride_bytes: u32,
        /// 状态位。
        pub flags: u32,
        /// reserved[0]=dither_seed。
        pub reserved: [u64; 4],
    }

    /// plan 头（128 B，align 64；core-v1 §11.10 v1.1.2 errata E1）。
    #[repr(C, align(64))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenPlanHeaderV1 {
        /// = KHAEN_PLAN_MAGIC。
        pub magic: u32,
        /// = ABI_VERSION。
        pub abi_version: u16,
        /// = 128。
        pub header_size: u16,
        /// 提交计数。
        pub plan_version: u32,
        /// 48000 / 96000。
        pub sample_rate: u32,
        /// 64 / 128。
        pub block_size: u16,
        /// 输入通道。
        pub n_in: u16,
        /// 输出（物理槽）数。
        pub n_out: u16,
        /// 低带 participants 数。
        pub low_band_count: u16,
        /// 非零矩阵路径数。
        pub path_count: u16,
        /// 对角 FIR 数。
        pub diag_fir_count: u16,
        /// 每 route 有效支撑上界。
        pub max_taps: u32,
        /// 4 或 2。
        pub workers: u8,
        /// 状态位。
        pub flags: u8,
        /// 0=f32。
        pub precision: u8,
        /// 必须为 0。
        pub reserved0: u8,
        /// 规范化内容 SHA-256。
        pub graph_id: [u8; 32],
        /// 系数区总字节。
        pub total_coeff_bytes: u32,
        /// Path 表偏移。
        pub path_table_offset: u32,
        /// Diag 表偏移。
        pub diag_table_offset: u32,
        /// 输入通道 stride（样本）。
        pub in_stride: u32,
        /// 输出通道 stride（样本）。
        pub out_stride: u32,
        /// Role 表偏移。
        pub role_table_offset: u32,
        /// role 数。
        pub role_table_count: u16,
        /// 必须为 0（errata E1 显式 padding）。
        pub reserved1: u16,
        /// affinity 表偏移（errata E1）。
        pub affinity_table_offset: u32,
        /// affinity 条目数。
        pub affinity_count: u16,
        /// 必须为 0（errata E1 显式 padding）。
        pub reserved2: u16,
        /// CTL 元数据偏移（errata E1）。
        pub ctl_meta_offset: u32,
        /// 元数据字节数（errata E1）。
        pub ctl_meta_len: u32,
        /// 必须为 0。
        pub reserved3: [u32; 3],
        /// 覆盖 header 的 CRC-32C。
        pub header_crc32c: u32,
    }

    /// 路径表项（32 B，align 8；core-v1 §11.10）。
    #[repr(C, align(8))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenPathV1 {
        /// 输入通道索引。
        pub in_index: u16,
        /// 物理输出索引。
        pub out_index: u16,
        /// 1..=max_taps。
        pub taps: u32,
        /// 输出侧整数延迟。
        pub delay_samples: u32,
        /// 1 或 -1。
        pub polarity: i8,
        /// 状态位。
        pub flags: u8,
        /// 必须为 0。
        pub reserved0: u16,
        /// 系数 blob 偏移。
        pub coeff_offset_bytes: u64,
        /// = taps * 4。
        pub coeff_len_bytes: u32,
        /// 必须为 0。
        pub reserved1: u32,
    }

    /// 对角 FIR 表项（32 B，align 8；core-v1 §11.10）。
    #[repr(C, align(8))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenDiagFirV1 {
        /// 物理输出索引。
        pub out_index: u16,
        /// 状态位。
        pub flags: u16,
        /// 1..=max_taps。
        pub taps: u32,
        /// 延迟。
        pub delay_samples: u32,
        /// 必须为 0。
        pub reserved0: u32,
        /// 系数 blob 偏移。
        pub coeff_offset_bytes: u64,
        /// = taps * 4。
        pub coeff_len_bytes: u32,
        /// 必须为 0。
        pub reserved1: u32,
    }

    /// role 表项（32 B，align 8；core-v1 §11.10）。
    #[repr(C, align(8))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct KhaenRoleV1 {
        /// 0..15。
        pub role_id: u16,
        /// 物理输出索引。
        pub out_index: u16,
        /// 状态位。
        pub flags: u16,
        /// 必须为 0。
        pub reserved0: u16,
        /// 角色级整数延迟。
        pub delay_samples: u32,
        /// Q15 线性增益。
        pub gain_q15: i16,
        /// 1 / -1。
        pub polarity: i8,
        /// 必须为 0。
        pub reserved1: u8,
        /// 角色级 IIR 段数。
        pub peq_count: u16,
        /// 必须为 0。
        pub reserved2: u16,
        /// role 名 CRC-32C。
        pub name_hash: u32,
        /// 必须为 0。
        pub reserved3: u32,
        /// 必须为 0。
        pub reserved4: u32,
    }

    /// worker block ack（64 B，align 64；core-v1 §11.6）。
    #[repr(C, align(64))]
    #[derive(Debug)]
    pub struct KhaenBlockAckV1 {
        /// 本 ack 对应块。
        pub block_seq: u64,
        /// 本 ack 对应 epoch。
        pub epoch_id: u64,
        /// B1/B2/B3 完成位图。
        pub phase_done: [AtomicU32; 3],
        /// 1 = 无效块。
        pub cutoff: AtomicU32,
        /// 本块 plan。
        pub plan_version: u32,
        /// 4（2 核 profile 为 2）。
        pub expected_workers: u8,
        /// 状态位。
        pub flags: u8,
        /// 必须为 0。
        pub reserved0: u16,
        /// 本块 hard cutoff。
        pub deadline_mono_ns: u64,
        /// master B1->B3 tick 差。
        pub compute_ticks: u64,
        /// 必须为 0。
        pub reserved1: u64,
    }

    /// SPSC 环头（64 B，align 64；core-v1 §11.7）。
    #[repr(C, align(64))]
    #[derive(Debug)]
    pub struct KhaenSpscHeaderV1 {
        /// = KHAEN_RING_MAGIC。
        pub magic: u32,
        /// = ABI_VERSION。
        pub abi_version: u16,
        /// = 64（BlockDesc 字节数）。
        pub entry_bytes: u16,
        /// 固定 4。
        pub capacity: u32,
        /// desc 数组 stride。
        pub entry_stride: u32,
        /// 生产者单调位置。
        pub write_pos: AtomicU64,
        /// 消费者单调位置。
        pub read_pos: AtomicU64,
        /// 丢块计数。
        pub dropped: AtomicU64,
        /// 环当前 epoch。
        pub epoch_id: AtomicU64,
        /// 必须为 0。
        pub reserved: [u64; 2],
    }

    /// per-worker 统计（64 B，align 64；core-v1 §11.8）。
    #[repr(C, align(64))]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct KhaenWorkerStatsV1 {
        /// 完成块数。
        pub blocks_done: u64,
        /// 有用计算 tick。
        pub busy_ticks: u64,
        /// 有界等待 tick。
        pub spin_ticks: u64,
        /// 最大单块 tick。
        pub max_block_ticks: u64,
        /// 迟到导致 invalid 的次数。
        pub late_blocks: u64,
        /// 见到的 invalid 块次数。
        pub invalid_blocks: u64,
        /// plan swap 次数。
        pub switches: u64,
        /// 状态位。
        pub flags: u64,
    }

    /// plan 内 affinity 表项（32 B，align 8；core-v1 §11.10/§11.13）。
    #[repr(C, align(8))]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct KhaenWorkerAffinityV1 {
        /// worker 编号 0..=3。
        pub worker_id: u8,
        /// FIFO 调度优先级初值。
        pub sched_priority: u8,
        /// 状态位。
        pub flags: u16,
        /// 实际拓扑 CPU 序号。
        pub cpu_index: u16,
        /// 必须为 0（显式 padding）。
        pub reserved0: u16,
        /// 依赖的 IRQ/设备位图。
        pub irq_dependency_mask: u64,
        /// 必须为 0。
        pub reserved1: u64,
        /// 必须为 0。
        pub reserved2: u64,
    }
}

/// core-v1 RT 类型镜像（§3.5 公式 + §13.2 needs_drop == false 类型清单）。
pub mod rt {
    use core::ptr::NonNull;

    use super::abi::{
        KhaenBlockAckV1, KhaenBlockDescV1, KhaenDiagFirV1, KhaenPathV1, KhaenPlanHeaderV1,
        KhaenRoleV1, KhaenWorkerStatsV1,
    };

    /// MOTU UltraLite-mk5 capture 上限（core-v1 §3）。
    pub const MAX_INPUT_CHANNELS: usize = 20;
    /// MOTU playback 上限（core-v1 §3）。
    pub const MAX_OUTPUT_CHANNELS: usize = 22;

    /// DSP worker 编号：0..=3。
    pub type WorkerId = u8;
    /// 运行期 plan 提交版本。
    pub type PlanVersion = u32;

    /// T_block = B / fs，向上取整 ns（不得因取整放宽门）。
    pub const fn t_block_ns_ceil(sample_rate_hz: u32, block_size: u16) -> u64 {
        let num = (block_size as u64) * 1_000_000_000;
        num.div_ceil(sample_rate_hz as u64)
    }

    /// gate_ns = T_block / 2，向下取整 ns（core-v1 §3.5/附录 B.1）。
    pub const fn gate_ns_floor(sample_rate_hz: u32, block_size: u16) -> u64 {
        (block_size as u64) * 1_000_000_000 / (2 * sample_rate_hz as u64)
    }

    /// 融合长度 L_F = L_G + L_P - 1（D 计入支撑）。
    pub const fn fused_len_max(l_g: u32, l_p: u32) -> u32 {
        l_g + l_p - 1
    }

    /// RT/准入错误码（repr(transparent) u32，Copy，无 Drop）。
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct RtStatus(pub u32);

    impl RtStatus {
        /// 0 = OK。
        pub const OK: RtStatus = RtStatus(0);
    }

    /// 非拥有、无 Drop 的 plan 借用（只在 block 内有效）。
    #[derive(Clone, Copy, Debug)]
    pub struct PlanRef {
        ptr: NonNull<KhaenPlanHeaderV1>,
        version: PlanVersion,
    }

    impl PlanRef {
        /// 构造（scaffold/测试用）。
        pub fn from_parts(ptr: NonNull<KhaenPlanHeaderV1>, version: PlanVersion) -> Self {
            Self { ptr, version }
        }

        /// 本 block 使用的 plan 版本。
        pub const fn version(self) -> PlanVersion {
            self.version
        }

        /// plan 头原始指针。
        pub fn as_ptr(self) -> *const KhaenPlanHeaderV1 {
            self.ptr.as_ptr()
        }
    }

    /// plan arena 的只读视图（引用，无 Drop）。
    #[derive(Clone, Copy, Debug)]
    pub struct PlanView<'a> {
        /// plan 头。
        pub header: &'a KhaenPlanHeaderV1,
        /// 路径表。
        pub paths: &'a [KhaenPathV1],
        /// 对角 FIR 表。
        pub diags: &'a [KhaenDiagFirV1],
        /// role 表。
        pub roles: &'a [KhaenRoleV1],
        /// 系数 blob。
        pub coeffs: &'a [u8],
    }

    /// 一个 block 的 planar f32 输入/输出视图（非拥有）。
    pub struct BlockIo<'a> {
        /// block 身份。
        pub desc: &'a KhaenBlockDescV1,
        /// 输入 planar f32。
        pub input: &'a [f32],
        /// 输出 planar f32。
        pub output: &'a mut [f32],
        /// 输入通道 stride（样本）。
        pub in_stride: u32,
        /// 输出通道 stride（样本）。
        pub out_stride: u32,
    }

    /// worker 的 RT 上下文（非拥有）。
    pub struct WorkerRtCtx<'a> {
        /// 本 worker 编号。
        pub worker: WorkerId,
        /// 本 block ack。
        pub ack: &'a KhaenBlockAckV1,
        /// 本 worker 统计。
        pub stats: &'a mut KhaenWorkerStatsV1,
    }

    /// RT 控制块（原子字段 + 固定数组，needs_drop == false）。
    #[repr(C, align(64))]
    #[derive(Debug)]
    pub struct RtControl {
        /// current plan arena 地址位（0 = 无）。
        pub current_plan_addr: core::sync::atomic::AtomicU64,
        /// pending plan arena 地址位（0 = 无）。
        pub pending_plan_addr: core::sync::atomic::AtomicU64,
        /// 当前生效 plan 版本。
        pub current_plan_version: core::sync::atomic::AtomicU32,
        /// 待生效 plan 版本。
        pub pending_plan_version: core::sync::atomic::AtomicU32,
        /// 切换状态。
        pub switch_state: core::sync::atomic::AtomicU32,
        /// retire gate 已就绪的旧版本。
        pub retire_ready_version: core::sync::atomic::AtomicU32,
        /// fault 请求。
        pub fault_request: core::sync::atomic::AtomicU32,
        /// 安全 mute 请求。
        pub mute_request: core::sync::atomic::AtomicU32,
        /// 当前 epoch_id。
        pub epoch_id: core::sync::atomic::AtomicU64,
        /// master heartbeat 的 block_seq。
        pub heartbeat_block_seq: core::sync::atomic::AtomicU64,
        /// 固定保留区。
        pub reserved: [u64; 25],
    }
}

/// rt-host 的 RT 可见类型镜像（core-v1 §13.2 清单中的 5 项）。
pub mod rt_host {
    use core::sync::atomic::{AtomicU64, Ordering};

    use super::abi::KhaenEventV1;
    use super::rt::RtStatus;

    /// 每 lane 固定槽数（§11.13）。
    pub const EVENT_LANE_SLOTS: usize = 128;

    /// 安全动作（repr(u8) POD，无 payload/Drop；§5.5）。
    #[repr(u8)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SafetyAction {
        /// 正常工作图。
        Normal = 0,
        /// G=I + 保留 P_post 最终保护。
        ProtectionFallback = 1,
        /// 预分配全零输出。
        SilenceMute = 2,
    }

    /// watchdog 观察结果（§5.1/§12.2）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WatchdogAction {
        /// 无动作。
        None,
        /// 请求安全 mute。
        RequestMute(RtStatus),
        /// 请求 fault（不自动恢复）。
        Fault(RtStatus),
    }

    /// dither 配置（repr(u8) POD；默认 off）。
    #[repr(u8)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum DitherCfg {
        /// 关闭（默认）。
        #[default]
        Off = 0,
        /// TPDF dither。
        Tpdf = 1,
    }

    /// 格式转换统计（非 RT 导出；RT 内只累加固定计数）。
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct FormatStats {
        /// 输入 nonfinite 样本数。
        pub nonfinite_in: u64,
        /// 输出 clip 样本数。
        pub clipped: u64,
        /// 输出 nonfinite 样本数。
        pub nonfinite_out: u64,
    }

    /// 单 lane 事件环（固定 128 槽 + 原子序号；lane 内单写者）。
    #[repr(C, align(64))]
    #[derive(Debug)]
    pub struct EventRingLane {
        /// lane 编号（0..7）。
        pub lane: u8,
        /// 必须为 0。
        pub reserved: [u8; 7],
        /// 已写入序号。
        pub write_seq: AtomicU64,
        /// 已读序号。
        pub read_seq: AtomicU64,
        /// 丢事件计数。
        pub dropped: AtomicU64,
        /// 固定槽。
        pub slots: [KhaenEventV1; EVENT_LANE_SLOTS],
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
                slots: [KhaenEventV1 {
                    seq: 0,
                    mono_ns: 0,
                    epoch_id: 0,
                    block_seq: 0,
                    kind: 0,
                    lane: 0,
                    severity: 0,
                    value_a: 0,
                    value_b: 0,
                    reserved0: 0,
                    payload: [0; 2],
                }; EVENT_LANE_SLOTS],
            }
        }

        /// 已发布（写入）的事件序号（下一槽）。
        pub fn next_seq(&self) -> u64 {
            self.write_seq.load(Ordering::Acquire)
        }
    }
}
