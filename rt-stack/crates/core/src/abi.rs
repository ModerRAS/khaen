//! ABI v1 字节契约（core-v1 §11 与附录 B.4）。
//!
//! T06 scaffold：固定 11 个 repr(C) 跨 RT 边界结构、魔数/版本/域分离常量与
//! 编译期静态断言（size/align/key offset/needs_drop）。**不实现** plan 打包、
//! graph_id 计算或任何 RT 逻辑（T12/T13/T26 负责）。本模块不是 RT 执行路径，
//! 但其结构是跨 RT 边界 POD：禁止出现 Drop 类型、bool、usize、裸指针或携带
//! payload 的 enum。
//!
//! 来源：rt-stack/spec/core-v1.md §11 / 附录 B.4（期望值）。
//! 结构 hash 规则：header_crc32c = CRC-32C；graph_id = SHA-256（见 §11.2/§11.10）。

use core::sync::atomic::{AtomicU32, AtomicU64};

/// ABI 结构/语义版本；不兼容变化必须 +1。
pub const ABI_VERSION: u16 = 1;
/// plan 魔数：ASCII "TSP1" 的 LE u32。
pub const TSPI_PLAN_MAGIC: u32 = 0x5453_5031;
/// ring 魔数：ASCII "TSPR" 的 LE u32。
pub const TSPI_RING_MAGIC: u32 = 0x5453_5052;
/// graph_id 哈希域分离字符串（含 1 个 0x00 字节）。
pub const GRAPH_HASH_DOMAIN: &[u8] = b"tspi-plan/v1\x00";

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
pub const TSPI_PLAN_ABI_V1: u16 = 1;
/// plan ABI v2（per-role IIR(PEQ) 区；additive 扩展，header 128 B 不变）。
pub const TSPI_PLAN_ABI_V2: u16 = 2;
/// plan ABI v2 IIR 表魔数：ASCII "TSI2" 的 LE u32（表头/表项见 §11.10 v2 附录）。
pub const TSPI_PLAN_IIR_MAGIC: u32 = 0x5453_4932;
/// IIR 表头字节数（`TspiPlanIirHeaderV2`）。
pub const PLAN_IIR_HEADER_BYTES: usize = 16;
/// IIR 表项字节数（`TspiRoleIirV2`）。
pub const PLAN_IIR_ENTRY_BYTES: usize = 32;
/// 每段 biquad 的 f32 系数个数：`[b0,b1,b2,a1,a2]`（a0 归一化为 1）。
pub const PLAN_IIR_SECTION_COEFFS: usize = 5;
/// IIR 区版本（与 plan abi_version 独立演进）。
pub const TSPI_PLAN_IIR_VERSION: u16 = 1;
/// 单 role 最大 IIR 段数（与 `TspiRoleV1.peq_count` 上限一致）。
pub const PLAN_IIR_MAX_SECTIONS: usize = 32;

/// plan ABI 版本是否被本实现接受（v1 旧 plan + v2 IIR plan）。
pub const fn plan_abi_supported(version: u16) -> bool {
    version == TSPI_PLAN_ABI_V1 || version == TSPI_PLAN_ABI_V2
}

// ---------------------------------------------------------------------------
// TspiPlanHeaderV1 布局常量（core-v1 §11.2 v1.1.2 errata E1 定稿；旧 94/102/106 作废）
// ---------------------------------------------------------------------------

/// `TspiPlanHeaderV1` 大小（errata E1 后不变）。
pub const PLAN_HEADER_SIZE: usize = 128;
/// `TspiPlanHeaderV1` 对齐（repr(C, align(64))）。
pub const PLAN_HEADER_ALIGN: usize = 64;
/// reserved1 偏移（E1 显式 padding，u16；把 u32 对齐到 96）。
pub const PLAN_HEADER_OFFSET_RESERVED1: usize = 94;
/// affinity_table_offset 偏移（v1.1/I15 + E1：u32@96；旧字面 94 作废）。
pub const PLAN_HEADER_OFFSET_AFFINITY_TABLE: usize = 96;
/// affinity_count 偏移（v1.1/I15 + E1：u16@100；旧字面 98 作废）。
pub const PLAN_HEADER_OFFSET_AFFINITY_COUNT: usize = 100;
/// reserved2 偏移（E1 显式 padding，u16；把 u32 对齐到 104）。
pub const PLAN_HEADER_OFFSET_RESERVED2: usize = 102;
/// ctl_meta_offset 偏移（E1：u32@104；旧字面 102 作废）。
pub const PLAN_HEADER_OFFSET_CTL_META: usize = 104;
/// ctl_meta_len 偏移（E1：u32@108；旧字面 106 作废）。
pub const PLAN_HEADER_OFFSET_CTL_META_LEN: usize = 108;
/// reserved3 偏移（E1：[u32;3]@112）。
pub const PLAN_HEADER_OFFSET_RESERVED3: usize = 112;
/// header_crc32c 偏移（E1：u32@124）。
pub const PLAN_HEADER_OFFSET_CRC: usize = 124;

