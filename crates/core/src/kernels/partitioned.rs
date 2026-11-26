//! [RT] 共享 FFT / 分区 b-p 历史卷积内核（legacy/pre-rewrite 的 T08 迁移；写域
//! `crates/core/src/kernels/partitioned.rs`）。
//!
//! RT-MODULE: DSP 纯 worker 路径禁止析构/分配/锁/syscall/日志/panic；FFT plan/scratch 必须
//! 在 NONRT 预建。本文件 [RT] 入口只做固定长度的切片读写与标量运算，计算复杂度
//! O(routes x partitions x FFT)，全部使用调用方预分配的 plan arena 与 state。
//!
//! 实现旧 reference 模型的共享 FFT 均匀分区卷积（uniform partitioned overlap-save，UPOLS）：
//!
//! ~~~text
//! u_k[n] = (a_k * x_k)[n]                            （输入列 FIR；A_map 只选择/零填）
//! v_j    = sum_k G_full[j][k] * u_k                  （G = delta_D I + g；低带矩阵嵌入）
//! y_j    = p_j * v_j                                 （行对角 FIR）
//! z[scatter[j]] = y_j；未使用容器通道逐样本 +0.0
//! ~~~
//!
//! 编译期（[NONRT] plan 构建）把每条（输出角色 j, 源通道 i）路由融合为一条完整 FIR
//! h(j,i) = p_j * G_full[j][k] * a_k（k = 选择源 i 的角色；fused 等价），长度
//! L_F = L_A + L_G + L_P - 2（D 计入 L_G）。运行时按 B 样本分块做 UPOLS：
//!
//! ~~~text
//! 窗口 W[b] = [上一块 x（B 样本）, 当前块 x（B 样本）]        长度 K = 2B 补零到 next_pow2
//! X[b]      = FFT_K(W[b])                                   （每源每块只算一次）
//! X_hist[b-p]                                               （共享 FFT 历史 b-p，p=0..Q-1）
//! Y_j[b]    = sum_(路由 r=(j,i)) sum_(p<Q) X_hist_i[b-p] * H_r[p]
//! y_j[b]    = IFFT_K(Y_j[b])[B..2B)                          （overlap-save 丢弃前 B 样本）
//! ~~~
//!
//! 其中 h 的每个分区 h_r[p] 为 B 个 tap（最后一个分区短尾补零），H_r[p] = FFT_K(h_r[p]
//! 零填充到 K)。K = next_pow2(2B)（B 为 2 的幂时 K = 2B；B 非 2 的幂时窗口 2B 与分区 B 都补零
//! 到更大的 K，如 B=3→8、B=100→256）。由于 K >= 2B 且每个分区长度 <= B，圆周卷积的卷绕只
//! 落在被丢弃的前 B 样本（输出有效区 [B,2B) 不受卷绕影响），因此频谱相乘等价于线性卷积。
//! 每个输入通道的 X[b-p] 被所有输出行/路由共享（共享 FFT 历史），不重复 FFT。
//!
// NOTE(legacy-defect): 旧 `rt.rs::fft_size(B)` 返回 2*B（注释写“K = 2 x B”），而本内核实际
// 用 K = next_pow2(2B)；B 非 2 的幂时两者不一致（如 B=3：旧 helper 给 6，本内核要 8）。
// 旧实现生产只准入 B∈{64,128}（两者相等），故未触发；但任何用旧 helper 给本内核预分配
// FFT 缓冲的调用方都会少分配。迁移保留内核行为（next_pow2），不复用该 helper，并在报告登记。
//!
//! 流式语义：输入尾部补 flush = L_F - 1 个零，再补零到 B 的整数倍；每块输出 B 个样本，
//! 拼接后截断到 L_out = L_in + L_F - 1；结果与离线线性卷积一致。本模块不做周期卷积、
//! 不跨块丢样、不做隐式 dither/clip/归一化。
//!
//! 精度分层：`partitioned_block_f32` 是 [RT] 生产入口；`partitioned_block_f64` 是
//! [TEST]/offline oracle（不进入生产 RT 调用图）。FFT 引入无理旋转因子，故与精确 MAC 的
//! 直算内核比较只能按容差（旧 T08 工程门：f64 相对 L2 <= 1e-12、f32 <= 1e-5）。
//!
//! 预分配契约：[NONRT] `PartitionedLayout::build` 给出 `plan_samples` / `state_samples` /
//! `build_scratch_samples` 三个容量；调用方提供固定缓冲后由 `PartitionedPlan::build_f32` /
//! `build_f64` 填充（twiddle 表 + 各路由各分区的频谱）。[RT] 入口只借用这些缓冲，不分配、
//! 不加锁、不 syscall、无析构、无显式 panic 分支。
//!
//! 与 plan/ABI 的边界：本模块消费 reference 语义图（`DirectTopology` 的 A_map/a/g/p/scatter
//! 与 D）。方案 v3 到语义图/plan arena 的适配属 host/compile 层，内核不解析方案文件。
//!
//! 支持的 block size：1..=128 的任意整数（radix-2 FFT，K = next_pow2(2B)，见上）。生产准入
//! 仍是旧 SPEC 的 B 属于 {64,128}；B=0 或 >128 返回 ERR_BLOCK_SIZE。非 2 的幂只付出更大的 K
//! （plan/state 按 K 线性放大），仅供 [TEST]/reference 对拍。
//!
//! 统一 partial/flush 契约（与 `direct` 的说明对齐）：
//! - flush 长度 = L_F - 1 个零，随后补零到 B 的整数倍；两块内核同一口径。
//! - `valid_frames < B`：DSP 把 `[V,B)` 视作 0 输入；输出 `[0,V)` 有效、`[V,B)` 写 +0.0。
//! - **partitioned 的 partial 块是终止性的**：UPOLS 的历史按 B 对齐，V<B 后无法在同一 epoch
//!   继续，故处理完（含 V=0）清零 x_prev/x_hist 并置 slot=0（等价新 epoch）。V=0 与 V>0 对称，
//!   都是“零/少样本 drain”，不是 idle no-op；调用方不得把 V=0 当空转块。
//! - direct 的 partial 块按 valid_frames 推进历史（可拼接 partial，属 direct 扩展语义）；
//!   连续流必须用 full B 块，partial 只在流末使用，之后需新 epoch/清零 state。
//!
//! 错误码（旧 leaf 约定，数值见 `crate::kernels::RtStatus`）：容量/通道 -> ERR_CHANNELS；
//! 角色/映射/scatter -> ERR_ROLES；taps/D/分区数 -> ERR_TAPS；系数长度/路由表 -> ERR_PATH_TABLE；
//! plan/state/输入/输出缓冲不足 -> ERR_CAPACITY；block 大小 -> ERR_BLOCK_SIZE；stride ->
//! ERR_STRIDE_ALIGN；plan/layout/valid_frames 不符 -> ERR_BAD_LAYOUT。

