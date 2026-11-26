//! [RT] 内核运行时派发：标量参考入口 vs 本机 SIMD 入口。
//!
//! 本文件**不是** RT-MODULE 纯模块（没有 RT-MODULE 标记）：它含运行期 CPU 特性探测
//! （std::is_x86_feature_detected!），而仓库的 rt-check F-40 规则禁止 RT-MODULE 文件出现
//! 运行期探测。这里把"探测 + 一次分支"隔离在冷路径薄层里：探测结果由 std 内部静态缓存
//! （首次调用后只是一次原子读），每次 block 只多一次可预测分支；真正的 RT 计算全部在
//! [`crate::kernels::direct_simd`] / [`crate::kernels::partitioned_simd`] 的纯 RT 模块里，
//! 那两个文件的 forbid/drop-audit 由 rt-check 正常扫描。
//!
//! 派发规则（与平台架构绑定，不做全局 -C target-cpu/-C target-feature）：
//!
//! - x86_64：运行期同时探测 AVX2 与 FMA，二者都有才走 AVX2+FMA 内核，否则回退标量参考；
//! - aarch64：NEON 是 ARMv8-A 基线，直接走 NEON 内核；
//! - 其它架构：回退标量参考。
//!
//! 目标机：N150（Gracemont，AVX2+FMA、无 AVX-512）与 RK3576（Cortex-A72+A53，NEON）。
//! 两条 SIMD 路径都不使用 AVX-512/FMA4 等可选扩展，编译产物在未开启该指令集的构建里也安全
//! （指令只在探测通过的运行期分支里执行）。

use crate::kernels::direct::{self, DirectCoeffsF32, DirectLayout, DirectTopology};
use crate::kernels::partitioned::{self, PartitionedPlan};
use crate::kernels::RtStatus;

/// 本机实际可用的 SIMD 内核后端（平台探测结果）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdBackend {
    /// 标量参考内核（无可用 SIMD 或平台不在派发表内）。
    Scalar,
    /// x86_64 AVX2 + FMA（8 x f32 / 指令）。
    Avx2Fma,
    /// aarch64 NEON（4 x f32 / 指令）。
    Neon,
}

impl SimdBackend {
    /// 稳定名字（报告/JSON/人类可读表用）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Avx2Fma => "avx2+fma",
            Self::Neon => "neon",
        }
    }

    /// 运行期探测本机后端（各平台都是"检测 + 一次分支"；std 会缓存探测结果）。
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
                Self::Avx2Fma
            } else {
                Self::Scalar
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self::Neon
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self::Scalar
        }
    }

    /// 是否走 SIMD 内核（非标量参考）。
    pub fn is_simd(self) -> bool {
        self != Self::Scalar
    }
}

/// [RT] direct 卷积的优化入口：按本机能力选 SIMD 内核，不可用时回退标量参考。
///
/// 与 [`direct::direct_block_f32`] 语义相同（含 partial/flush、V=0 早退、错误码），差异只有
/// SIMD 路径的浮点舍入与 state 内部历史方向（见 [`crate::kernels::direct_simd`] 模块文档；
/// 标量与 SIMD 内核不得在同一 epoch 内共用同一条 state）。
#[allow(clippy::too_many_arguments)]
pub fn direct_block_f32_optimized(
    topo: &DirectTopology,
    layout: &DirectLayout,
    coeffs: &DirectCoeffsF32<'_>,
    state: &mut [f32],
    input: &[f32],
    in_stride: usize,
    output: &mut [f32],
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
) -> RtStatus {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        if SimdBackend::detect().is_simd() {
            // SAFETY: 仅当 detect() 返回非 Scalar 时进入：x86_64 已确认 AVX2+FMA，
            // aarch64 的 NEON 是基线；两条 SIMD 内核都只读输入/系数、只写调用方预分配的
            // state/output，不分配、不加锁、不做 IO、无 panic 分支。
            return unsafe {
                crate::kernels::direct_simd::direct_block_simd_dispatch(
                    topo,
                    layout,
                    coeffs,
                    state,
                    input,
                    in_stride,
                    output,
                    out_stride,
                    block_size,
                    valid_frames,
                )
            };
        }
    }
    direct::direct_block_f32(
        topo,
        layout,
        coeffs,
        state,
        input,
        in_stride,
        output,
        out_stride,
        block_size,
        valid_frames,
    )
}

/// [RT] 分区卷积的优化入口：按本机能力选 SIMD 内核，不可用时回退标量参考。
///
/// 与 [`partitioned::partitioned_block_f32`] 语义相同（含终止性 DRAIN/reset、错误码），
/// state 字节布局也相同（SIMD 与标量可在同一 epoch 内交替调用）；差异只有 FFT/复数乘加/
/// 输出缩放的浮点舍入（FMA 融合），见 [`crate::kernels::partitioned_simd`] 模块文档。
#[allow(clippy::too_many_arguments)]
pub fn partitioned_block_f32_optimized(
    topo: &DirectTopology,
    plan: &PartitionedPlan,
    plan_data: &[f32],
    state: &mut [f32],
    input: &[f32],
    in_stride: usize,
    output: &mut [f32],
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
) -> RtStatus {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        if SimdBackend::detect().is_simd() {
            // SAFETY: 仅当 detect() 返回非 Scalar 时进入：x86_64 已确认 AVX2+FMA，
            // aarch64 的 NEON 是基线；SIMD 内核只读输入/系数/plan、只写调用方预分配的
            // state/output，不分配、不加锁、不做 IO、无 panic 分支。
            return unsafe {
                crate::kernels::partitioned_simd::partitioned_block_simd_dispatch(
                    topo,
                    plan,
                    plan_data,
                    state,
                    input,
                    in_stride,
                    output,
                    out_stride,
                    block_size,
                    valid_frames,
                )
            };
        }
    }
    partitioned::partitioned_block_f32(
        topo,
        plan,
        plan_data,
        state,
        input,
        in_stride,
        output,
        out_stride,
        block_size,
        valid_frames,
    )
}