/// TspiHwParamsV1.sample_format 枚举值（v1 冻结，core-v1 §11.9）。
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

/// block 身份（64 B，align 64）。RT 只通过本结构判断 epoch/version/边界。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiBlockDescV1 {
    /// = ABI_VERSION。
    pub abi_version: u16,
    /// 块大小 B（v1 准入只允许 64/128）。
    pub block_size: u16,
    /// 本块输入通道数。
    pub n_in: u16,
    /// 本块输出通道数。
    pub n_out: u16,
    /// 时间轴 ID（u64 单调、永不复用）。
    pub epoch_id: u64,
    /// epoch 内块序号（从 0 连续 +1）。
    pub block_seq: u64,
    /// 输入绝对帧号（= block_seq * B）。
    pub frame_index: u64,
    /// 产生本块的 plan 版本。
    pub plan_version: u32,
    /// 状态位（core-v1 §6.2）。
    pub flags: u32,
    /// 有效帧数（0..=block_size；只有 DRAIN_TAIL 可 < B）。
    pub valid_frames: u32,
    /// RtArena 中 payload 槽号。
    pub slot_index: u32,
    /// capture 发布时刻（CLOCK_MONOTONIC）。
    pub mono_publish_ns: u64,
    /// 本块必须被 playback 消费的绝对时刻。
    pub play_deadline_ns: u64,
}

impl TspiBlockDescV1 {
    /// 全零占位（仅 scaffold/测试用；VALID 位为 0，不得发布给 playback）。
    pub const ZERO: Self = Self {
        abi_version: ABI_VERSION,
        block_size: 0,
        n_in: 0,
        n_out: 0,
        epoch_id: 0,
        block_seq: 0,
        frame_index: 0,
        plan_version: 0,
        flags: 0,
        valid_frames: 0,
        slot_index: 0,
        mono_publish_ns: 0,
        play_deadline_ns: 0,
    };
}

/// epoch 元数据（64 B，align 64）。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiEpochInfoV1 {
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

/// 事件记录（64 B，align 64；core-v1 §8.1/§11.5）。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiEventV1 {
    /// lane 内单调序号（从 0 起）。
    pub seq: u64,
    /// 仅 I/O/CTL/WD 填写；DSP 事件写 0。
    pub mono_ns: u64,
    /// 关联 epoch。
    pub epoch_id: u64,
    /// 关联块；非块事件写 0。
    pub block_seq: u64,
    /// 事件类型（core-v1 §8.2）。
    pub kind: u16,
    /// 0..3 DSP worker、4 capture、5 playback、6 watchdog、7 control。
    pub lane: u8,
    /// 1 info / 2 warning / 3 error / 4 fatal。
    pub severity: u8,
    /// kind 语义。
    pub value_a: u32,
    /// kind 语义。
    pub value_b: u32,
    /// 必须为 0。
    pub reserved0: u32,
    /// kind 专用；未用必须写 0。
    pub payload: [u64; 2],
}

impl TspiEventV1 {
    /// 全零事件（scaffold/固定数组初始化用）。
    pub const ZERO: Self = Self {
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
    };
}

/// 硬件参数（64 B，align 64；core-v1 §11.9）。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiHwParamsV1 {
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