use crate::kernels::direct::{
    DirectCoeffsF32, DirectCoeffsF64, DirectTopology, DIRECT_MAX_CONTAINERS,
    DIRECT_MAX_PARTICIPANTS, DIRECT_MAX_ROLES, DIRECT_MAX_SOURCES, DIRECT_MAX_TAPS,
    DIRECT_NO_SOURCE,
};
use crate::kernels::{RtStatus, MAX_MATRIX_PATHS, MAX_PARTITIONS, TAPS_EXTENDED_MAX};

// ---------------------------------------------------------------------------
// 固定容量（与旧 SPEC / direct 常量绑定，见文件尾静态断言）
// ---------------------------------------------------------------------------

/// 源通道容量（旧 SPEC：1 <= N <= 32）。
pub const PARTITIONED_MAX_SOURCES: usize = DIRECT_MAX_SOURCES;
/// 输出角色容量（旧 SPEC：1 <= O <= 22；生产 role 表上限 16 由高层准入约束）。
pub const PARTITIONED_MAX_ROLES: usize = DIRECT_MAX_ROLES;
/// 低带 participants 上限（= `crate::rt::MAX_LOW_BAND_PARTICIPANTS` = 8）。
pub const PARTITIONED_MAX_PARTICIPANTS: usize = DIRECT_MAX_PARTICIPANTS;
/// 播放容器通道容量（= `crate::rt::MAX_OUTPUT_CHANNELS` = 22）。
pub const PARTITIONED_MAX_CONTAINERS: usize = DIRECT_MAX_CONTAINERS;
/// 每路融合有效支撑上限：L_F = L_A + L_G + L_P - 2 <= 4096（= `crate::rt::TAPS_NORMAL_MAX`）。
pub const PARTITIONED_MAX_TAPS: usize = DIRECT_MAX_TAPS;
/// partitioned 引擎的 extended 融合支撑上限：L_F <= `crate::kernels::TAPS_EXTENDED_MAX` = 16384
/// （B=64 时 Q = 256）。仅 `PartitionedLayout::build_extended` 使用；direct 仍受 4096 限制。
pub const PARTITIONED_MAX_TAPS_EXTENDED: usize = TAPS_EXTENDED_MAX as usize;
/// 单 block 样本数上限（生产准入 B in {64,128}；1..=128 任意整数作 [TEST]/reference 覆盖）。
pub const PARTITIONED_MAX_BLOCK: usize = 128;
/// 共享 FFT 长度上限 K = next_pow2(2 x 128) = 256。
pub const PARTITIONED_MAX_FFT: usize = 2 * PARTITIONED_MAX_BLOCK;
/// twiddle 表长度上限（K/2）。
pub const PARTITIONED_MAX_TWIDDLE: usize = PARTITIONED_MAX_FFT / 2;
/// 每路径路由（输出角色, 源通道）数上限（= `crate::kernels::MAX_MATRIX_PATHS` = 128）。
pub const PARTITIONED_MAX_ROUTES: usize = MAX_MATRIX_PATHS;
/// 每路径分区数上限（= `crate::kernels::MAX_PARTITIONS`，现为 1500）。
pub const PARTITIONED_MAX_PARTITIONS: usize = MAX_PARTITIONS as usize;

// ---------------------------------------------------------------------------
// [NONRT] 布局：plan / state / build scratch 的固定容量与分区偏移
// ---------------------------------------------------------------------------

/// [NONRT] 共享 FFT 分区卷积的固定缓冲布局（Copy/POD；偏移/容量以“样本数”计）。
///
/// 记 B = block_size、W = 2B = window_samples、K = fft_size = next_pow2(W)（B 为 2 的幂时
/// K=W）。state 分区（全部在调用方一条 state 切片内）：x_prev 为 n_src*B（上一块输入样本，
/// 构成 K 窗口的前半）；x_hist 为 n_src*Q*2K（每源 Q 个历史频谱槽，环形；b-p 历史即槽
/// (slot-p) mod Q）；work 为 4K（FFT 实部/虚部 + 每行累加实部/虚部）；slot 为 1（下一个写入
/// 槽号 0..Q-1，以标量保存，f32/f64 精确可表示）。plan 分区（调用方一条 plan_data 切片）：
/// twiddle K 个（K/2 re + K/2 im），随后每条路由每个分区 2K 个（K re + K im）。
/// plan 与 state 都由 NONRT 一次分配，RT 只借用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartitionedLayout {
    /// 源通道数 N。
    pub n_src: u16,
    /// 输出角色数 O。
    pub n_role: u16,
    /// 播放容器通道数 C（>= n_role）。
    pub container_channels: u16,
    /// 分块长度 B（1..=128；生产 B 属于 {64,128}）。
    pub block_size: u16,
    /// 窗口样本数 W = 2 x B（FFT 前零填充到 fft_size）。
    pub window_samples: u16,
    /// 共享 FFT 长度 K = next_pow2(2 x B)（B 为 2 的幂时 K = 2B）。
    pub fft_size: u16,
    /// 每路分区数 Q = ceil(L_F / B)。
    pub partitions: u16,
    /// 路由数（输出角色, 源通道）。
    pub n_routes: u16,
    /// 融合路由长度 L_F = L_A + L_G + L_P - 2（D 计入 L_G）。
    pub taps: u32,
    /// x_prev 分区起始样本偏移。
    pub x_prev_off: u32,
    /// x_hist 分区起始样本偏移。
    pub x_hist_off: u32,
    /// work 分区起始样本偏移。
    pub work_off: u32,
    /// slot 计数分区起始样本偏移。
    pub slot_off: u32,
    /// state 需要的最小样本数。
    pub state_samples: u32,
    /// plan_data 需要的最小样本数。
    pub plan_samples: u32,
    /// build_scratch 需要的最小样本数。
    pub build_scratch_samples: u32,
}

