//! [RT] direct 卷积内核的显式 SIMD 实现（x86_64 AVX2+FMA / aarch64 NEON）。
//!
//! RT-MODULE: 本文件是 DSP 纯 worker 路径的 RT 模块：禁止析构/分配/锁/syscall/日志/panic；
//! 计算只用调用方预分配的 state/input/output 切片与栈上的定长累加数组（[f32; 128]），
//! 不分配、不加锁、不做 IO、无显式 panic 分支。
//!
//! # 语义
//!
//! 与标量参考入口 [`crate::kernels::direct::direct_block_f32`] 完全同语义：同样的三阶段
//! FIR（u = A x；v = (delta_D I + g) u；y = P v；z[scatter] = y）、同样的 valid_frames
//! partial 契约（输入 [V, block) 视作 0、输出 [V, block) 写 +0.0、历史按 V 个有效样本推进）、
//! 同样的 V=0 早退（只清输出、不推进历史）、同样的固定错误码与"校验失败前不写缓冲"。
//! 只有浮点累加顺序/FMA 融合引入的舍入差异（见下"数值契约"）。
//!
//! # 状态表示（与标量内核的差异）
//!
//! 分区容量/偏移与 [`crate::kernels::direct::DirectLayout`] 完全相同，但 SIMD 路径在
//! 同一批分区里按**时序（chronological）方向**存历史，而不是标量参考的倒序环形约定：
//!
//! ~~~text
//! 标量参考:  x_past[i][t] = x_i[block_start - 1 - t]        （t = 0 是最近样本）
//! SIMD 路径:  x_past[i][j] = x_i[block_start - (L_A-1) + j]  （j 升序 = 时间升序）
//! ~~~
//!
//! 这样每个 tap 的头部（历史）与尾部（当前块）都是随输出样本序号 n 单调递增的连续读，
//! 内层无需反向置换即可 8-wide/4-wide FMA。两条路径的 state 长度、偏移、清零初始化要求
//! 相同，但**不得在同一个 epoch 内混用**（交替调用标量/SIMD 会读到对方的镜像历史；
//! 换内核时必须重新清零 state 或换新 state）。部分帧/跨块历史的推进语义两条路径一致。
//!
//! # 派发规则
//!
//! 公开入口是 [`crate::kernels::dispatch::direct_block_f32_optimized`]：x86_64 运行期
//! 检测 AVX2+FMA，aarch64 直接用 NEON（ARMv8 基线，无需探测），其它情况回退标量参考。
//! 编译不需要 -C target-cpu/-C target-feature：SIMD 内层函数带 target_feature 属性，
//! 由 std::arch 内建函数在函数内生成对应指令，运行期未探测到指令集时不会被调用。
//!
//! # 数值契约
//!
//! 标量参考按 tap 升序、非融合（mul 再 add）逐样本累加；SIMD 路径在同一 tap 升序上用
//! FMA 融合（acc = fma(coef, src, acc)）且部分 tap 的加数与标量顺序不同，因此与标量参考
//! **非逐位一致**：实测 random 输入下 f32 每块 L2 相对误差 <= 1e-6、单样本绝对误差
//! <= 1e-6（tests/kernels_simd.rs 用随机输入 x taps{1,8,64,2048,4096} x block{64,128}
//! 逐样本断言并记录实测上界）。对 f64 oracle 的误差门与标量路径相同（partitioned 的
//! f32 相对 L2 <= 1e-5 口径）。系数为 0 的 tap 被整条跳过（标量参考会累加 0*样本）：
//! 对有限输入两者相等，故数值契约只覆盖有限系数/有限输入（生产准入已拒绝非有限系数）。

use crate::kernels::direct::{
    validate_call, DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_BLOCK,
    DIRECT_NO_SOURCE,
};
use crate::kernels::RtStatus;