/// plan 头（128 B，align 64；core-v1 §11.10 v1.1.2 errata E1 定稿）。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiPlanHeaderV1 {
    /// = TSPI_PLAN_MAGIC。
    pub magic: u32,
    /// = ABI_VERSION。
    pub abi_version: u16,
    /// = 128。
    pub header_size: u16,
    /// 提交计数（不参与 graph_id）。
    pub plan_version: u32,
    /// 48000 / 96000。
    pub sample_rate: u32,
    /// 64 / 128。
    pub block_size: u16,
    /// 输入通道。
    pub n_in: u16,
    /// 输出（物理槽）数。
    pub n_out: u16,
    /// M（低带 participants 数，<= 8）。
    pub low_band_count: u16,
    /// 非零矩阵路径数（<= MAX_MATRIX_PATHS）。
    pub path_count: u16,
    /// 对角 FIR 数（<= MAX_DIAG_FIRS）。
    pub diag_fir_count: u16,
    /// 本 plan 每 route 有效支撑上界。
    pub max_taps: u32,
    /// 4 或 2。
    pub workers: u8,
    /// bit0=stereo_strict、bit1=modal_support_to_other_outputs、bit2=crossfade、
    /// bit3=extended_taps_admitted、bit4=fallback_graph、bit5..7=0。
    pub flags: u8,
    /// 0=f32（v1 唯一允许）。
    pub precision: u8,
    /// 必须为 0。
    pub reserved0: u8,
    /// 规范化内容 SHA-256（§11.10 规则）。
    pub graph_id: [u8; 32],
    /// 系数区总字节。
    pub total_coeff_bytes: u32,
    /// Path 表偏移。
    pub path_table_offset: u32,
    /// Diag 表偏移。
    pub diag_table_offset: u32,
    /// 输入通道 stride（样本，16 的倍数）。
    pub in_stride: u32,
    /// 输出通道 stride（样本，16 的倍数）。
    pub out_stride: u32,
    /// Role 表偏移。
    pub role_table_offset: u32,
    /// role 数（<= 16）。
    pub role_table_count: u16,
    /// 必须为 0（v1.1.2 errata E1 显式 padding：把 u32 对齐到 96）。
    pub reserved1: u16,
    /// **v1.1/I15 + errata E1（v1.1.2 定稿）**：`TspiWorkerAffinityV1` 表偏移（0 = 无）。
    ///
    /// E1 布局：reserved1@94 为显式 padding（u16），本字段 **u32@96**、affinity_count **u16@100**、
    /// reserved2@102 显式 padding、ctl_meta_offset **u32@104**、ctl_meta_len **u32@108**、
    /// reserved3 [u32;3]@112、header_crc32c@124；size **128** / align **64** 不变。
    /// v1.1 初版字面 94/98/102/106 的 u32 非 4 字节对齐、Rust repr(C) 不可表达，已作废。
    pub affinity_table_offset: u32,
    /// **v1.1/I15**：affinity 条目数（<= 4）。
    pub affinity_count: u16,
    /// 必须为 0（v1.1.2 errata E1 显式 padding：把 u32 对齐到 104）。
    pub reserved2: u16,
    /// role 名等 CTL 元数据偏移（errata E1：102 -> 104）。
    pub ctl_meta_offset: u32,
    /// 元数据字节数（errata E1：106 -> 108）。
    pub ctl_meta_len: u32,
    /// 必须为 0。
    pub reserved3: [u32; 3],
    /// 覆盖 header 的 CRC-32C（本字段置 0 后计算）。
    pub header_crc32c: u32,
}

/// 路径表项（32 B，align 8；core-v1 §11.10）。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiPathV1 {
    /// 输入通道索引（< N）。
    pub in_index: u16,
    /// 物理输出索引（< O）。
    pub out_index: u16,
    /// 1..=max_taps。
    pub taps: u32,
    /// 输出侧整数延迟 D（含在支撑内）。
    pub delay_samples: u32,
    /// 1 或 -1。
    pub polarity: i8,
    /// bit0=diag_baseline_included、bit1=fused、bit2=zero_pad_linear_conv、
    /// bit3=input_side_delay，其余 0。
    pub flags: u8,
    /// 必须为 0。
    pub reserved0: u16,
    /// 系数 blob 偏移（64 B 对齐）。
    pub coeff_offset_bytes: u64,
    /// = taps * 4（f32）。
    pub coeff_len_bytes: u32,
    /// 必须为 0。
    pub reserved1: u32,
}