impl PartitionedLayout {
    /// [NONRT] 由拓扑 + block size 建立固定缓冲布局并校验容量（direct 口径上限 4096）。
    ///
    /// 校验项：拓扑合法性（复用 `DirectTopology::validate`）、B 属于 1..=128（任意整数，
    /// FFT 长度 K = next_pow2(2B)）、Q <= MAX_PARTITIONS、路由数 <= MAX_ROUTES、容量 32 位不溢出。
    pub fn build(topo: &DirectTopology, block_size: usize) -> Result<Self, RtStatus> {
        Self::build_with_fused_taps(topo, block_size, PARTITIONED_MAX_TAPS)
    }

    /// [NONRT] partitioned 引擎 extended 口径（L_F <= `crate::kernels::TAPS_EXTENDED_MAX`）。
    ///
    /// 生产校准链的 4096-tap P_post FIR 在 host 适配器里会与 G 支撑融合成
    /// L_F = L_A + L_G + L_P - 2 = 4103（> 4096 但 <= 16384）：direct 引擎按冻结口径拒绝，
    /// partitioned 引擎可承载（Q=ceil(L_F/B) 仍 <= MAX_PARTITIONS）。内核公式/误差门不变。
    pub fn build_extended(topo: &DirectTopology, block_size: usize) -> Result<Self, RtStatus> {
        Self::build_with_fused_taps(topo, block_size, PARTITIONED_MAX_TAPS_EXTENDED)
    }

    /// [NONRT] 引擎相关布局构建：`max_fused_taps` 是 L_F 上限（direct=4096 / partitioned=16384）。
    pub fn build_with_fused_taps(
        topo: &DirectTopology,
        block_size: usize,
        max_fused_taps: usize,
    ) -> Result<Self, RtStatus> {
        let status = topo.validate_with_fused_taps(max_fused_taps);
        if !status.is_ok() {
            return Err(status);
        }
        if block_size == 0 || block_size > PARTITIONED_MAX_BLOCK {
            return Err(RtStatus::ERR_BLOCK_SIZE);
        }
        let b = block_size as u64;
        let window = 2 * b;
        let mut k = 1u64;
        while k < window {
            k <<= 1;
        }
        if k > PARTITIONED_MAX_FFT as u64 {
            return Err(RtStatus::ERR_BLOCK_SIZE);
        }
        let l_f = topo.l_a as u64 + topo.l_g as u64 + topo.l_p as u64 - 2;
        if l_f == 0 || l_f > max_fused_taps as u64 {
            return Err(RtStatus::ERR_TAPS);
        }
        let q = l_f.div_ceil(b);
        if q > PARTITIONED_MAX_PARTITIONS as u64 {
            return Err(RtStatus::ERR_TAPS);
        }
        let table = route_table(topo);
        if table.n_routes as usize > PARTITIONED_MAX_ROUTES {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        let n_src = topo.n_src as u64;
        let n_route = table.n_routes as u64;
        let x_prev = n_src * b;
        let x_hist = n_src * q * k * 2;
        let work = 4 * k;
        let x_prev_off = 0u64;
        let x_hist_off = x_prev_off + x_prev;
        let work_off = x_hist_off + x_hist;
        let slot_off = work_off + work;
        let state = slot_off + 1;
        let plan = k + n_route * q * k * 2;
        // 构建 scratch：两段 L_F（G*A 中间结果 + 融合路由）+ FFT 实/虚部 2K。
        let build_scratch = 2 * l_f + 2 * k;
        let max = u32::MAX as u64;
        if state > max || plan > max || build_scratch > max {
            return Err(RtStatus::ERR_CAPACITY);
        }
        Ok(Self {
            n_src: topo.n_src,
            n_role: topo.n_role,
            container_channels: topo.container_channels,
            block_size: block_size as u16,
            window_samples: window as u16,
            fft_size: k as u16,
            partitions: q as u16,
            n_routes: table.n_routes,
            taps: l_f as u32,
            x_prev_off: x_prev_off as u32,
            x_hist_off: x_hist_off as u32,
            work_off: work_off as u32,
            slot_off: slot_off as u32,
            state_samples: state as u32,
            plan_samples: plan as u32,
            build_scratch_samples: build_scratch as u32,
        })
    }

    /// state 需要的最小样本数（调用方按此长度提供并清零 state）。
    pub const fn state_samples(&self) -> usize {
        self.state_samples as usize
    }

    /// plan_data 需要的最小样本数（调用方按此长度提供固定缓冲）。
    pub const fn plan_samples(&self) -> usize {
        self.plan_samples as usize
    }

    /// build scratch 需要的最小样本数（[NONRT] 构建期临时缓冲）。
    pub const fn build_scratch_samples(&self) -> usize {
        self.build_scratch_samples as usize
    }
}

// ---------------------------------------------------------------------------
// 路由表（[NONRT] 由拓扑确定；plan 携带、RT 校验）
// ---------------------------------------------------------------------------

/// 私有路由表（POD；[NONRT] 生成，plan 字段公开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RouteTable {
    out: [u16; PARTITIONED_MAX_ROUTES],
    src: [u16; PARTITIONED_MAX_ROUTES],
    n_routes: u16,
}

