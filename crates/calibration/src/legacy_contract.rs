// NOTE(port): legacy/pre-rewrite rt-stack/crates/{core/src/abi.rs,core/src/rt.rs,core/src/workers.rs}
// —— v1 硬件 ABI / RT 错误码 / 控制块契约替身（迁移期临时文件；ABI v3 接入后整体删除）。
//!
//! 为什么需要这个文件：v2 的 `khaen_core` 只保留 `rt::{BlockDesc, BLOCK_ABI_VERSION, ...}`
//! 与 plan ABI v3（`core::plan`），**没有** v1 的 `khaen_core::abi` / `khaen_core::workers`
//! 模块。capture / format / clock / stage_backend（T62/T67/T19/T22 写域）是围绕 v1 的设备
//! 格式（`KhaenHwParamsV1` / `sample_format`）与 RT 控制块（`KhaenRtControlV1`）写的；
//! 本次回迁 spike 的目标是"证明能搬回来"，故把**实际用到的 v1 POD/常量原样**集中在本文件，
//! 作为唯一契约替身（不复制算法、不改字段/布局/错误码），供 `legacy_format` / `legacy_clock`
//! / `capture` / `stage_backend` 引用。正式迁移应由 core 提供 v3 等价类型后整体删除。
//!
//! 来源（只读分支，逐块原样复制；`git show legacy/pre-rewrite:<path>`）：
//! - `rt-stack/crates/core/src/abi.rs`：`sample_format`、`ABI_VERSION`、`KHAEN_PLAN_MAGIC`、
//!   `KHAEN_RING_MAGIC`、plan ABI 版本支持判定、`KhaenEpochInfoV1`、`KhaenHwParamsV1`；
//! - `rt-stack/crates/core/src/rt.rs`：`RtStatus` 及其 §9.2 冻结错误码常量；
//! - `rt-stack/crates/core/src/workers.rs`：`KhaenRtControlV1`（§11.13；T19 TickBudget 用）。
//!
//! NOT_RUN：这些结构未与 v2 的 ABI v3 做布局等价校验（v3 目前没有硬件格式/RT 控制块定义）。

#![allow(dead_code)]

use core::sync::atomic::{AtomicU32, AtomicU64};

// ---------------------------------------------------------------------------
// 常量（原样自 core/src/abi.rs）
// ---------------------------------------------------------------------------

/// ABI 结构/语义版本；不兼容变化必须 +1。
pub const ABI_VERSION: u16 = 1;
/// plan 魔数：ASCII "TSP1" 的 LE u32。
pub const KHAEN_PLAN_MAGIC: u32 = 0x5453_5031;
/// ring 魔数：ASCII "TSPR" 的 LE u32。
pub const KHAEN_RING_MAGIC: u32 = 0x5453_5052;
/// graph_id 哈希域分离字符串（含 1 个 0x00 字节）。
pub const GRAPH_HASH_DOMAIN: &[u8] = b"khaen-plan/v1\x00";

// ---------------------------------------------------------------------------
// plan ABI 版本（GAP-T66-3：per-role IIR(PEQ) 区，ABI v2 扩展）
// ---------------------------------------------------------------------------
//
// v1：128 B header + path/diag/role/affinity 表 + ctl_meta + 系数 blob；role 只有
//     `peq_count` 计数、没有系数区（PEQ 无法进入 plan）。
// v2（additive，header size 仍 128、magic 不变）：header.reserved3[0] = IIR 表偏移、
//     reserved3[1] = IIR 表项数；IIR 表与系数仍落在既有表区/系数区，v1 解析路径不变。
//     旧 host 读到 abi_version=2 会明确 ERR_ABI_VERSION（不会把 v2 当 v1 运行）。

/// plan ABI v1（无 role IIR 系数区；S0/S1 及全部旧 plan）。
pub const KHAEN_PLAN_ABI_V1: u16 = 1;
/// plan ABI v2（per-role IIR(PEQ) 区；additive 扩展，header 128 B 不变）。
pub const KHAEN_PLAN_ABI_V2: u16 = 2;
/// plan ABI v2 IIR 表魔数：ASCII "TSI2" 的 LE u32（表头/表项见 §11.10 v2 附录）。
pub const KHAEN_PLAN_IIR_MAGIC: u32 = 0x5453_4932;
/// IIR 表头字节数（`KhaenPlanIirHeaderV2`）。
pub const PLAN_IIR_HEADER_BYTES: usize = 16;
/// IIR 表项字节数（`KhaenRoleIirV2`）。
pub const PLAN_IIR_ENTRY_BYTES: usize = 32;
/// 每段 biquad 的 f32 系数个数：`[b0,b1,b2,a1,a2]`（a0 归一化为 1）。
pub const PLAN_IIR_SECTION_COEFFS: usize = 5;
/// IIR 区版本（与 plan abi_version 独立演进）。
pub const KHAEN_PLAN_IIR_VERSION: u16 = 1;
/// 单 role 最大 IIR 段数（与 `KhaenRoleV1.peq_count` 上限一致）。
pub const PLAN_IIR_MAX_SECTIONS: usize = 32;