/// 单行 FIR 累积函数的函数指针类型（架构相关的 SIMD 内层）。
///
/// 参数依次为：累加缓冲（长度 >= valid）、当前块缓冲（长度 >= block）、历史缓冲
/// （长度 = l-1）、tap 系数（长度 = l，或空切片表示"只有 delta 脉冲"）、delta 脉冲位置
/// （usize::MAX = 无）、有效样本数、声明支撑 l。
///
/// 语义：对 t in 0..l 执行 acc[n] += coef(t) * src(t, n)（n in 0..valid），其中
/// coef(t) = taps[t] + (t == delta_at ? 1 : 0)（taps 为空时 coef(t) 只在 t == delta_at
/// 处取 1），src(t, n) = n >= t ? cur[n-t] : past[(l-1) - t + n]。
type FirRowFn = unsafe fn(&mut [f32], &[f32], &[f32], &[f32], usize, usize, usize);

/// 跨块历史推进（时序方向）：cur 为当前块有效样本，past 长度 = L-1。
///
/// 新历史 = 最近 (L-1) 个样本（含本块 valid 个新样本），等价于把 past 左移 valid 再拼上
/// cur[0..valid]；valid >= past.len() 时整段历史都来自本块 cur。
fn update_history(cur: &[f32], past: &mut [f32], valid_frames: usize) {
    let k = past.len();
    if k == 0 {
        return;
    }
    if valid_frames >= k {
        let start = valid_frames - k;
        past.copy_from_slice(&cur[start..start + k]);
    } else {
        let shift = k - valid_frames;
        past.copy_within(valid_frames..k, 0);
        past[shift..shift + valid_frames].copy_from_slice(&cur[..valid_frames]);
    }
}

