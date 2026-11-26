//! [RT] 共享 FFT / 分区卷积内核的显式 SIMD 实现（x86_64 AVX2+FMA / aarch64 NEON）。
//!
//! RT-MODULE: 本文件是 DSP 纯 worker 路径的 RT 模块：禁止析构/分配/锁/syscall/日志/panic；
//! 只用调用方预分配的 plan_data/state/input/output 切片与 std::arch 内建函数，不分配、
//! 不加锁、不做 IO、无显式 panic 分支。
//!
//! # 语义
//!
//! 与标量参考入口 [`crate::kernels::partitioned::partitioned_block_f32`] 完全同语义：
//! 同一套 [`crate::kernels::partitioned::PartitionedLayout`] state 布局（含环槽计数、
//! 终止性 DRAIN/reset_history）、同一套 partial 契约（V < B 是终止性 DRAIN：输出
//! [0,V) 正确、[V,B) 写 +0.0，处理完清历史）、同样的固定错误码与"校验失败前不写缓冲"。
//! 唯一差异是 FFT 蝶形 / 复数乘加 / 输出缩放的浮点舍入（FMA 融合），见"数值契约"。
//!
//! 与标量内核的 state 表示**完全相同**（x_prev / x_hist 环形槽 / work / slot 都是同一
//! 字节布局与同一语义），因此 SIMD/标量可以在同一 epoch 内交替调用、也可以把上一条流
//! 的 state 交给另一条路径继续处理（输出仍按容差一致）。direct SIMD 内核则使用镜像历史
//! 表示，两者不可混用（见 direct_simd 模块文档）。
//!
//! # SIMD 加速点（radix-2 DIT FFT；K = next_pow2(2B) <= 256）
//!
//! 1. 蝶形：half >= 8（AVX2）/ >= 4（NEON）的阶段按 8/4 个连续 t 做向量蝶形；
//!    half 更小的前几个阶段保持标量（twiddle 是 1/-j 等平凡值，量也很小）。
//!    向量化阶段需要 t*step 的 twiddle：step == 1（末级）直接连续加载，其它 stage 在
//!    AVX2 上用 vgatherdps 按索引取，NEON 上退化为 4 个标量装载后 vld1q。
//! 2. 频域复数乘加 Y += X_hist * H：deinterleaved 实/虚平面，8/4 路 FMA 一次算 8/4 个复数。
//! 3. 输出缩放与共轭取反：向量乘 / 向量取反。
//!
//! # 数值契约
//!
//! 标量参考的 FFT/复数乘加是非融合 mul+add/sub，SIMD 路径用 FMA 且蝶形部分重排；因此
//! 与标量参考**非逐位一致**。实测（tests/kernels_simd.rs）：random 输入下 f32 每块相对
//! L2 误差 <= 1e-5、单样本绝对误差 <= 1e-4（4096-tap 分区、随机输入；与 f64 oracle 的
//! 误差门沿用 T08 的 f32 相对 L2 <= 1e-5 口径）。twiddle/滤波器频谱仍在 NONRT 由标量
//! 构建（build_f32 不变），SIMD 只影响 RT 内的正/逆变换与累加。

use crate::kernels::direct::DirectTopology;
use crate::kernels::partitioned::{validate_call, PartitionedPlan};
use crate::kernels::RtStatus;

/// 正向 FFT 的函数指针类型（架构相关）：buf 内前 k 实部后 k 虚部（可用 re_off/im_off
/// 指到同一缓冲的 work 区），twiddle 表长度 k/2（re、im 分开）。
type FftFn = unsafe fn(&mut [f32], usize, usize, usize, &[f32], &[f32]);

/// 频域复数乘加的函数指针类型（架构相关）：work[acc_re..] / work[acc_im..] +=
/// hist[x_re..] * spec[h_re..] 的复数乘法（长度 k）。
type ComplexMacFn =
    unsafe fn(&mut [f32], usize, usize, &[f32], usize, usize, &[f32], usize, usize, usize);

/// 输出缩放的函数指针类型（架构相关）：dst[n] = src[n] * scale。
type ScaleCopyFn = unsafe fn(&mut [f32], &[f32], f32);