/// 对角 FIR 表项（32 B，align 8；core-v1 §11.10）。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiDiagFirV1 {
    /// 物理输出索引。
    pub out_index: u16,
    /// bit0=protection、bit1=limiter、bit2=crossover、bit3=target、
    /// bit4=phase_delay_polarity_only，其余 0。
    pub flags: u16,
    /// 1..=max_taps。
    pub taps: u32,
    /// 延迟 D。
    pub delay_samples: u32,
    /// 必须为 0。
    pub reserved0: u32,
    /// 系数 blob 偏移（64 B 对齐）。
    pub coeff_offset_bytes: u64,
    /// = taps * 4（f32）。
    pub coeff_len_bytes: u32,
    /// 必须为 0。
    pub reserved1: u32,
}

/// role 表项（32 B，align 8；core-v1 §11.10）。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiRoleV1 {
    /// 0..15（plan 内序号）。
    pub role_id: u16,
    /// 物理输出索引（无效用 0xFFFF）。
    pub out_index: u16,
    /// bit0=enabled、bit1=zero_fill，其余 0。
    pub flags: u16,
    /// 必须为 0。
    pub reserved0: u16,
    /// 角色级整数延迟（0..9600 @48k，200 ms 上限）。
    pub delay_samples: u32,
    /// Q15 线性增益（1.0 = 32767）。
    pub gain_q15: i16,
    /// 1 / -1。
    pub polarity: i8,
    /// 必须为 0。
    pub reserved1: u8,
    /// 角色级 IIR 段数（<= 32）。
    pub peq_count: u16,
    /// 必须为 0。
    pub reserved2: u16,
    /// role 名 CRC-32C（快速标识）。
    pub name_hash: u32,
    /// 必须为 0。
    pub reserved3: u32,
    /// 必须为 0。
    pub reserved4: u32,
}

/// plan ABI v2 IIR 表头（16 B，align 8；core-v1 §11.10 v2 附录 / GAP-T66-3）。
///
/// 表区位于 ctl_meta 之后、系数 blob 之前；`table_crc32c` 覆盖表头（本字段置 0）+
/// entries + 按 entry 顺序的系数 blob 原始字节（防系数被替换而 graph_id 校验缺失的洞）。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiPlanIirHeaderV2 {
    /// = `TSPI_PLAN_IIR_MAGIC`。
    pub magic: u32,
    /// = `TSPI_PLAN_IIR_VERSION`（1）。
    pub version: u16,
    /// = 32（`PLAN_IIR_ENTRY_BYTES`）。
    pub entry_bytes: u16,
    /// 表项数（1..=16；必须 == header.reserved3[1]）。
    pub entry_count: u16,
    /// 必须为 0。
    pub reserved0: u16,
    /// 覆盖 表头(crc=0)+entries+系数 blob（见类型注释）。
    pub table_crc32c: u32,
}

/// plan ABI v2 每 role IIR 表项（32 B，align 8；core-v1 §11.10 v2 附录 / GAP-T66-3）。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TspiRoleIirV2 {
    /// 命中的 role 表 role_id（表项按 role_id 严格升序）。
    pub role_id: u16,
    /// IIR 阶数：当前唯一支持 2（biquad）。
    pub order: u16,
    /// 段数（1..=32；必须 == 对应 role.peq_count）。
    pub section_count: u16,
    /// 必须为 0。
    pub flags: u16,
    /// 系数 blob 偏移（相对系数区起点，64 B 对齐）。
    pub coeff_offset_bytes: u64,
    /// = section_count × 5 × 4。
    pub coeff_len_bytes: u32,
    /// 必须为 0。
    pub reserved0: u32,
    /// 必须为 0。
    pub reserved1: u32,
    /// 必须为 0。
    pub reserved2: u32,
}

/// worker block ack（64 B，align 64；core-v1 §11.6）。含原子字段，不是 Copy。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct TspiBlockAckV1 {
    /// 本 ack 对应块。
    pub block_seq: u64,
    /// 本 ack 对应 epoch。
    pub epoch_id: u64,
    /// B1/B2/B3 完成位图（bit k = worker k）。
    pub phase_done: [AtomicU32; 3],
    /// 1 = 无效块；worker 必须丢弃。
    pub cutoff: AtomicU32,
    /// 本块 plan。
    pub plan_version: u32,
    /// 4（2 核 profile 为 2）。
    pub expected_workers: u8,
    /// bit0=valid、bit1=late_seen，其余 0。
    pub flags: u8,
    /// 必须为 0。
    pub reserved0: u16,
    /// 本块 hard cutoff。
    pub deadline_mono_ns: u64,
    /// master B1->B3 的 tick 差（预算内 2 次采样）。
    pub compute_ticks: u64,
    /// 必须为 0。
    pub reserved1: u64,
}

