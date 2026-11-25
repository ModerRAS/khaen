//! 实时卷积内核：全采样率 direct 卷积、共享 FFT 分区卷积与有理数多相重采样。
//!
//! RT-MODULE: [`direct::direct_block_f32`] 与 [`partitioned::partitioned_block_f32`] 是 RT 路径
//! 生产入口：只读写调用方预分配的切片，不分配、不加锁、不做 IO、无显式 `panic!`/`unwrap`
//! 分支（越界由各入口的 `validate_call` 在写入任何缓冲之前排除）。状态与 plan 的容量/构建
//! 全部在 NONRT：调用方按 `DirectLayout::state_samples()` / `PartitionedLayout::state_samples()`
//! 预分配并清零，plan 用 `PartitionedLayout::plan_samples()` / `build_scratch_samples()` 预分配。
//!
//! [`resample::RationalResampler`] 同样是 RT 入口：构造期（NONRT）算好系数与环形历史，
//! `process` 零分配、无锁、无 IO、无 panic；矩阵段的 3 kHz 低速率域用它做抽取 / 插值。
//!
//! 本模块从 legacy/pre-rewrite 分支（`rt-stack/crates/core/src/`）迁移，对应关系：
//! [`direct`] ← 旧 `direct.rs`（WBS T07）；[`partitioned`] ← 旧 `partitioned.rs`（WBS T08）；
//! [`RtStatus`] 与下面几个旧 `rt.rs` 常量 ← 旧 `rt.rs` 的最小必要子集（不整包搬）。
//! 旧 `abi.rs` 的 plan 表结构（`KhaenPathV1` 等）不进入内核：内核消费 reference 语义图
//! （A_map/a/g/p/scatter），方案 v3 到语义图的适配属 host/compile 层（方案 ABI 见 [`crate::plan`]）。
//!
//! 数值行为与旧实现逐位一致：固定累加顺序（u/v/y 阶段的 tap 升序、v 阶段的列 k 升序），
//! f32 是生产精度，f64 只作 offline oracle（与旧实现一致）。迁移未改动任何数值行为；旧实现
//! 遗留的文档/防御缺口以 `// NOTE(legacy-defect):` 原位标注，最终清单见迁移报告。
//!
//! 上限常量的来源约定：`crate::rt` 已收录的（[`crate::rt::MAX_OUTPUT_CHANNELS`]、
//! [`crate::rt::MAX_LOW_BAND_PARTICIPANTS`]、[`crate::rt::TAPS_NORMAL_MAX`]、
//! [`crate::rt::BLOCK_SIZES`]）直接引用，本模块不再镜像一份；`crate::rt` 尚未收录的
//! [`MAX_MATRIX_PATHS`]、[`MAX_PARTITIONS`]、[`TAPS_EXTENDED_MAX`] 暂存本模块，待 rt 层补齐
//! 后迁走（迁移报告已登记）。

pub mod direct;
pub mod partitioned;
pub mod resample;

// ---------------------------------------------------------------------------
// 旧 rt.rs 的最小错误码子集（内核可达的码）
// ---------------------------------------------------------------------------

/// RT/准入错误码：旧 `rt.rs` 固定错误码的最小必要子集。
///
/// `repr(transparent)` 的 u32 新类型、`Copy`、无 `Drop`；禁止带 payload 的
/// `Result<_, String>` 跨 RT 边界。旧码全表在 legacy 分支 `rt.rs`，本模块只保留内核入口会
/// 返回的码；数值与旧实现逐一相同（不得改号）。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RtStatus(pub u32);

impl RtStatus {
    /// 0 = OK。
    pub const OK: Self = Self(0);

    /// 字段/布局不符：layout 与拓扑不一致、`valid_frames > block_size` 等（旧码 103）。
    pub const ERR_BAD_LAYOUT: Self = Self(103);
    /// block 大小不合法：0 或超过内核上限（旧码 105）。
    pub const ERR_BLOCK_SIZE: Self = Self(105);
    /// 通道/容器容量越界（旧码 106）。
    pub const ERR_CHANNELS: Self = Self(106);
    /// 角色/映射/participant/scatter 非法（旧码 107）。
    pub const ERR_ROLES: Self = Self(107);
    /// stride 小于 block 大小（旧码 109）。
    pub const ERR_STRIDE_ALIGN: Self = Self(109);
    /// taps/D/融合支撑超限（旧码 110）。
    pub const ERR_TAPS: Self = Self(110);
    /// 系数数组长度或路由表内容不符（旧码 111）。
    pub const ERR_PATH_TABLE: Self = Self(111);
    /// 缓冲容量不足或 32 位偏移溢出（旧码 113）。
    pub const ERR_CAPACITY: Self = Self(113);

    /// 是否 OK（0）。
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
}

/// 每路径路由（输出角色, 源通道）数上限：旧 `rt.rs::MAX_MATRIX_PATHS`。
///
/// 新 `crate::rt` 暂未收录（T10c 只收录类型/块长/taps 常量），暂存本模块。
pub const MAX_MATRIX_PATHS: usize = 128;
/// 每路径分区数上限：旧 `rt.rs::MAX_PARTITIONS`（侧路 24000 点 / 低速率块 16 点 = 1500）。
///
/// 新 `crate::rt` 暂未收录，暂存本模块。
pub const MAX_PARTITIONS: u32 = 1_500;
/// extended profile 的融合/抽头上限：旧 `rt.rs::TAPS_EXTENDED_MAX`。
///
/// 新 `crate::rt` 暂未收录，暂存本模块。
pub const TAPS_EXTENDED_MAX: u32 = 16_384;

// ---------------------------------------------------------------------------
// 编译期静态断言（迁移值绑定；const assert 不是运行期 assert!）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(MAX_MATRIX_PATHS == 128);
    assert!(MAX_PARTITIONS == 1_500);
    assert!(TAPS_EXTENDED_MAX == 16_384);
    assert!(!core::mem::needs_drop::<RtStatus>());
};