/// 角色在 participants 的 dense 位置（非 participant 返回 None）。
fn participant_pos(topo: &DirectTopology, role: u16) -> Option<usize> {
    let mut a = 0usize;
    while a < topo.m as usize {
        if topo.participants[a] == role {
            return Some(a);
        }
        a += 1;
    }
    None
}

/// 选择一个源的角色（A_map 每源至多一行；拓扑已校验）。
fn role_of_source(topo: &DirectTopology, src: u16) -> u16 {
    let mut j = 0u16;
    while j < topo.n_role {
        if topo.src_of_role[j as usize] == src {
            return j;
        }
        j += 1;
    }
    DIRECT_NO_SOURCE
}

/// 枚举融合路由，顺序固定：输出角色 j 升序；每个 j 先对角 baseline 路由（若该行有源），
/// 再按 participants 顺序枚举 k != j 且该 participant 有源的非对角路由。
///
/// 对角路由的 G_jj 含 delta_D（D 计入支撑）与可选 g_jj；非对角路由只有 g_jk（只有
/// participant x participant 才有 g 项）。顺序固定保证跨 block/跨平台同一构建的浮点累加顺序
/// 可复现。
///
// NOTE(legacy-defect): 旧实现在 `n_routes` 达到 PARTITIONED_MAX_ROUTES 时静默丢弃后续路由
// （数组写有界、不会越界，但会得到缺路由的错误输出且无错误码）。静态断言证明当前上界
// 78 <= 128 不可达；迁移保留旧行为（不加运行期错误路径），仅在此登记该防御缺口。
fn route_table(topo: &DirectTopology) -> RouteTable {
    let mut out = [0u16; PARTITIONED_MAX_ROUTES];
    let mut src = [0u16; PARTITIONED_MAX_ROUTES];
    let mut n = 0usize;
    let mut j = 0usize;
    while j < topo.n_role as usize {
        let src_of_j = topo.src_of_role[j];
        if src_of_j != DIRECT_NO_SOURCE && n < PARTITIONED_MAX_ROUTES {
            out[n] = j as u16;
            src[n] = src_of_j;
            n += 1;
        }
        if participant_pos(topo, j as u16).is_some() {
            let mut a = 0usize;
            while a < topo.m as usize {
                let role = topo.participants[a] as usize;
                if role != j {
                    let src_of_k = topo.src_of_role[role];
                    if src_of_k != DIRECT_NO_SOURCE && n < PARTITIONED_MAX_ROUTES {
                        out[n] = j as u16;
                        src[n] = src_of_k;
                        n += 1;
                    }
                }
                a += 1;
            }
        }
        j += 1;
    }
    RouteTable {
        out,
        src,
        n_routes: n as u16,
    }
}

// ---------------------------------------------------------------------------
// 私有标量抽象（泛型只在模块内部，公开入口不暴露泛型）
// ---------------------------------------------------------------------------

trait KernelSample: Copy {
    const ZERO: Self;
    const ONE: Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn mul(self, other: Self) -> Self;
    fn div_scalar(self, divisor: usize) -> Self;
    fn from_f64(value: f64) -> Self;
    fn as_usize(self) -> usize;
}

impl KernelSample for f32 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn sub(self, other: Self) -> Self {
        self - other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }
    fn div_scalar(self, divisor: usize) -> Self {
        self / divisor as f32
    }
    fn from_f64(value: f64) -> Self {
        value as f32
    }
    fn as_usize(self) -> usize {
        self as usize
    }
}

impl KernelSample for f64 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn sub(self, other: Self) -> Self {
        self - other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }
    fn div_scalar(self, divisor: usize) -> Self {
        self / divisor as f64
    }
    fn from_f64(value: f64) -> Self {
        value
    }
    fn as_usize(self) -> usize {
        self as usize
    }
}

// ---------------------------------------------------------------------------
// [NONRT] 预分配 plan（twiddle 表 + 各路由各分区频谱）
// ---------------------------------------------------------------------------

/// [NONRT] 预分配的分区卷积 plan（Copy/POD；由 NONRT 构建，经引用进入 RT）。
///
/// 路由表与布局固定；可变数据（twiddle 表 + 各路由各分区滤波器频谱）存于调用方提供的
/// plan_data 固定缓冲，由 `build_f32`/`build_f64` 一次写入并在发布后只读。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartitionedPlan {
    /// 固定布局/容量。
    pub layout: PartitionedLayout,
    /// 构建时的融合支撑上限（direct/normal=4096；partitioned extended=16384）。
    ///
    /// `validate_call` 按同一上限复算布局，避免 extended plan 在 RT 入口被 normal 口径误拒。
    pub fused_taps_limit: u32,
    /// 路由输出角色（前 n_routes 项有效）。
    pub route_out: [u16; PARTITIONED_MAX_ROUTES],
    /// 路由源通道（前 n_routes 项有效）。
    pub route_src: [u16; PARTITIONED_MAX_ROUTES],
}

impl PartitionedPlan {
    /// [NONRT] 构建 f32 plan：twiddle 表 + 融合路由 FIR 的分区频谱写入 plan_data。
    ///
    /// 参数：topo/coeffs 与 direct 同一 reference 语义（a/g/p 行主序零填充）；plan_data 长度
    /// 不小于 `PartitionedLayout::plan_samples()`；build_scratch 长度不小于
    /// `PartitionedLayout::build_scratch_samples()`。失败返回固定错误码（ERR_PATH_TABLE 系数
    /// 长度不符、其它见 `PartitionedLayout::build`）。本函数只被 NONRT 调用，不属于 RT 路径。
    pub fn build_f32(
        topo: &DirectTopology,
        block_size: usize,
        coeffs: &DirectCoeffsF32<'_>,
        plan_data: &mut [f32],
        build_scratch: &mut [f32],
    ) -> Result<Self, RtStatus> {
        build_plan_scalar(
            topo,
            block_size,
            coeffs.a_fir,
            coeffs.g,
            coeffs.p,
            plan_data,
            build_scratch,
            PARTITIONED_MAX_TAPS,
        )
    }