/// 取反的函数指针类型（架构相关）：buf[off + n] = -buf[off + n]（n < k）。
type NegateFn = unsafe fn(&mut [f32], usize, usize);

/// 一台机器上选中的 SIMD 原语集合（由派发层按探测结果构造）。
struct SimdOps {
    /// 正向 radix-2 FFT。
    fft: FftFn,
    /// 频域复数乘加。
    complex_mac: ComplexMacFn,
    /// 输出缩放。
    scale_copy: ScaleCopyFn,
    /// 共轭（虚部取反）。
    negate: NegateFn,
}

/// SIMD 入口的公共编排（架构无关；SIMD 算术全部在 ops 的函数指针里）。
#[allow(clippy::too_many_arguments)]
fn partitioned_block_simd_impl(
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
    ops: &SimdOps,
) -> RtStatus {
    let status = validate_call(
        topo,
        plan,
        plan_data.len(),
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

    let layout = plan.layout;
    let n_src = layout.n_src as usize;
    let n_role = layout.n_role as usize;
    let containers = layout.container_channels as usize;
    let b = layout.block_size as usize;
    let k = layout.fft_size as usize;
    let half = k / 2;
    let q = layout.partitions as usize;
    let n_routes = layout.n_routes as usize;
    let x_prev_off = layout.x_prev_off as usize;
    let x_hist_off = layout.x_hist_off as usize;
    let work_off = layout.work_off as usize;
    let slot_off = layout.slot_off as usize;

    // twiddle 表位于 plan_data 前 K 个（K/2 re + K/2 im）。
    let (tw_re, tw_im) = plan_data[..k].split_at(half);
    let inv_k = 1.0f32 / k as f32;

    // 输出块先整体清零：未映射容器通道 / valid_frames 之后的样本必须逐样本严格 +0.0。
    let mut c = 0;
    while c < containers {
        let base = c * out_stride;
        output[base..base + block_size].fill(0.0);
        c += 1;
    }
    // valid_frames < B（含 V=0）统一为终止性 DRAIN：输出已清零；V=0 不再做 FFT/卷积，
    // 直接清历史（x_prev + x_hist）并置 slot=0，与 0<V<B 完全对称（新 epoch 起点）。
    if valid_frames == 0 {
        state[..work_off].fill(0.0);
        state[slot_off] = 0.0;
        return RtStatus::OK;
    }

    // 历史区在 work 区之前：一次 split 后 hist 只读、work 可写，互不干扰。
    let (hist, work) = state.split_at_mut(work_off);
    let work_re = 0usize;
    let work_im = k;
    let acc_re = 2 * k;
    let acc_im = 3 * k;
    let slot_rel = 4 * k;

    // 当前历史写入槽（0..Q-1；标量保存，分块足够小使 f32 精确）。
    let slot = work[slot_rel] as usize;
    // 防御：slot 越界（state 损坏）按 epoch 起点处理，不 panic（与标量参考一致）。
    let slot = if slot < q { slot } else { 0 };

    // 阶段 1：每源构造窗口 W = [x_prev(B), 当前块(B) 有效样本 + 零] 并 FFT，写入共享历史槽。
    let mut i = 0usize;
    while i < n_src {
        work[work_im..work_im + k].fill(0.0);
        let prev = x_prev_off + i * b;
        work[work_re..work_re + b].copy_from_slice(&hist[prev..prev + b]);
        // 当前块实部：先清零 [b, k)（覆盖 cur 的无效样本与 [2B,K) 零填充），再写有效样本。
        work[work_re + b..work_re + k].fill(0.0);
        let in_base = i * in_stride;
        work[work_re + b..work_re + b + valid_frames]
            .copy_from_slice(&input[in_base..in_base + valid_frames]);
        // SAFETY: fft 是本机派发选中的 SIMD 实现；work 长度 >= 4K，re/im 区 [0,K)/[K,2K)
        // 与 twiddle 表（[0,K/2)）都在校验过的容量内。
        unsafe {
            (ops.fft)(&mut work[..2 * k], work_re, work_im, k, tw_re, tw_im);
        }
        let hist_base = x_hist_off + (i * q + slot) * 2 * k;
        hist[hist_base..hist_base + 2 * k].copy_from_slice(&work[..2 * k]);
        i += 1;
    }

    // 阶段 2：每输出角色按固定路由顺序累加频域 Y_j = sum_routes sum_p X_hist[b-p] * H_r[p]，
    // 然后 IFFT（共轭技巧）+ overlap-save 取后 B 样本，写 scatter 容器通道。
    let mut j = 0usize;
    while j < n_role {
        work[acc_re..acc_re + 2 * k].fill(0.0);
        let mut r = 0usize;
        while r < n_routes {
            if plan.route_out[r] as usize == j {
                let src = plan.route_src[r] as usize;
                let hist_base = x_hist_off + src * q * 2 * k;
                let spec_base = k + r * q * 2 * k;
                let mut part = 0usize;
                while part < q {
                    let hist_slot = (slot + q - part) % q;
                    let x_off = hist_base + hist_slot * 2 * k;
                    let h_off = spec_base + part * 2 * k;
                    // SAFETY: complex_mac 是本机派发选中的 SIMD 实现；work 的 acc 区
                    // [2K,4K)、hist 的槽区、plan_data 的频谱区都在校验过的容量内。
                    unsafe {
                        (ops.complex_mac)(
                            work,
                            acc_re,
                            acc_im,
                            hist,
                            x_off,
                            x_off + k,
                            plan_data,
                            h_off,
                            h_off + k,
                            k,
                        );
                    }
                    part += 1;
                }
            }
            r += 1;
        }
        // Re(IFFT(Y))[n] = Re(FFT(conj Y))[n] / K：共轭后做一次正向 FFT，取实部 / K。
        // SAFETY: 同阶段 1；work 长度 >= 4K。
        unsafe {
            (ops.negate)(work, acc_im, k);
            (ops.fft)(&mut work[..4 * k], acc_re, acc_im, k, tw_re, tw_im);
            let scatter = topo.scatter[j] as usize;
            let out_base = scatter * out_stride;
            (ops.scale_copy)(
                &mut output[out_base..out_base + valid_frames],
                &work[acc_re + b..acc_re + b + valid_frames],
                inv_k,
            );
        }
        j += 1;
    }

    if valid_frames == b {
        // 阶段 3：历史搬运（每 block 一次 O(n_src*B)，不做逐样本移位）并推进环槽。
        let mut i2 = 0usize;
        while i2 < n_src {
            let prev = x_prev_off + i2 * b;
            let in_base = i2 * in_stride;
            hist[prev..prev + b].copy_from_slice(&input[in_base..in_base + b]);
            i2 += 1;
        }
        let next = (slot + 1) % q;
        work[slot_rel] = next as f32;
    } else {
        // 终止性 DRAIN（0 <= V < B）：清 x_prev/x_hist 并置 slot=0，后续调用从新 epoch 起点开始。
        hist.fill(0.0);
        work[slot_rel] = 0.0;
    }
    RtStatus::OK
}

// ---------------------------------------------------------------------------
// x86_64：AVX2 + FMA（8 x f32 / 指令）
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod x86_avx2 {
    use super::*;
    use core::arch::x86_64::*;

    /// radix-2 DIT 正向 FFT（AVX2+FMA）：buf 内前 k 实部、后 k 虚部（re_off/im_off 偏移）。
    ///
    /// # Safety
    ///
    /// 调用方必须已确认 AVX2+FMA 可用；k 是 2 的幂且 k >= 2；buf.len() >= max(re_off, im_off)
    /// + k；tw_re/tw_im 长度 >= k/2。
    #[target_feature(enable = "avx2,fma")]
    unsafe fn fft_forward_avx2(
        buf: &mut [f32],
        re_off: usize,
        im_off: usize,
        k: usize,
        tw_re: &[f32],
        tw_im: &[f32],
    ) {
        let base = buf.as_mut_ptr();
        let tw_re_ptr = tw_re.as_ptr();
        let tw_im_ptr = tw_im.as_ptr();
        // 位反序置换（标量；K <= 256，代价远小于蝶形）。
        let mut j = 0usize;
        let mut i = 1usize;
        while i < k {
            let mut bit = k >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j ^= bit;
            if i < j {
                unsafe {
                    let a = re_off + i;
                    let b = re_off + j;
                    let tmp = *base.add(a);
                    *base.add(a) = *base.add(b);
                    *base.add(b) = tmp;
                    let a2 = im_off + i;
                    let b2 = im_off + j;
                    let tmp2 = *base.add(a2);
                    *base.add(a2) = *base.add(b2);
                    *base.add(b2) = tmp2;
                }
            }
            i += 1;
        }
        // 迭代 Cooley-Tukey（DIT）。
        let mut len = 2usize;
        while len <= k {
            let half = len >> 1;
            let step = k / len;
            let mut start = 0usize;
            while start < k {
                if half >= 8 {
                    let mut idx = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
                    if step != 1 {
                        idx = _mm256_mullo_epi32(idx, _mm256_set1_epi32(step as i32));
                    }
                    let idx_step = _mm256_set1_epi32((8 * step) as i32);
                    let mut t = 0usize;
                    while t + 8 <= half {
                        // SAFETY: t + 8 <= half <- len/2；tw 下标 t*step + 7*step < k/2；
                        // 数据下标 start+t+half < start+len <= k。
                        unsafe {
                            let (wr, wi) = if step == 1 {
                                (
                                    _mm256_loadu_ps(tw_re_ptr.add(t)),
                                    _mm256_loadu_ps(tw_im_ptr.add(t)),
                                )
                            } else {
                                (
                                    _mm256_i32gather_ps(tw_re_ptr, idx, 4),
                                    _mm256_i32gather_ps(tw_im_ptr, idx, 4),
                                )
                            };
                            let a = start + t;
                            let bb = a + half;
                            let ar = _mm256_loadu_ps(base.add(re_off + a));
                            let ai = _mm256_loadu_ps(base.add(im_off + a));
                            let br = _mm256_loadu_ps(base.add(re_off + bb));
                            let bi = _mm256_loadu_ps(base.add(im_off + bb));
                            let tr = _mm256_fmsub_ps(br, wr, _mm256_mul_ps(bi, wi));
                            let ti = _mm256_fmadd_ps(br, wi, _mm256_mul_ps(bi, wr));
                            _mm256_storeu_ps(base.add(re_off + a), _mm256_add_ps(ar, tr));
                            _mm256_storeu_ps(base.add(im_off + a), _mm256_add_ps(ai, ti));
                            _mm256_storeu_ps(base.add(re_off + bb), _mm256_sub_ps(ar, tr));
                            _mm256_storeu_ps(base.add(im_off + bb), _mm256_sub_ps(ai, ti));
                        }
                        idx = _mm256_add_epi32(idx, idx_step);
                        t += 8;
                    }
                    while t < half {
                        // SAFETY: 同上一循环的边界分析（逐元素版本）。
                        unsafe {
                            scalar_butterfly_avx2(
                                base, re_off, im_off, start, t, half, step, tw_re_ptr, tw_im_ptr,
                            );
                        }
                        t += 1;
                    }
                } else {
                    let mut t = 0usize;
                    while t < half {
                        // SAFETY: 同上；标量前几级 twiddle 是 1/-j 等平凡值。
                        unsafe {
                            scalar_butterfly_avx2(
                                base, re_off, im_off, start, t, half, step, tw_re_ptr, tw_im_ptr,
                            );
                        }
                        t += 1;
                    }
                }
                start += len;
            }
            len <<= 1;
        }
    }

    /// 单个标量蝶形（FFT 前几级/余数用）。
    ///
    /// # Safety
    ///
    /// 索引均在 buf 与 twiddle 表范围内（由调用方的 FFT 契约保证）。
    #[allow(clippy::too_many_arguments)]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn scalar_butterfly_avx2(
        base: *mut f32,
        re_off: usize,
        im_off: usize,
        start: usize,
        t: usize,
        half: usize,
        step: usize,
        tw_re: *const f32,
        tw_im: *const f32,
    ) {
        unsafe {
            let tw = t * step;
            let wr = *tw_re.add(tw);
            let wi = *tw_im.add(tw);
            let a = start + t;
            let b = a + half;
            let br = *base.add(re_off + b);
            let bi = *base.add(im_off + b);
            let tr = br * wr - bi * wi;
            let ti = br * wi + bi * wr;
            let ar = *base.add(re_off + a);
            let ai = *base.add(im_off + a);
            *base.add(re_off + b) = ar - tr;
            *base.add(im_off + b) = ai - ti;
            *base.add(re_off + a) = ar + tr;
            *base.add(im_off + a) = ai + ti;
        }
    }

    /// 频域复数乘加（AVX2+FMA）：work 的 acc 实/虚部 += hist 复频谱 * spec 复频谱。
    ///
    /// # Safety
    ///
    /// 已确认 AVX2+FMA；所有下标区间的长度 k 都在各切片容量内（调用方已校验布局）。
    #[allow(clippy::too_many_arguments)]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn complex_mac_avx2(
        work: &mut [f32],
        acc_re: usize,
        acc_im: usize,
        hist: &[f32],
        x_re: usize,
        x_im: usize,
        spec: &[f32],
        h_re: usize,
        h_im: usize,
        k: usize,
    ) {
        let work_ptr = work.as_mut_ptr();
        let hist_ptr = hist.as_ptr();
        let spec_ptr = spec.as_ptr();
        let mut n = 0usize;
        while n + 8 <= k {
            // SAFETY: n + 8 <= k，acc 区 [2K,4K)、hist/spec 的槽区都按布局容量分配。
            unsafe {
                let xr = _mm256_loadu_ps(hist_ptr.add(x_re + n));
                let xi = _mm256_loadu_ps(hist_ptr.add(x_im + n));
                let hr = _mm256_loadu_ps(spec_ptr.add(h_re + n));
                let hi = _mm256_loadu_ps(spec_ptr.add(h_im + n));
                let ar = _mm256_loadu_ps(work_ptr.add(acc_re + n));
                let ai = _mm256_loadu_ps(work_ptr.add(acc_im + n));
                let nr = _mm256_fnmadd_ps(xi, hi, _mm256_fmadd_ps(xr, hr, ar));
                let ni = _mm256_fmadd_ps(xi, hr, _mm256_fmadd_ps(xr, hi, ai));
                _mm256_storeu_ps(work_ptr.add(acc_re + n), nr);
                _mm256_storeu_ps(work_ptr.add(acc_im + n), ni);
            }
            n += 8;
        }
        while n < k {
            // SAFETY: n < k，其余同上一循环。
            unsafe {
                let xr = *hist_ptr.add(x_re + n);
                let xi = *hist_ptr.add(x_im + n);
                let hr = *spec_ptr.add(h_re + n);
                let hi = *spec_ptr.add(h_im + n);
                let ar = *work_ptr.add(acc_re + n);
                let ai = *work_ptr.add(acc_im + n);
                *work_ptr.add(acc_re + n) = ar + xr * hr - xi * hi;
                *work_ptr.add(acc_im + n) = ai + xr * hi + xi * hr;
            }
            n += 1;
        }
    }

    /// dst[n] = src[n] * scale（长度为 dst.len()，要求 dst.len() == src.len()）。
    ///
    /// # Safety
    ///
    /// 已确认 AVX2+FMA；dst.len() == src.len() 且都在调用方缓冲内。
    #[target_feature(enable = "avx2,fma")]
    unsafe fn scale_copy_avx2(dst: &mut [f32], src: &[f32], scale: f32) {
        let len = dst.len();
        let dst_ptr = dst.as_mut_ptr();
        let src_ptr = src.as_ptr();
        let vscale = _mm256_set1_ps(scale);
        let mut n = 0usize;
        while n + 8 <= len {
            // SAFETY: n + 8 <= len = dst.len() = src.len()。
            unsafe {
                let v = _mm256_loadu_ps(src_ptr.add(n));
                _mm256_storeu_ps(dst_ptr.add(n), _mm256_mul_ps(v, vscale));
            }
            n += 8;
        }
        while n < len {
            // SAFETY: n < len。
            unsafe {
                *dst_ptr.add(n) = *src_ptr.add(n) * scale;
            }
            n += 1;
        }
    }

    /// buf[off + n] = -buf[off + n]（n < k；0.0 - v，与标量的 ZERO.sub(v) 同号零行为）。
    ///
    /// # Safety
    ///
    /// 已确认 AVX2+FMA；off + k <= buf.len()。
    #[target_feature(enable = "avx2,fma")]
    unsafe fn negate_avx2(buf: &mut [f32], off: usize, k: usize) {
        let ptr = buf.as_mut_ptr();
        let zero = _mm256_setzero_ps();
        let mut n = 0usize;
        while n + 8 <= k {
            // SAFETY: n + 8 <= k，off + k <= buf.len()。
            unsafe {
                let v = _mm256_loadu_ps(ptr.add(off + n));
                _mm256_storeu_ps(ptr.add(off + n), _mm256_sub_ps(zero, v));
            }
            n += 8;
        }
        while n < k {
            // SAFETY: n < k。
            unsafe {
                *ptr.add(off + n) = 0.0 - *ptr.add(off + n);
            }
            n += 1;
        }
    }

    /// AVX2+FMA 的分区卷积入口（由 dispatch 在运行期探测通过后调用）。
    ///
    /// # Safety
    ///
    /// 调用方必须已确认 CPU 支持 AVX2 与 FMA。
    #[allow(clippy::too_many_arguments)]
    #[target_feature(enable = "avx2,fma")]
    pub(crate) unsafe fn partitioned_block_avx2(
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
        let ops = SimdOps {
            fft: fft_forward_avx2,
            complex_mac: complex_mac_avx2,
            scale_copy: scale_copy_avx2,
            negate: negate_avx2,
        };
        partitioned_block_simd_impl(
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
            &ops,
        )
    }
}