impl TspiBlockAckV1 {
    /// 构造 all-zero ack（scaffold/测试用）。
    pub const fn new(
        block_seq: u64,
        epoch_id: u64,
        plan_version: u32,
        expected_workers: u8,
    ) -> Self {
        Self {
            block_seq,
            epoch_id,
            phase_done: [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
            cutoff: AtomicU32::new(0),
            plan_version,
            expected_workers,
            flags: 0,
            reserved0: 0,
            deadline_mono_ns: 0,
            compute_ticks: 0,
            reserved1: 0,
        }
    }
}

/// SPSC 环头（64 B，align 64；core-v1 §11.7）。环只存 desc 值，不存指针。
#[repr(C, align(64))]
#[derive(Debug)]
pub struct TspiSpscHeaderV1 {
    /// = TSPI_RING_MAGIC。
    pub magic: u32,
    /// = ABI_VERSION。
    pub abi_version: u16,
    /// = 64（BlockDesc 字节数）。
    pub entry_bytes: u16,
    /// 固定 4。
    pub capacity: u32,
    /// desc 数组 stride（64）。
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

impl TspiSpscHeaderV1 {
    /// 构造固定容量 ring 头（scaffold/测试用）。
    pub const fn new(capacity: u32, entry_bytes: u16) -> Self {
        Self {
            magic: TSPI_RING_MAGIC,
            abi_version: ABI_VERSION,
            entry_bytes,
            capacity,
            entry_stride: entry_bytes as u32,
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            epoch_id: AtomicU64::new(0),
            reserved: [0; 2],
        }
    }
}

/// per-worker 统计（64 B，align 64；core-v1 §11.8）。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TspiWorkerStatsV1 {
    /// 完成块数。
    pub blocks_done: u64,
    /// 有用计算 tick（不含等待）。
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
    /// bit0=alive、bit1=last_block_late、bit2=cutoff_seen，其余 0。
    pub flags: u64,
}

/// 每 worker 一条的 plan 内 affinity 表项（32 B，align 8；core-v1 §11.10/§11.13 I15）。
///
/// **align 例外（v1.1.1/N4）**：本类型是 plan 内 POD 表项，固定 **32 B / align 8**，
/// 不是跨线程共享的原子结构（§11.13 其余 6 个共享结构才是 align(64)）。
/// RT/非 RT 均可按值拷贝；无 Drop。flags bit0=rt、bit1=isolated_cpu（T45 定义 IRQ 位义）。
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TspiWorkerAffinityV1 {
    /// worker 编号 0..=3。
    pub worker_id: u8,
    /// FIFO 调度优先级初值（T45 建议 70）。
    pub sched_priority: u8,
    /// bit0=rt、bit1=isolated_cpu，其余 0。
    pub flags: u16,
    /// 实际拓扑 CPU 序号（T45 实测后回写）。
    pub cpu_index: u16,
    /// 必须为 0（显式 padding，对齐 irq_dependency_mask）。
    pub reserved0: u16,
    /// 依赖的 IRQ/设备位图（T45 定义位义；未知位=115）。
    pub irq_dependency_mask: u64,
    /// 必须为 0。
    pub reserved1: u64,
    /// 必须为 0。
    pub reserved2: u64,
}

// ---------------------------------------------------------------------------
// 编译期静态断言（core-v1 A-ABI-6 / B-12 / D-19；期望值见附录 B.4）
// ---------------------------------------------------------------------------

const _: () = {
    // TspiBlockDescV1
    assert!(core::mem::size_of::<TspiBlockDescV1>() == 64);
    assert!(core::mem::align_of::<TspiBlockDescV1>() == 64);
    assert!(core::mem::offset_of!(TspiBlockDescV1, epoch_id) == 8);
    assert!(core::mem::offset_of!(TspiBlockDescV1, block_seq) == 16);
    assert!(core::mem::offset_of!(TspiBlockDescV1, frame_index) == 24);
    assert!(core::mem::offset_of!(TspiBlockDescV1, plan_version) == 32);
    assert!(core::mem::offset_of!(TspiBlockDescV1, flags) == 36);
    assert!(core::mem::offset_of!(TspiBlockDescV1, valid_frames) == 40);
    assert!(core::mem::offset_of!(TspiBlockDescV1, slot_index) == 44);
    assert!(core::mem::offset_of!(TspiBlockDescV1, mono_publish_ns) == 48);
    assert!(core::mem::offset_of!(TspiBlockDescV1, play_deadline_ns) == 56);
    assert!(!core::mem::needs_drop::<TspiBlockDescV1>());

    // TspiEpochInfoV1
    assert!(core::mem::size_of::<TspiEpochInfoV1>() == 64);
    assert!(core::mem::align_of::<TspiEpochInfoV1>() == 64);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, plan_version) == 8);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, sample_rate) == 12);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, block_size) == 16);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, n_in) == 18);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, n_out) == 20);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, mode) == 22);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, flags) == 24);
    assert!(core::mem::offset_of!(TspiEpochInfoV1, start_mono_ns) == 32);
    assert!(!core::mem::needs_drop::<TspiEpochInfoV1>());

    // TspiHwParamsV1
    assert!(core::mem::size_of::<TspiHwParamsV1>() == 64);
    assert!(core::mem::align_of::<TspiHwParamsV1>() == 64);
    assert!(core::mem::offset_of!(TspiHwParamsV1, sample_format) == 2);
    assert!(core::mem::offset_of!(TspiHwParamsV1, n_in) == 4);
    assert!(core::mem::offset_of!(TspiHwParamsV1, n_out) == 6);
    assert!(core::mem::offset_of!(TspiHwParamsV1, sample_rate) == 8);
    assert!(core::mem::offset_of!(TspiHwParamsV1, period_frames) == 12);
    assert!(core::mem::offset_of!(TspiHwParamsV1, buffer_frames) == 16);
    assert!(core::mem::offset_of!(TspiHwParamsV1, in_stride_bytes) == 20);
    assert!(core::mem::offset_of!(TspiHwParamsV1, out_stride_bytes) == 24);
    assert!(core::mem::offset_of!(TspiHwParamsV1, flags) == 28);
    assert!(!core::mem::needs_drop::<TspiHwParamsV1>());

    // TspiPlanHeaderV1
    assert!(core::mem::size_of::<TspiPlanHeaderV1>() == 128);
    assert!(core::mem::align_of::<TspiPlanHeaderV1>() == 64);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, plan_version) == 8);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, block_size) == 16);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, path_count) == 24);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, workers) == 32);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, graph_id) == 36);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, total_coeff_bytes) == 68);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, role_table_offset) == 88);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, role_table_count) == 92);
    // v1.1.2 errata E1（T02 定稿）：repr(C) 合法对齐版（旧字面 94/98/102/106 作废）。
    assert!(core::mem::size_of::<TspiPlanHeaderV1>() == PLAN_HEADER_SIZE);
    assert!(core::mem::align_of::<TspiPlanHeaderV1>() == PLAN_HEADER_ALIGN);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, reserved1) == PLAN_HEADER_OFFSET_RESERVED1);
    assert!(
        core::mem::offset_of!(TspiPlanHeaderV1, affinity_table_offset)
            == PLAN_HEADER_OFFSET_AFFINITY_TABLE
    );
    assert!(
        core::mem::offset_of!(TspiPlanHeaderV1, affinity_count)
            == PLAN_HEADER_OFFSET_AFFINITY_COUNT
    );
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, reserved2) == PLAN_HEADER_OFFSET_RESERVED2);
    assert!(
        core::mem::offset_of!(TspiPlanHeaderV1, ctl_meta_offset) == PLAN_HEADER_OFFSET_CTL_META
    );
    assert!(
        core::mem::offset_of!(TspiPlanHeaderV1, ctl_meta_len) == PLAN_HEADER_OFFSET_CTL_META_LEN
    );
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, reserved3) == PLAN_HEADER_OFFSET_RESERVED3);
    assert!(core::mem::offset_of!(TspiPlanHeaderV1, header_crc32c) == PLAN_HEADER_OFFSET_CRC);
    assert!(!core::mem::needs_drop::<TspiPlanHeaderV1>());

    // TspiWorkerAffinityV1（§11.13 I15 / B-18；32 B / align 8 例外）
    assert!(core::mem::size_of::<TspiWorkerAffinityV1>() == 32);
    assert!(core::mem::align_of::<TspiWorkerAffinityV1>() == 8);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, worker_id) == 0);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, sched_priority) == 1);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, flags) == 2);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, cpu_index) == 4);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, reserved0) == 6);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, irq_dependency_mask) == 8);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, reserved1) == 16);
    assert!(core::mem::offset_of!(TspiWorkerAffinityV1, reserved2) == 24);
    assert!(!core::mem::needs_drop::<TspiWorkerAffinityV1>());

    // TspiEventV1
    assert!(core::mem::size_of::<TspiEventV1>() == 64);
    assert!(core::mem::align_of::<TspiEventV1>() == 64);
    assert!(core::mem::offset_of!(TspiEventV1, mono_ns) == 8);
    assert!(core::mem::offset_of!(TspiEventV1, epoch_id) == 16);
    assert!(core::mem::offset_of!(TspiEventV1, block_seq) == 24);
    assert!(core::mem::offset_of!(TspiEventV1, kind) == 32);
    assert!(core::mem::offset_of!(TspiEventV1, lane) == 34);
    assert!(core::mem::offset_of!(TspiEventV1, severity) == 35);
    assert!(core::mem::offset_of!(TspiEventV1, value_a) == 36);
    assert!(core::mem::offset_of!(TspiEventV1, value_b) == 40);
    assert!(core::mem::offset_of!(TspiEventV1, payload) == 48);
    assert!(!core::mem::needs_drop::<TspiEventV1>());

    // TspiBlockAckV1
    assert!(core::mem::size_of::<TspiBlockAckV1>() == 64);
    assert!(core::mem::align_of::<TspiBlockAckV1>() == 64);
    assert!(core::mem::offset_of!(TspiBlockAckV1, phase_done) == 16);
    assert!(core::mem::offset_of!(TspiBlockAckV1, cutoff) == 28);
    assert!(core::mem::offset_of!(TspiBlockAckV1, plan_version) == 32);
    assert!(core::mem::offset_of!(TspiBlockAckV1, expected_workers) == 36);
    assert!(core::mem::offset_of!(TspiBlockAckV1, flags) == 37);
    assert!(core::mem::offset_of!(TspiBlockAckV1, deadline_mono_ns) == 40);
    assert!(core::mem::offset_of!(TspiBlockAckV1, compute_ticks) == 48);
    assert!(!core::mem::needs_drop::<TspiBlockAckV1>());

    // TspiSpscHeaderV1
    assert!(core::mem::size_of::<TspiSpscHeaderV1>() == 64);
    assert!(core::mem::align_of::<TspiSpscHeaderV1>() == 64);
    assert!(core::mem::offset_of!(TspiSpscHeaderV1, capacity) == 8);
    assert!(core::mem::offset_of!(TspiSpscHeaderV1, entry_stride) == 12);
    assert!(core::mem::offset_of!(TspiSpscHeaderV1, write_pos) == 16);
    assert!(core::mem::offset_of!(TspiSpscHeaderV1, read_pos) == 24);
    assert!(core::mem::offset_of!(TspiSpscHeaderV1, dropped) == 32);
    assert!(core::mem::offset_of!(TspiSpscHeaderV1, epoch_id) == 40);
    assert!(!core::mem::needs_drop::<TspiSpscHeaderV1>());

    // TspiWorkerStatsV1
    assert!(core::mem::size_of::<TspiWorkerStatsV1>() == 64);
    assert!(core::mem::align_of::<TspiWorkerStatsV1>() == 64);
    assert!(!core::mem::needs_drop::<TspiWorkerStatsV1>());

    // TspiPathV1 / TspiDiagFirV1 / TspiRoleV1
    assert!(core::mem::size_of::<TspiPathV1>() == 32);
    assert!(core::mem::align_of::<TspiPathV1>() == 8);
    assert!(core::mem::offset_of!(TspiPathV1, in_index) == 0);
    assert!(core::mem::offset_of!(TspiPathV1, out_index) == 2);
    assert!(core::mem::offset_of!(TspiPathV1, taps) == 4);
    assert!(core::mem::offset_of!(TspiPathV1, delay_samples) == 8);
    assert!(core::mem::offset_of!(TspiPathV1, polarity) == 12);
    assert!(core::mem::offset_of!(TspiPathV1, flags) == 13);
    assert!(core::mem::offset_of!(TspiPathV1, coeff_offset_bytes) == 16);
    assert!(core::mem::offset_of!(TspiPathV1, coeff_len_bytes) == 24);
    assert!(!core::mem::needs_drop::<TspiPathV1>());

    assert!(core::mem::size_of::<TspiDiagFirV1>() == 32);
    assert!(core::mem::align_of::<TspiDiagFirV1>() == 8);
    assert!(core::mem::offset_of!(TspiDiagFirV1, out_index) == 0);
    assert!(core::mem::offset_of!(TspiDiagFirV1, flags) == 2);
    assert!(core::mem::offset_of!(TspiDiagFirV1, taps) == 4);
    assert!(core::mem::offset_of!(TspiDiagFirV1, delay_samples) == 8);
    assert!(core::mem::offset_of!(TspiDiagFirV1, coeff_offset_bytes) == 16);
    assert!(core::mem::offset_of!(TspiDiagFirV1, coeff_len_bytes) == 24);
    assert!(!core::mem::needs_drop::<TspiDiagFirV1>());

    assert!(core::mem::size_of::<TspiRoleV1>() == 32);
    assert!(core::mem::align_of::<TspiRoleV1>() == 8);
    assert!(core::mem::offset_of!(TspiRoleV1, role_id) == 0);
    assert!(core::mem::offset_of!(TspiRoleV1, out_index) == 2);
    assert!(core::mem::offset_of!(TspiRoleV1, flags) == 4);
    assert!(core::mem::offset_of!(TspiRoleV1, delay_samples) == 8);
    assert!(core::mem::offset_of!(TspiRoleV1, gain_q15) == 12);
    assert!(core::mem::offset_of!(TspiRoleV1, polarity) == 14);
    assert!(core::mem::offset_of!(TspiRoleV1, peq_count) == 16);
    assert!(core::mem::offset_of!(TspiRoleV1, name_hash) == 20);
    assert!(!core::mem::needs_drop::<TspiRoleV1>());

    // plan ABI v2：IIR 表头 / 表项（GAP-T66-3，additive；header size 128 不变）
    assert!(core::mem::size_of::<TspiPlanIirHeaderV2>() == PLAN_IIR_HEADER_BYTES);
    assert!(core::mem::align_of::<TspiPlanIirHeaderV2>() == 8);
    assert!(core::mem::offset_of!(TspiPlanIirHeaderV2, magic) == 0);
    assert!(core::mem::offset_of!(TspiPlanIirHeaderV2, version) == 4);
    assert!(core::mem::offset_of!(TspiPlanIirHeaderV2, entry_bytes) == 6);
    assert!(core::mem::offset_of!(TspiPlanIirHeaderV2, entry_count) == 8);
    assert!(core::mem::offset_of!(TspiPlanIirHeaderV2, reserved0) == 10);
    assert!(core::mem::offset_of!(TspiPlanIirHeaderV2, table_crc32c) == 12);
    assert!(!core::mem::needs_drop::<TspiPlanIirHeaderV2>());

    assert!(core::mem::size_of::<TspiRoleIirV2>() == PLAN_IIR_ENTRY_BYTES);
    assert!(core::mem::align_of::<TspiRoleIirV2>() == 8);
    assert!(core::mem::offset_of!(TspiRoleIirV2, role_id) == 0);
    assert!(core::mem::offset_of!(TspiRoleIirV2, order) == 2);
    assert!(core::mem::offset_of!(TspiRoleIirV2, section_count) == 4);
    assert!(core::mem::offset_of!(TspiRoleIirV2, flags) == 6);
    assert!(core::mem::offset_of!(TspiRoleIirV2, coeff_offset_bytes) == 8);
    assert!(core::mem::offset_of!(TspiRoleIirV2, coeff_len_bytes) == 16);
    assert!(core::mem::offset_of!(TspiRoleIirV2, reserved0) == 20);
    assert!(core::mem::offset_of!(TspiRoleIirV2, reserved1) == 24);
    assert!(core::mem::offset_of!(TspiRoleIirV2, reserved2) == 28);
    assert!(!core::mem::needs_drop::<TspiRoleIirV2>());
    assert!(plan_abi_supported(TSPI_PLAN_ABI_V1));
    assert!(plan_abi_supported(TSPI_PLAN_ABI_V2));
    assert!(!plan_abi_supported(3));

    // 端序：ABI 只支持小端（A-ABI-4）。
    assert!(cfg!(target_endian = "little"));
};