/// plan ABI 版本是否被本实现接受（v1 旧 plan + v2 IIR plan）。
pub const fn plan_abi_supported(version: u16) -> bool {
    version == KHAEN_PLAN_ABI_V1 || version == KHAEN_PLAN_ABI_V2
}

// ---------------------------------------------------------------------------
// 设备 sample_format 枚举值（原样自 core/src/abi.rs）
// ---------------------------------------------------------------------------

/// KhaenHwParamsV1.sample_format 枚举值（v1 冻结，core-v1 §11.9）。
pub mod sample_format {
    /// core 内部 canonical；不允许来自设备。
    pub const F32_PLANAR: u16 = 0;
    /// MOTU/ALSA 24-bit 常见格式。
    pub const S24_3LE_INTERLEAVED: u16 = 1;
    /// ALSA 32-bit 容器。
    pub const S32_LE_INTERLEAVED: u16 = 2;
    /// 测试/兼容。
    pub const S16_LE_INTERLEAVED: u16 = 3;
    /// 文件/fake backend。
    pub const F32_INTERLEAVED: u16 = 4;
    /// 仅 reference/offline；runtime 准入拒绝。
    pub const F64_PLANAR_REFERENCE: u16 = 5;
}

// ---------------------------------------------------------------------------
// KhaenEpochInfoV1（原样自 core/src/abi.rs）
// ---------------------------------------------------------------------------

/// epoch 元数据（64 B，align 64）。
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
    /// 0=usb_toslink、1=aes67、2=dante、3=file、4=other（只用于诊断）。
    pub mode: u16,
    /// bit0=first_after_discontinuity、bit1=fallback_epoch，其余 0。
    pub flags: u64,
    /// frame 0 的单调时刻。
    pub start_mono_ns: u64,
    /// 必须为 0。
    pub reserved: [u64; 3],
}

// ---------------------------------------------------------------------------
// KhaenHwParamsV1（原样自 core/src/abi.rs）
// ---------------------------------------------------------------------------

/// 硬件参数（64 B，align 64；core-v1 §11.9）。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KhaenHwParamsV1 {
    /// = ABI_VERSION。
    pub abi_version: u16,
    /// sample_format 值（见本模块 sample_format）。
    pub sample_format: u16,
    /// 设备输入通道。
    pub n_in: u16,
    /// 设备输出通道。
    pub n_out: u16,
    /// 48000 / 96000。
    pub sample_rate: u32,
    /// ALSA period/actual chunk（必须 = 64/128）。
    pub period_frames: u32,
    /// ALSA buffer（>= period）。
    pub buffer_frames: u32,
    /// 输入交错/平面缓冲跨通道字节步长。
    pub in_stride_bytes: u32,
    /// 输出交错/平面缓冲跨通道字节步长。
    pub out_stride_bytes: u32,
    /// bit0=dither_enabled、bit1=dither_tpdf、bit2=capture_sanitize、
    /// bit3=output_clip_meter，其余 0。
    pub flags: u32,
    /// reserved[0]=dither_seed（非 RT 配置），其余 0。
    pub reserved: [u64; 4],
}

// ---------------------------------------------------------------------------
// RtStatus（原样自 core/src/rt.rs）
// ---------------------------------------------------------------------------

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
// KhaenRtControlV1（原样自 core/src/workers.rs；T19 TickBudget 需要）
// ---------------------------------------------------------------------------

/// master 每 block spin 上界初值（原样自 workers.rs）。
pub const WORKER_DEFAULT_SPIN_MAX_ITERS: u32 = 1_000_000;
/// master/worker cutoff drain 上界初值（原样自 workers.rs）。
pub const WORKER_DEFAULT_CUTOFF_DRAIN_ITERS: u32 = 4096;

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