/// SIMD 入口的公共编排（架构无关；SIMD 算术全部在 fir_row 里）。
///
/// 只读 coeffs/topo/layout/input，只写 output 与 state；校验失败时直接返回固定错误码、
/// 不写任何缓冲。state 必须由调用方预分配并按布局长度清零。
#[allow(clippy::too_many_arguments)]
fn direct_block_simd_impl(
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
    fir_row: FirRowFn,
) -> RtStatus {
    let status = validate_call(
        topo,
        layout,
        coeffs.a_fir.len(),
        coeffs.g.len(),
        coeffs.p.len(),
        state.len(),
        input.len(),
        in_stride,
        output.len(),
        out_stride,
        block_size,
        valid_frames,
    );
    if !status.is_ok() {
        return status;
    }

    let n_src = topo.n_src as usize;
    let n_role = topo.n_role as usize;
    let containers = topo.container_channels as usize;
    let l_a = topo.l_a as usize;
    let l_g = topo.l_g as usize;
    let l_p = topo.l_p as usize;
    let d = topo.d_samples as usize;
    let m = topo.m as usize;

    let x_past_len = l_a - 1;
    let u_past_len = l_g - 1;
    let v_past_len = l_p - 1;
    let block = DIRECT_MAX_BLOCK;
    let x_past_off = layout.x_past_off as usize;
    let u_past_off = layout.u_past_off as usize;
    let v_past_off = layout.v_past_off as usize;
    let x_cur_off = layout.x_cur_off as usize;
    let u_cur_off = layout.u_cur_off as usize;
    let v_cur_off = layout.v_cur_off as usize;

    // 输出块先整体清零：未映射容器通道 / valid_frames 之后的样本必须逐样本严格 +0.0。
    let mut c = 0;
    while c < containers {
        let base = c * out_stride;
        output[base..base + block_size].fill(0.0);
        c += 1;
    }
    if valid_frames == 0 {
        return RtStatus::OK;
    }

    // 角色到 dense participant 位置（非 participant 为 DIRECT_NO_SOURCE）。
    let mut part_pos = [DIRECT_NO_SOURCE; crate::kernels::direct::DIRECT_MAX_ROLES];
    let mut a_idx = 0;
    while a_idx < m {
        part_pos[topo.participants[a_idx] as usize] = a_idx as u16;
        a_idx += 1;
    }

    // 阶段 0：当前块输入拷入 x_cur（时序方向的数据留在 x_cur，历史在 x_past）。
    let mut i = 0;
    while i < n_src {
        let in_base = i * in_stride;
        let cur_base = x_cur_off + i * block;
        state[cur_base..cur_base + valid_frames]
            .copy_from_slice(&input[in_base..in_base + valid_frames]);
        i += 1;
    }

    // 布局里全部历史区在 cur 区之前（x_past/u_past/v_past < x_cur/u_cur/v_cur）：
    // 一次 split 拿到两半，读写互不重叠，也不需要 unsafe 别名。
    let (past_area, cur_area) = state.split_at_mut(x_cur_off);
    let u_cur_rel = u_cur_off - x_cur_off;
    let v_cur_rel = v_cur_off - x_cur_off;
    let empty_taps: &[f32] = &[];

    // 阶段 1：u_j = a_src(j) * x_src(j)（A_map 只选择/零填）。
    let mut j = 0;
    while j < n_role {
        let u_rel = u_cur_rel + j * block;
        let src = topo.src_of_role[j];
        if src == DIRECT_NO_SOURCE {
            cur_area[u_rel..u_rel + valid_frames].fill(0.0);
        } else {
            let si = src as usize;
            let a_base = si * l_a;
            let past_rel = x_past_off + si * x_past_len;
            let cur_rel = si * block;
            let mut acc = [0.0f32; DIRECT_MAX_BLOCK];
            {
                let cur = &cur_area[cur_rel..cur_rel + block];
                let past = &past_area[past_rel..past_rel + x_past_len];
                let taps = &coeffs.a_fir[a_base..a_base + l_a];
                // SAFETY: fir_row 是本机派发选中的 SIMD 实现（调用方已确认指令集可用）；
                // 切片长度满足其契约（acc = DIRECT_MAX_BLOCK >= valid，cur = block，
                // past = l_a-1，taps = l_a，valid <= block_size <= DIRECT_MAX_BLOCK）。
                unsafe {
                    fir_row(&mut acc, cur, past, taps, usize::MAX, valid_frames, l_a);
                }
            }
            cur_area[u_rel..u_rel + valid_frames].copy_from_slice(&acc[..valid_frames]);
        }
        j += 1;
    }

    // 阶段 2：v_j = sum_k G[j][k] * u_k，G 含 identity baseline（D 计入支撑）。
    // 只有 participant x participant 的 (j,k) 才有 g 项；非对角且无 g 的行整行是 0，
    // 直接跳过（否则 l_g 很大时会空转 l_g 次）。对角无 g 时只剩 delta_D 一项。
    let mut j = 0;
    while j < n_role {
        let j_pos = part_pos[j];
        let mut acc = [0.0f32; DIRECT_MAX_BLOCK];
        let mut k = 0;
        while k < n_role {
            let k_pos = part_pos[k];
            let u_rel = u_cur_rel + k * block;
            let past_rel = u_past_off + k * u_past_len;
            let has_g = j_pos != DIRECT_NO_SOURCE && k_pos != DIRECT_NO_SOURCE;
            if has_g {
                let g_base = (j_pos as usize * m + k_pos as usize) * l_g;
                let delta = if j == k { d } else { usize::MAX };
                {
                    let cur = &cur_area[u_rel..u_rel + block];
                    let past = &past_area[past_rel..past_rel + u_past_len];
                    let taps = &coeffs.g[g_base..g_base + l_g];
                    // SAFETY: 同阶段 1；长度满足 fir_row 契约。
                    unsafe {
                        fir_row(&mut acc, cur, past, taps, delta, valid_frames, l_g);
                    }
                }
            } else if j == k {
                {
                    let cur = &cur_area[u_rel..u_rel + block];
                    let past = &past_area[past_rel..past_rel + u_past_len];
                    // SAFETY: 同阶段 1；taps 为空时 fir_row 只在一个 tap 上施加 delta。
                    unsafe {
                        fir_row(&mut acc, cur, past, empty_taps, d, valid_frames, l_g);
                    }
                }
            }
            k += 1;
        }
        let v_rel = v_cur_rel + j * block;
        cur_area[v_rel..v_rel + valid_frames].copy_from_slice(&acc[..valid_frames]);
        j += 1;
    }

    // 阶段 3：y_j = p_j * v_j，写 scatter 容器通道。
    let mut j = 0;
    while j < n_role {
        let p_base = j * l_p;
        let v_rel = v_cur_rel + j * block;
        let past_rel = v_past_off + j * v_past_len;
        let mut acc = [0.0f32; DIRECT_MAX_BLOCK];
        {
            let cur = &cur_area[v_rel..v_rel + block];
            let past = &past_area[past_rel..past_rel + v_past_len];
            let taps = &coeffs.p[p_base..p_base + l_p];
            // SAFETY: 同阶段 1；长度满足 fir_row 契约。
            unsafe {
                fir_row(&mut acc, cur, past, taps, usize::MAX, valid_frames, l_p);
            }
        }
        let out_base = topo.scatter[j] as usize * out_stride;
        output[out_base..out_base + valid_frames].copy_from_slice(&acc[..valid_frames]);
        j += 1;
    }

    // 阶段 4：历史推进（时序方向；每 block 一次 O(taps) 拷贝，不做逐样本移位）。
    let mut i = 0;
    while i < n_src {
        let cur_rel = i * block;
        let past_rel = x_past_off + i * x_past_len;
        update_history(
            &cur_area[cur_rel..cur_rel + block],
            &mut past_area[past_rel..past_rel + x_past_len],
            valid_frames,
        );
        i += 1;
    }
    let mut j = 0;
    while j < n_role {
        let cur_rel = u_cur_rel + j * block;
        let past_rel = u_past_off + j * u_past_len;
        update_history(
            &cur_area[cur_rel..cur_rel + block],
            &mut past_area[past_rel..past_rel + u_past_len],
            valid_frames,
        );
        j += 1;
    }
    let mut j = 0;
    while j < n_role {
        let cur_rel = v_cur_rel + j * block;
        let past_rel = v_past_off + j * v_past_len;
        update_history(
            &cur_area[cur_rel..cur_rel + block],
            &mut past_area[past_rel..past_rel + v_past_len],
            valid_frames,
        );
        j += 1;
    }

    RtStatus::OK
}