// ---------------------------------------------------------------------------
// aarch64：NEON（4 x f32 / 指令；ARMv8 基线，无需运行期探测）
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod aarch64_neon {
    use super::*;
    use core::arch::aarch64::*;

    /// radix-2 DIT 正向 FFT（NEON），语义同 AVX2 版本。
    ///
    /// # Safety
    ///
    /// k 是 2 的幂且 k >= 2；buf.len() >= max(re_off, im_off) + k；tw_re/tw_im 长度 >= k/2。
    unsafe fn fft_forward_neon(
        buf: &mut [f32],
        re_off: usize,
        im_off: usize,
        k: usize,
        tw_re: &[f32],
        tw_im: &[f32],
    ) {
        let base = buf.as_mut_ptr();
        let tw_re_ptr = tw_re.as_ptr();
        let tw_im_ptr = tw_im.as_ptr();
        let mut j = 0usize;
        let mut i = 1usize;
        while i < k {
            let mut bit = k >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j ^= bit;
            if i < j {
                unsafe {
                    let a = re_off + i;
                    let b = re_off + j;
                    let tmp = *base.add(a);
                    *base.add(a) = *base.add(b);
                    *base.add(b) = tmp;
                    let a2 = im_off + i;
                    let b2 = im_off + j;
                    let tmp2 = *base.add(a2);
                    *base.add(a2) = *base.add(b2);
                    *base.add(b2) = tmp2;
                }
            }
            i += 1;
        }
        let mut len = 2usize;
        while len <= k {
            let half = len >> 1;
            let step = k / len;
            let mut start = 0usize;
            while start < k {
                if half >= 4 {
                    let mut t = 0usize;
                    while t + 4 <= half {
                        // SAFETY: t + 4 <= half；tw 下标 (t+3)*step < k/2；数据下标 < k。
                        unsafe {
                            let wr = if step == 1 {
                                vld1q_f32(tw_re_ptr.add(t))
                            } else {
                                let mut tmp = [0.0f32; 4];
                                let mut m = 0usize;
                                while m < 4 {
                                    tmp[m] = *tw_re_ptr.add((t + m) * step);
                                    m += 1;
                                }
                                vld1q_f32(tmp.as_ptr())
                            };
                            let wi = if step == 1 {
                                vld1q_f32(tw_im_ptr.add(t))
                            } else {
                                let mut tmp = [0.0f32; 4];
                                let mut m = 0usize;
                                while m < 4 {
                                    tmp[m] = *tw_im_ptr.add((t + m) * step);
                                    m += 1;
                                }
                                vld1q_f32(tmp.as_ptr())
                            };
                            let a = start + t;
                            let bb = a + half;
                            let ar = vld1q_f32(base.add(re_off + a));
                            let ai = vld1q_f32(base.add(im_off + a));
                            let br = vld1q_f32(base.add(re_off + bb));
                            let bi = vld1q_f32(base.add(im_off + bb));
                            let tr = vfmsq_f32(vmulq_f32(br, wr), bi, wi);
                            let ti = vfmaq_f32(vmulq_f32(br, wi), bi, wr);
                            vst1q_f32(base.add(re_off + a), vaddq_f32(ar, tr));
                            vst1q_f32(base.add(im_off + a), vaddq_f32(ai, ti));
                            vst1q_f32(base.add(re_off + bb), vsubq_f32(ar, tr));
                            vst1q_f32(base.add(im_off + bb), vsubq_f32(ai, ti));
                        }
                        t += 4;
                    }
                    while t < half {
                        // SAFETY: 同上一循环的边界分析（逐元素版本）。
                        unsafe {
                            scalar_butterfly_neon(
                                base, re_off, im_off, start, t, half, step, tw_re_ptr, tw_im_ptr,
                            );
                        }
                        t += 1;
                    }
                } else {
                    let mut t = 0usize;
                    while t < half {
                        // SAFETY: 同上。
                        unsafe {
                            scalar_butterfly_neon(
                                base, re_off, im_off, start, t, half, step, tw_re_ptr, tw_im_ptr,
                            );
                        }
                        t += 1;
                    }
                }
                start += len;
            }
            len <<= 1;
        }
    }

    /// 单个标量蝶形。
    ///
    /// # Safety
    ///
    /// 索引均在 buf 与 twiddle 表范围内。
    #[allow(clippy::too_many_arguments)]
    unsafe fn scalar_butterfly_neon(
        base: *mut f32,
        re_off: usize,
        im_off: usize,
        start: usize,
        t: usize,
        half: usize,
        step: usize,
        tw_re: *const f32,
        tw_im: *const f32,
    ) {
        unsafe {
            let tw = t * step;
            let wr = *tw_re.add(tw);
            let wi = *tw_im.add(tw);
            let a = start + t;
            let b = a + half;
            let br = *base.add(re_off + b);
            let bi = *base.add(im_off + b);
            let tr = br * wr - bi * wi;
            let ti = br * wi + bi * wr;
            let ar = *base.add(re_off + a);
            let ai = *base.add(im_off + a);
            *base.add(re_off + b) = ar - tr;
            *base.add(im_off + b) = ai - ti;
            *base.add(re_off + a) = ar + tr;
            *base.add(im_off + a) = ai + ti;
        }
    }

    /// 频域复数乘加（NEON）。
    ///
    /// # Safety
    ///
    /// 所有下标区间的长度 k 都在各切片容量内（调用方已校验布局）。
    #[allow(clippy::too_many_arguments)]
    unsafe fn complex_mac_neon(
        work: &mut [f32],
        acc_re: usize,
        acc_im: usize,
        hist: &[f32],
        x_re: usize,
        x_im: usize,
        spec: &[f32],
        h_re: usize,
        h_im: usize,
        k: usize,
    ) {
        let work_ptr = work.as_mut_ptr();
        let hist_ptr = hist.as_ptr();
        let spec_ptr = spec.as_ptr();
        let mut n = 0usize;
        while n + 4 <= k {
            // SAFETY: n + 4 <= k；各区间都在按布局分配的容量内。
            unsafe {
                let xr = vld1q_f32(hist_ptr.add(x_re + n));
                let xi = vld1q_f32(hist_ptr.add(x_im + n));
                let hr = vld1q_f32(spec_ptr.add(h_re + n));
                let hi = vld1q_f32(spec_ptr.add(h_im + n));
                let ar = vld1q_f32(work_ptr.add(acc_re + n));
                let ai = vld1q_f32(work_ptr.add(acc_im + n));
                let nr = vfmsq_f32(vfmaq_f32(ar, xr, hr), xi, hi);
                let ni = vfmaq_f32(vfmaq_f32(ai, xr, hi), xi, hr);
                vst1q_f32(work_ptr.add(acc_re + n), nr);
                vst1q_f32(work_ptr.add(acc_im + n), ni);
            }
            n += 4;
        }
        while n < k {
            // SAFETY: n < k。
            unsafe {
                let xr = *hist_ptr.add(x_re + n);
                let xi = *hist_ptr.add(x_im + n);
                let hr = *spec_ptr.add(h_re + n);
                let hi = *spec_ptr.add(h_im + n);
                let ar = *work_ptr.add(acc_re + n);
                let ai = *work_ptr.add(acc_im + n);
                *work_ptr.add(acc_re + n) = ar + xr * hr - xi * hi;
                *work_ptr.add(acc_im + n) = ai + xr * hi + xi * hr;
            }
            n += 1;
        }
    }

    /// dst[n] = src[n] * scale（NEON）。
    ///
    /// # Safety
    ///
    /// dst.len() == src.len() 且都在调用方缓冲内。
    unsafe fn scale_copy_neon(dst: &mut [f32], src: &[f32], scale: f32) {
        let len = dst.len();
        let dst_ptr = dst.as_mut_ptr();
        let src_ptr = src.as_ptr();
        let vscale = vdupq_n_f32(scale);
        let mut n = 0usize;
        while n + 4 <= len {
            // SAFETY: n + 4 <= len = dst.len() = src.len()。
            unsafe {
                let v = vld1q_f32(src_ptr.add(n));
                vst1q_f32(dst_ptr.add(n), vmulq_f32(v, vscale));
            }
            n += 4;
        }
        while n < len {
            // SAFETY: n < len。
            unsafe {
                *dst_ptr.add(n) = *src_ptr.add(n) * scale;
            }
            n += 1;
        }
    }

    /// buf[off + n] = 0.0 - buf[off + n]（NEON）。
    ///
    /// # Safety
    ///
    /// off + k <= buf.len()。
    unsafe fn negate_neon(buf: &mut [f32], off: usize, k: usize) {
        let ptr = buf.as_mut_ptr();
        let zero = vdupq_n_f32(0.0);
        let mut n = 0usize;
        while n + 4 <= k {
            // SAFETY: n + 4 <= k，off + k <= buf.len()。
            unsafe {
                let v = vld1q_f32(ptr.add(off + n));
                vst1q_f32(ptr.add(off + n), vsubq_f32(zero, v));
            }
            n += 4;
        }
        while n < k {
            // SAFETY: n < k。
            unsafe {
                *ptr.add(off + n) = 0.0 - *ptr.add(off + n);
            }
            n += 1;
        }
    }

    /// NEON 的分区卷积入口（aarch64 上由 dispatch 直接调用）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn partitioned_block_neon(
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
        let ops = SimdOps {
            fft: fft_forward_neon,
            complex_mac: complex_mac_neon,
            scale_copy: scale_copy_neon,
            negate: negate_neon,
        };
        partitioned_block_simd_impl(
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
            &ops,
        )
    }
}

// ---------------------------------------------------------------------------
// 本机派发入口（dispatch 在探测通过后调用；名字与签名平台无关）
// ---------------------------------------------------------------------------

/// 本机已确认指令集可用的分区卷积 SIMD 入口（由 kernels::dispatch 调用）。
///
/// # Safety
///
/// x86_64：调用方必须已确认 AVX2 与 FMA 可用；aarch64：NEON 是基线，无额外要求。
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(crate) unsafe fn partitioned_block_simd_dispatch(
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
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: 由调用方（dispatch）保证 AVX2+FMA 可用。
        unsafe {
            x86_avx2::partitioned_block_avx2(
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
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON 是 aarch64 基线。
        unsafe {
            aarch64_neon::partitioned_block_neon(
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
    }
}