    /// [NONRT] 构建 f32 plan（partitioned extended 口径）：融合支撑上限
    /// `crate::kernels::TAPS_EXTENDED_MAX` = 16384。
    ///
    /// 生产 4096-tap P_post FIR 的融合 L_F=4103（> direct 的 4096）：仅 plan-arena 的
    /// partitioned 引擎使用本入口；direct 仍用 `build_f32`（4096）并在超限时 fail closed。
    /// 内核公式/误差门与 `build_f32` 完全相同（只放宽布局的 L_F 上限）。
    pub fn build_f32_extended(
        topo: &DirectTopology,
        block_size: usize,
        coeffs: &DirectCoeffsF32<'_>,
        plan_data: &mut [f32],
        build_scratch: &mut [f32],
    ) -> Result<Self, RtStatus> {
        build_plan_scalar(
            topo,
            block_size,
            coeffs.a_fir,
            coeffs.g,
            coeffs.p,
            plan_data,
            build_scratch,
            PARTITIONED_MAX_TAPS_EXTENDED,
        )
    }

    /// [NONRT] 构建 f64 plan（[TEST]/offline oracle 用；生产只准入 f32）。
    pub fn build_f64(
        topo: &DirectTopology,
        block_size: usize,
        coeffs: &DirectCoeffsF64<'_>,
        plan_data: &mut [f64],
        build_scratch: &mut [f64],
    ) -> Result<Self, RtStatus> {
        build_plan_scalar(
            topo,
            block_size,
            coeffs.a_fir,
            coeffs.g,
            coeffs.p,
            plan_data,
            build_scratch,
            PARTITIONED_MAX_TAPS,
        )
    }