// ---------------------------------------------------------------------------
// x86_64：AVX2 + FMA（8 x f32 / 指令）
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod x86_avx2 {
    use super::{direct_block_simd_impl, FirRowFn};
    use crate::kernels::direct::{DirectCoeffsF32, DirectLayout, DirectTopology};
    use crate::kernels::RtStatus;
    use core::arch::x86_64::*;

    /// 单行 FIR 累积的 AVX2+FMA 实现（8-wide，时序方向读取）。
    ///
    /// # Safety
    ///
    /// 调用方必须已确认 CPU 支持 AVX2+FMA，且切片长度满足 [super::FirRowFn] 的契约
    /// （acc.len() >= valid、cur.len() >= valid、past.len() = l-1、valid <= 128、
    /// l >= 1 且 taps 为空或 taps.len() = l）。
    #[target_feature(enable = "avx2,fma")]
    unsafe fn fir_row_avx2(
        acc: &mut [f32],
        cur: &[f32],
        past: &[f32],
        taps: &[f32],
        delta_at: usize,
        valid: usize,
        l: usize,
    ) {
        let acc_ptr = acc.as_mut_ptr();
        let cur_ptr = cur.as_ptr();
        let past_ptr = past.as_ptr();
        let has_taps = !taps.is_empty();
        let t_begin = if has_taps || delta_at < l {
            if has_taps {
                0
            } else {
                delta_at
            }
        } else {
            l
        };
        let t_end = if has_taps {
            l
        } else if delta_at < l {
            delta_at + 1
        } else {
            l
        };
        let mut t = t_begin;
        while t < t_end {
            let mut coef = if has_taps {
                // SAFETY: 契约保证 t < l = taps.len()。
                unsafe { *taps.get_unchecked(t) }
            } else {
                1.0f32
            };
            if has_taps && t == delta_at {
                coef += 1.0;
            }
            if coef != 0.0 {
                let vcoef = _mm256_set1_ps(coef);
                // 头部：n in [0, min(t, valid))，源 = past[(l-1) - t + n]（n 升序连续）。
                let head = if t < valid { t } else { valid };
                let mut n = 0usize;
                while n + 8 <= head {
                    // SAFETY: head <= valid <= acc.len()，n+8 <= head；past 下标
                    // (l-1)-t+n <= (l-1)-t+head-1 <= l-2（t >= 1）；下界 >= 0（t <= l-1）。
                    unsafe {
                        let src = _mm256_loadu_ps(past_ptr.add(l - 1 - t + n));
                        let a = _mm256_loadu_ps(acc_ptr.add(n));
                        _mm256_storeu_ps(acc_ptr.add(n), _mm256_fmadd_ps(vcoef, src, a));
                    }
                    n += 8;
                }
                while n < head {
                    // SAFETY: n < head <= valid；past 下标同上界分析。
                    unsafe {
                        *acc_ptr.add(n) += coef * *past_ptr.add(l - 1 - t + n);
                    }
                    n += 1;
                }
                // 尾部：n in [head, valid)，源 = cur[n - t]（n 升序连续）。
                let mut n2 = head;
                while n2 + 8 <= valid {
                    // SAFETY: n2 >= t（head = min(t, valid)），cur 下标 n2-t >= 0 且
                    // n2+8 <= valid <= cur.len()；acc 下标 n2+8 <= valid <= acc.len()。
                    unsafe {
                        let src = _mm256_loadu_ps(cur_ptr.add(n2 - t));
                        let a = _mm256_loadu_ps(acc_ptr.add(n2));
                        _mm256_storeu_ps(acc_ptr.add(n2), _mm256_fmadd_ps(vcoef, src, a));
                    }
                    n2 += 8;
                }
                while n2 < valid {
                    // SAFETY: 同上一循环，逐样本版本。
                    unsafe {
                        *acc_ptr.add(n2) += coef * *cur_ptr.add(n2 - t);
                    }
                    n2 += 1;
                }
            }
            t += 1;
        }
    }

    /// AVX2+FMA 的 direct 入口（由 dispatch 在运行期探测通过后调用）。
    ///
    /// # Safety
    ///
    /// 调用方必须已确认 CPU 支持 AVX2 与 FMA。
    #[allow(clippy::too_many_arguments)]
    #[target_feature(enable = "avx2,fma")]
    pub(crate) unsafe fn direct_block_avx2(
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
        let row: FirRowFn = fir_row_avx2;
        direct_block_simd_impl(
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
            row,
        )
    }
}

// ---------------------------------------------------------------------------
// aarch64：NEON（4 x f32 / 指令；ARMv8 基线，无需运行期探测）
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod aarch64_neon {
    use super::{direct_block_simd_impl, FirRowFn};
    use crate::kernels::direct::{DirectCoeffsF32, DirectLayout, DirectTopology};
    use crate::kernels::RtStatus;
    use core::arch::aarch64::*;

    /// 单行 FIR 累积的 NEON 实现（4-wide，时序方向读取），语义同 AVX2 版本。
    ///
    /// aarch64 的 NEON 是 ARMv8-A 基线，目标特性默认开启，无需 target_feature 探测。
    ///
    /// # Safety
    ///
    /// 切片长度必须满足 [super::FirRowFn] 的契约（acc.len() >= valid、cur.len() >= valid、
    /// past.len() = l-1、valid <= 128、l >= 1 且 taps 为空或 taps.len() = l）。
    unsafe fn fir_row_neon(
        acc: &mut [f32],
        cur: &[f32],
        past: &[f32],
        taps: &[f32],
        delta_at: usize,
        valid: usize,
        l: usize,
    ) {
        let acc_ptr = acc.as_mut_ptr();
        let cur_ptr = cur.as_ptr();
        let past_ptr = past.as_ptr();
        let has_taps = !taps.is_empty();
        let t_begin = if has_taps {
            0
        } else if delta_at < l {
            delta_at
        } else {
            l
        };
        let t_end = if has_taps {
            l
        } else if delta_at < l {
            delta_at + 1
        } else {
            l
        };
        let mut t = t_begin;
        while t < t_end {
            let mut coef = if has_taps {
                // SAFETY: 契约保证 t < l = taps.len()。
                unsafe { *taps.get_unchecked(t) }
            } else {
                1.0f32
            };
            if has_taps && t == delta_at {
                coef += 1.0;
            }
            if coef != 0.0 {
                let vcoef = vdupq_n_f32(coef);
                let head = if t < valid { t } else { valid };
                let mut n = 0usize;
                while n + 4 <= head {
                    // SAFETY: 下标同 AVX2 版本的上界分析（head <= valid，past 下标 [0, l-2]）。
                    unsafe {
                        let src = vld1q_f32(past_ptr.add(l - 1 - t + n));
                        let a = vld1q_f32(acc_ptr.add(n));
                        vst1q_f32(acc_ptr.add(n), vfmaq_f32(a, vcoef, src));
                    }
                    n += 4;
                }
                while n < head {
                    // SAFETY: n < head <= valid；past 下标在 [0, l-2]。
                    unsafe {
                        *acc_ptr.add(n) += coef * *past_ptr.add(l - 1 - t + n);
                    }
                    n += 1;
                }
                let mut n2 = head;
                while n2 + 4 <= valid {
                    // SAFETY: n2 >= t，cur 下标 n2-t >= 0 且 n2+4 <= valid <= cur.len()。
                    unsafe {
                        let src = vld1q_f32(cur_ptr.add(n2 - t));
                        let a = vld1q_f32(acc_ptr.add(n2));
                        vst1q_f32(acc_ptr.add(n2), vfmaq_f32(a, vcoef, src));
                    }
                    n2 += 4;
                }
                while n2 < valid {
                    // SAFETY: 同上一循环，逐样本版本。
                    unsafe {
                        *acc_ptr.add(n2) += coef * *cur_ptr.add(n2 - t);
                    }
                    n2 += 1;
                }
            }
            t += 1;
        }
    }

    /// NEON 的 direct 入口（aarch64 上由 dispatch 直接调用；ARMv8 基线无需探测）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn direct_block_neon(
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
        let row: FirRowFn = fir_row_neon;
        direct_block_simd_impl(
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
            row,
        )
    }
}

// ---------------------------------------------------------------------------
// 本机派发入口（dispatch 在探测通过后调用；名字与签名平台无关）
// ---------------------------------------------------------------------------

/// 本机已确认指令集可用的 SIMD 入口（由 kernels::dispatch 调用）。
///
/// # Safety
///
/// x86_64：调用方必须已确认 AVX2 与 FMA 可用；aarch64：NEON 是基线，无额外要求。
///
/// 只覆盖 x86_64/aarch64 两种架构；其它架构不编译本模块（dispatch 直接回退标量）。
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(crate) unsafe fn direct_block_simd_dispatch(
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
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: 由调用方（dispatch）保证 AVX2+FMA 可用。
        unsafe {
            x86_avx2::direct_block_avx2(
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
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON 是 aarch64 基线。
        unsafe {
            aarch64_neon::direct_block_neon(
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
    }
}