    /// [NONRT] 构建 f64 plan（partitioned extended 口径；仅 [TEST]/offline oracle）。
    pub fn build_f64_extended(
        topo: &DirectTopology,
        block_size: usize,
        coeffs: &DirectCoeffsF64<'_>,
        plan_data: &mut [f64],
        build_scratch: &mut [f64],
    ) -> Result<Self, RtStatus> {
        build_plan_scalar(
            topo,
            block_size,
            coeffs.a_fir,
            coeffs.g,
            coeffs.p,
            plan_data,
            build_scratch,
            PARTITIONED_MAX_TAPS_EXTENDED,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn build_plan_scalar<T: KernelSample>(
    topo: &DirectTopology,
    block_size: usize,
    a_fir: &[T],
    g: &[T],
    p: &[T],
    plan_data: &mut [T],
    build_scratch: &mut [T],
    max_fused_taps: usize,
) -> Result<PartitionedPlan, RtStatus> {
    let layout = PartitionedLayout::build_with_fused_taps(topo, block_size, max_fused_taps)?;
    let n_src = topo.n_src as usize;
    let l_a = topo.l_a as usize;
    let l_g = topo.l_g as usize;
    let l_p = topo.l_p as usize;
    let m = topo.m as usize;
    if a_fir.len() != n_src * l_a || g.len() != m * m * l_g || p.len() != topo.n_role as usize * l_p
    {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    if plan_data.len() < layout.plan_samples() {
        return Err(RtStatus::ERR_CAPACITY);
    }
    if build_scratch.len() < layout.build_scratch_samples() {
        return Err(RtStatus::ERR_CAPACITY);
    }
    let table = route_table(topo);
    if table.n_routes != layout.n_routes {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }

    let b = layout.block_size as usize;
    let k = layout.fft_size as usize;
    let half = k / 2;
    let q = layout.partitions as usize;
    let l_f = layout.taps as usize;
    let n_routes = layout.n_routes as usize;

    // twiddle 表：w[i] = exp(-j 2 pi i / K)，i = 0..K/2-1（实部 + 虚部两块）。
    let mut i = 0usize;
    while i < half {
        let angle = -2.0 * core::f64::consts::PI * (i as f64) / (k as f64);
        plan_data[i] = T::from_f64(angle.cos());
        plan_data[half + i] = T::from_f64(angle.sin());
        i += 1;
    }

    // 临时缓冲（全部由调用方预分配的 build_scratch 提供）。
    let (t1_area, rest) = build_scratch.split_at_mut(l_f);
    let (route_area, work_area) = rest.split_at_mut(l_f);
    let t1_len = l_a + l_g - 1;
    let t1 = &mut t1_area[..t1_len];
    let route = &mut route_area[..l_f];
    let work = &mut work_area[..2 * k];

    // twiddle 切片（plan_data 前 K 个；之后是只写频谱区）。
    let (tw, spectra) = plan_data.split_at_mut(k);
    let (tw_re, tw_im) = tw.split_at(half);
    let tw_re: &[T] = tw_re;
    let tw_im: &[T] = tw_im;

    let mut r = 0usize;
    while r < n_routes {
        let j = table.out[r] as usize;
        let src = table.src[r] as usize;
        let k_role = role_of_source(topo, src as u16) as usize;
        // 清空本路由临时缓冲。
        let mut t = 0usize;
        while t < l_f {
            route[t] = T::ZERO;
            t += 1;
        }
        t = 0usize;
        while t < t1_len {
            t1[t] = T::ZERO;
            t += 1;
        }
        // G_full[j][k] 的声明支撑 L_G：对角项含 delta_D baseline；participant x participant
        // 有 g；非 participant 对角行只有 baseline，非对角项只为 0。
        let mut has_g = false;
        let mut g_base = 0usize;
        if let (Some(j_pos), Some(k_pos)) = (
            participant_pos(topo, j as u16),
            participant_pos(topo, k_role as u16),
        ) {
            has_g = true;
            g_base = (j_pos * m + k_pos) * l_g;
        }
        let mut t_g = 0usize;
        while t_g < l_g {
            let mut coef = T::ZERO;
            if has_g {
                coef = g[g_base + t_g];
            }
            if j == k_role && t_g == topo.d_samples as usize {
                coef = coef.add(T::ONE);
            }
            let mut t_a = 0usize;
            while t_a < l_a {
                let value = t1[t_g + t_a].add(coef.mul(a_fir[src * l_a + t_a]));
                t1[t_g + t_a] = value;
                t_a += 1;
            }
            t_g += 1;
        }
        // route = t1 * p_j（长度 L_P + (L_A+L_G-1) - 1 = L_F）。
        let mut t_p = 0usize;
        while t_p < l_p {
            let coef = p[j * l_p + t_p];
            let mut idx = 0usize;
            while idx < t1_len {
                let value = route[t_p + idx].add(coef.mul(t1[idx]));
                route[t_p + idx] = value;
                idx += 1;
            }
            t_p += 1;
        }
        // 每分区频谱：h_r[p] = route[p*B .. p*B+B) 零填充到 K 后 FFT。
        let mut part = 0usize;
        while part < q {
            let mut n = 0usize;
            while n < 2 * k {
                work[n] = T::ZERO;
                n += 1;
            }
            let mut t_idx = 0usize;
            while t_idx < b {
                let src_idx = part * b + t_idx;
                if src_idx < l_f {
                    work[t_idx] = route[src_idx];
                }
                t_idx += 1;
            }
            fft_forward_block(work, k, tw_re, tw_im);
            let spec_off = (r * q + part) * 2 * k;
            let mut n2 = 0usize;
            while n2 < k {
                spectra[spec_off + n2] = work[n2];
                spectra[spec_off + k + n2] = work[k + n2];
                n2 += 1;
            }
            part += 1;
        }
        r += 1;
    }

    Ok(PartitionedPlan {
        layout,
        fused_taps_limit: max_fused_taps as u32,
        route_out: table.out,
        route_src: table.src,
    })
}

// ---------------------------------------------------------------------------
// [RT] 生产入口（f32）与 [TEST] oracle 入口（f64）
// ---------------------------------------------------------------------------

/// [RT] 分区卷积一个 block（planar f32 输入到 planar f32 输出，固定缓冲，无分配）。
///
/// 参数：topo/plan/plan_data 由 [NONRT] 一次构建并发布后只读；state 为布局给出长度的可写
/// 固定缓冲（首次使用前必须清零，跨 block 保持）；input/output 为 planar 缓冲，
/// in_stride/out_stride 为每通道样本步长（调用方保证 >= block_size）。
///
/// `valid_frames == block_size`：正常块（推进历史/环槽）。`valid_frames < block_size`（含 V=0）
/// 是统一的终止性 DRAIN：输入 `[V,B)` 视作 0；输出对 `[0,V)` 给出正确卷积值、对 `[V,B)`
/// 写 +0.0（V=0 时整块 +0.0）；处理完清零 x_prev/x_hist 并置 slot=0（等价新 epoch）。
/// 调用方不得在同一 epoch 继续喂流。
#[allow(clippy::too_many_arguments)]
pub fn partitioned_block_f32(
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
    run_block_scalar(
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

/// [TEST]/offline oracle 分区卷积（f64；与旧 reference / direct f64 对拍用）。
///
/// 语义与 `partitioned_block_f32` 完全相同，只换标量精度。f64 不进入生产 RT 调用图，
/// 仅供 offline/oracle 与夹具验收。
#[allow(clippy::too_many_arguments)]
pub fn partitioned_block_f64(
    topo: &DirectTopology,
    plan: &PartitionedPlan,
    plan_data: &[f64],
    state: &mut [f64],
    input: &[f64],
    in_stride: usize,
    output: &mut [f64],
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
) -> RtStatus {
    run_block_scalar(
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

/// [NONRT] 把 f32 state 缓冲清零（新 epoch / 首块前调用；只写固定缓冲，无分配）。
pub fn partitioned_state_zero_f32(state: &mut [f32]) {
    for value in state.iter_mut() {
        *value = 0.0;
    }
}

/// [NONRT] 把 f64 oracle state 缓冲清零（测试/offline 用）。
pub fn partitioned_state_zero_f64(state: &mut [f64]) {
    for value in state.iter_mut() {
        *value = 0.0;
    }
}

// ---------------------------------------------------------------------------
// [RT] 分区卷积实现（泛型内部）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_block_scalar<T: KernelSample>(
    topo: &DirectTopology,
    plan: &PartitionedPlan,
    plan_data: &[T],
    state: &mut [T],
    input: &[T],
    in_stride: usize,
    output: &mut [T],
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
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
    let work_re = work_off;
    let work_im = work_off + k;
    let acc_re = work_off + 2 * k;
    let acc_im = work_off + 3 * k;

    // twiddle 表位于 plan_data 前 K 个（K/2 re + K/2 im）。
    let (tw_re, tw_im) = plan_data[..k].split_at(half);

    // 输出块先整体清零：未映射容器通道/valid_frames 之后的样本必须逐样本严格 +0.0。
    let mut c = 0usize;
    while c < containers {
        let base = c * out_stride;
        let mut n = 0usize;
        while n < block_size {
            output[base + n] = T::ZERO;
            n += 1;
        }
        c += 1;
    }
    // valid_frames < B（含 V=0）统一为终止性 DRAIN：输出已清零；V=0 不再做 FFT/卷积，
    // 直接清历史（x_prev + x_hist）并置 slot=0，与 0<V<B 完全对称（新 epoch 起点）。
    if valid_frames == 0 {
        reset_history(&layout, state);
        return RtStatus::OK;
    }

    // 当前历史写入槽（0..Q-1；标量保存，分块足够小使 f32 精确）。
    let slot = state[slot_off].as_usize();
    // 防御：slot 越界（state 损坏）按 epoch 起点处理，不 panic。
    let slot = if slot < q { slot } else { 0 };

    // 阶段 1：每源构造窗口 W = [x_prev(B), 当前块(B) 有效样本 + 零] 并 FFT，写入共享历史槽。
    let mut i = 0usize;
    while i < n_src {
        let mut n = 0usize;
        while n < b {
            state[work_re + n] = state[x_prev_off + i * b + n];
            state[work_im + n] = T::ZERO;
            let cur = if n < valid_frames {
                input[i * in_stride + n]
            } else {
                T::ZERO
            };
            state[work_re + b + n] = cur;
            state[work_im + b + n] = T::ZERO;
            n += 1;
        }
        // 窗口长度 W=2B；K=next_pow2(W) 时把 [2B,K) 的实/虚部补零（B 为 2 的幂时循环为空）。
        let mut nz = 2 * b;
        while nz < k {
            state[work_re + nz] = T::ZERO;
            state[work_im + nz] = T::ZERO;
            nz += 1;
        }
        fft_forward_block_offsets(state, work_re, work_im, k, tw_re, tw_im);
        let hist_base = x_hist_off + (i * q + slot) * 2 * k;
        let mut n2 = 0usize;
        while n2 < k {
            state[hist_base + n2] = state[work_re + n2];
            state[hist_base + k + n2] = state[work_im + n2];
            n2 += 1;
        }
        i += 1;
    }

    // 阶段 2：每输出角色按固定路由顺序累加频域 Y_j = sum_routes sum_p X_hist[b-p] * H_r[p]，
    // 然后 IFFT（共轭技巧）+ overlap-save 取后 B 样本，写 scatter 容器通道。
    let mut j = 0usize;
    while j < n_role {
        let mut n = 0usize;
        while n < 2 * k {
            state[acc_re + n] = T::ZERO;
            n += 1;
        }
        let mut r = 0usize;
        while r < n_routes {
            if plan.route_out[r] as usize == j {
                let src = plan.route_src[r] as usize;
                let hist_base = x_hist_off + (src * q) * 2 * k;
                let spec_base = k + (r * q) * 2 * k;
                let mut part = 0usize;
                while part < q {
                    let hist_slot = (slot + q - part) % q;
                    let x_off = hist_base + hist_slot * 2 * k;
                    let h_off = spec_base + part * 2 * k;
                    let mut n2 = 0usize;
                    while n2 < k {
                        let xr = state[x_off + n2];
                        let xi = state[x_off + k + n2];
                        let hr = plan_data[h_off + n2];
                        let hi = plan_data[h_off + k + n2];
                        let re = state[acc_re + n2].add(xr.mul(hr).sub(xi.mul(hi)));
                        let im = state[acc_im + n2].add(xr.mul(hi).add(xi.mul(hr)));
                        state[acc_re + n2] = re;
                        state[acc_im + n2] = im;
                        n2 += 1;
                    }
                    part += 1;
                }
            }
            r += 1;
        }
        // Re(IFFT(Y))[n] = Re(FFT(conj Y))[n] / K：共轭后做一次正向 FFT，取实部 / K。
        let mut n3 = 0usize;
        while n3 < k {
            state[acc_im + n3] = T::ZERO.sub(state[acc_im + n3]);
            n3 += 1;
        }
        fft_forward_block_offsets(state, acc_re, acc_im, k, tw_re, tw_im);
        let scatter = topo.scatter[j] as usize;
        let out_base = scatter * out_stride;
        let mut n4 = 0usize;
        while n4 < valid_frames {
            output[out_base + n4] = state[acc_re + b + n4].div_scalar(k);
            n4 += 1;
        }
        j += 1;
    }

    if valid_frames == b {
        // 阶段 3：历史搬运（每 block 一次 O(n_src*B)，不做逐样本移位）并推进环槽。
        let mut i2 = 0usize;
        while i2 < n_src {
            let mut n = 0usize;
            while n < b {
                state[x_prev_off + i2 * b + n] = input[i2 * in_stride + n];
                n += 1;
            }
            i2 += 1;
        }
        let next = (slot + 1) % q;
        state[slot_off] = T::from_f64(next as f64);
    } else {
        // 终止性 DRAIN（0 <= V < B）：清 x_prev/x_hist 并置 slot=0，后续调用从新 epoch 起点开始。
        reset_history(&layout, state);
    }
    RtStatus::OK
}

/// [RT] 清空可变历史（x_prev + x_hist）并把环槽归零 = DRAIN 终止/新 epoch 起点。
///
/// 只写语义上“活”的历史区（x_prev_off..work_off）与 slot；work/acc 是每块先写后读的
/// scratch，不需要清零。最大配置下一次 O(n_src*Q*2K) 写（RT 内一次、无分配/锁）。
fn reset_history<T: KernelSample>(layout: &PartitionedLayout, state: &mut [T]) {
    let mut n = layout.x_prev_off as usize;
    let end = layout.work_off as usize;
    while n < end {
        state[n] = T::ZERO;
        n += 1;
    }
    state[layout.slot_off as usize] = T::ZERO;
}

/// [RT] 入口参数校验（只读标量/长度，不写缓冲）；任一不符返回固定错误码。
///
/// 标量入口与 SIMD 入口（`super::partitioned_simd`）共用同一套校验：SIMD 入口在校验失败时
/// 返回与标量入口完全相同的固定错误码，且失败前不写任何缓冲。
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_call(
    topo: &DirectTopology,
    plan: &PartitionedPlan,
    plan_data_len: usize,
    state_len: usize,
    input_len: usize,
    in_stride: usize,
    output_len: usize,
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
) -> RtStatus {
    // 按构建该 plan 时的融合支撑上限校验（normal=4096 / extended=16384）：extended plan
    // 不能在 RT 入口被 normal 口径误拒；topo/plan 不一致由下面的布局相等检查兜底。
    let status = topo.validate_with_fused_taps(plan.fused_taps_limit as usize);
    if !status.is_ok() {
        return status;
    }
    let expected = match PartitionedLayout::build_with_fused_taps(
        topo,
        block_size,
        plan.fused_taps_limit as usize,
    ) {
        Ok(expected) => expected,
        Err(err) => return err,
    };
    if plan.layout != expected {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    let table = route_table(topo);
    if table.n_routes != plan.layout.n_routes {
        return RtStatus::ERR_PATH_TABLE;
    }
    let mut r = 0usize;
    while r < table.n_routes as usize {
        if plan.route_out[r] != table.out[r] || plan.route_src[r] != table.src[r] {
            return RtStatus::ERR_PATH_TABLE;
        }
        r += 1;
    }
    if block_size != plan.layout.block_size as usize {
        return RtStatus::ERR_BLOCK_SIZE;
    }
    if valid_frames > block_size {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    if plan_data_len < plan.layout.plan_samples() || state_len < plan.layout.state_samples() {
        return RtStatus::ERR_CAPACITY;
    }
    if in_stride < block_size || out_stride < block_size {
        return RtStatus::ERR_STRIDE_ALIGN;
    }
    let n_src = plan.layout.n_src as usize;
    let containers = plan.layout.container_channels as usize;
    let need_in = match n_src.checked_mul(in_stride) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    let need_out = match containers.checked_mul(out_stride) {
        Some(value) => value,
        None => return RtStatus::ERR_CAPACITY,
    };
    if input_len < need_in || output_len < need_out {
        return RtStatus::ERR_CAPACITY;
    }
    RtStatus::OK
}

// ---------------------------------------------------------------------------
// radix-2 复数 FFT（原地；twiddle 由 plan 预分配，RT 内不建 plan）
// ---------------------------------------------------------------------------

/// 对 buf[0..2K) 布局（前 K 实部、后 K 虚部）的复数序列做正向 radix-2 FFT。
///
/// K 必须是 2 的幂（布局校验保证）；twiddle 长度 K/2（re + im 由调用方分开给出）。
fn fft_forward_block<T: KernelSample>(buf: &mut [T], k: usize, tw_re: &[T], tw_im: &[T]) {
    fft_forward_block_offsets(buf, 0, k, k, tw_re, tw_im);
}

/// 同 `fft_forward_block`，但实部/虚部位于同一缓冲的 re_off/im_off 偏移处（RT 内复用 work 区）。
fn fft_forward_block_offsets<T: KernelSample>(
    buf: &mut [T],
    re_off: usize,
    im_off: usize,
    k: usize,
    tw_re: &[T],
    tw_im: &[T],
) {
    // 位反序置换。
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
            buf.swap(re_off + i, re_off + j);
            buf.swap(im_off + i, im_off + j);
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
            let mut t = 0usize;
            while t < half {
                let tw = t * step;
                let w_re = tw_re[tw];
                let w_im = tw_im[tw];
                let a = start + t;
                let b = a + half;
                let br = buf[re_off + b];
                let bi = buf[im_off + b];
                let tr = br.mul(w_re).sub(bi.mul(w_im));
                let ti = br.mul(w_im).add(bi.mul(w_re));
                let ar = buf[re_off + a];
                let ai = buf[im_off + a];
                buf[re_off + b] = ar.sub(tr);
                buf[im_off + b] = ai.sub(ti);
                buf[re_off + a] = ar.add(tr);
                buf[im_off + a] = ai.add(ti);
                t += 1;
            }
            start += len;
        }
        len <<= 1;
    }
}

// ---------------------------------------------------------------------------
// 编译期静态断言（needs_drop 与旧容量绑定）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(PARTITIONED_MAX_SOURCES == 32);
    assert!(PARTITIONED_MAX_ROLES == 22);
    assert!(PARTITIONED_MAX_ROLES <= PARTITIONED_MAX_CONTAINERS);
    assert!(PARTITIONED_MAX_PARTICIPANTS == crate::rt::MAX_LOW_BAND_PARTICIPANTS);
    assert!(PARTITIONED_MAX_CONTAINERS == crate::rt::MAX_OUTPUT_CHANNELS);
    assert!(PARTITIONED_MAX_TAPS as u32 == crate::rt::TAPS_NORMAL_MAX);
    assert!(PARTITIONED_MAX_TAPS_EXTENDED as u32 == TAPS_EXTENDED_MAX);
    assert!(
        PARTITIONED_MAX_TAPS_EXTENDED
            <= PARTITIONED_MAX_PARTITIONS * crate::rt::BLOCK_SIZES[0] as usize
    );
    assert!(PARTITIONED_MAX_BLOCK == crate::rt::BLOCK_SIZES[1] as usize);
    assert!(PARTITIONED_MAX_FFT == 2 * PARTITIONED_MAX_BLOCK);
    assert!(PARTITIONED_MAX_ROUTES == MAX_MATRIX_PATHS);
    assert!(PARTITIONED_MAX_PARTITIONS == MAX_PARTITIONS as usize);
    // 路由上界：对角 <= O，非对角 <= M*(M-1)；O <= 22、M <= 8 -> <= 78 <= 128。
    assert!(
        PARTITIONED_MAX_ROLES + PARTITIONED_MAX_PARTICIPANTS * (PARTITIONED_MAX_PARTICIPANTS - 1)
            <= PARTITIONED_MAX_ROUTES
    );
    assert!(!core::mem::needs_drop::<PartitionedLayout>());
    assert!(!core::mem::needs_drop::<PartitionedPlan>());
    assert!(!core::mem::needs_drop::<DirectTopology>());
};
