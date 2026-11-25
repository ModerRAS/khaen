//! [RT] 全采样率 direct 卷积内核（legacy/pre-rewrite 的 T07 迁移；写域
//! `crates/core/src/kernels/direct.rs`）。
//!
//! RT-MODULE: 本文件实现 DSP 纯 worker 路径的 RT 模块，禁止析构/分配/锁/syscall/日志/panic；
//! 例外仅为 `direct_block_f64` 与 `DirectCoeffsF64`：它们是 [TEST]/offline oracle 入口，
//! 不进入生产 RT 调用图（生产只准入 f32）。
//!
//! 实现旧 reference 模型的固定缓冲全速直算（语义与旧实现逐位一致）：
//!
//! ~~~text
//! u_i[n] = sum_k a_i[k] * x_i[n-k]                  （输入侧 FIR，作用在列 i）
//! u_j    = u_src(j) 或 0                             （A_map 只选择/零填）
//! G[j][k] = (j == k ? z^-D : 0) + g[pos(j)][pos(k)]  （G 含 identity baseline；D 计入支撑）
//! v_j[n] = sum_k (G[j][k] * u_k)[n]                  （低带矩阵嵌入）
//! y_j[n] = sum_t p_j[t] * v_j[n-t]                   （对角 FIR，作用在行 j）
//! z[scatter[j]] = y_j；未使用容器通道逐样本 +0.0
//! ~~~
//!
//! 固定缓冲契约：kernel 只读输入/系数，只写调用方预分配并清零的 state 与 block 输出；
//! 不分配、不加锁、不 syscall、不写日志、无显式 panic 分支。state 的样本数由
//! `DirectLayout::state_samples()` 给出，block 容量固定为 `DIRECT_MAX_BLOCK`。
//!
//! 逐样本累加顺序固定（u: tap 升序；v: 列 k 升序 + tap 升序；y: tap 升序），与 block 切分
//! 无关，因此同一图/输入在任意 block 大小下逐位一致（旧 SPEC 的 block-size invariance 与
//! 固定 reduce/累加顺序要求）。
//!
//! 统一 partial/flush 契约（与 `partitioned` 文件头一致）：
//! - flush 长度 = L_F-1 个零，随后补零到 block 整数倍；两块内核同一口径。
//! - `valid_frames < block_size`：DSP 把 `[V, block_size)` 视作 0 输入；输出 `[0,V)` 有效、
//!   `[V, block_size)` 写 +0.0。
//! - direct 的 partial 块按 V 个有效样本推进历史（可拼接 partial，属 direct 扩展语义：两个
//!   V=2 块等价一个 V=4 块）；V=0 只清输出、不推进历史，不是 drain reset。
//! - partitioned 的 partial 块是终止性 DRAIN（见 `partitioned` 文件头）；连续流必须用 full
//!   block，partial 只在流末使用；在 direct 上按 DRAIN 终止 epoch 时，调用方必须随后
//!   `direct_state_zero_f32`/`_f64`（或新分配 state），不得把 direct 可拼接 partial 当作
//!   partitioned 终止性 DRAIN 在同一 epoch 内复用。
//!
//! 精度分层：`direct_block_f32` 是 [RT] 生产路径；`direct_block_f64` 仅 offline oracle
//! （不进入生产 RT 调用图）。旧实现的夹具判据（exact 逐位 / C11 tolerance）留在迁移报告与
//! 旧仓测试中，本次迁移只保证算法与旧实现逐位一致。
//!
//! 与 plan/ABI 的边界：本模块消费 reference 语义图（A_map/a/g/p/scatter）。方案 v3 的
//! Path/Role 表到 `DirectTopology` + `DirectCoeffsF32` 的适配属 host/compile 层（旧仓由 T26
//! integrator 做同样的事），内核不解析方案文件。
//!
//! 错误码（旧 leaf 约定，数值见 `crate::kernels::RtStatus`）：容量/通道 -> ERR_CHANNELS；
//! 角色/映射/scatter -> ERR_ROLES；taps/D/融合长度 -> ERR_TAPS；系数长度 -> ERR_PATH_TABLE；
//! state/输入/输出缓冲不足 -> ERR_CAPACITY；block 大小 -> ERR_BLOCK_SIZE；stride 小于 block
//! -> ERR_STRIDE_ALIGN；layout 与拓扑不符或 valid_frames > block_size -> ERR_BAD_LAYOUT。

use crate::kernels::RtStatus;

// ---------------------------------------------------------------------------
// 固定容量（与旧 SPEC/core-v1 常量绑定，见文件尾静态断言）
// ---------------------------------------------------------------------------

/// 源通道容量（旧 SPEC：1 <= N <= 32）。
pub const DIRECT_MAX_SOURCES: usize = 32;
/// 输出角色容量（旧 SPEC：1 <= O <= 22；生产 role 表上限 16 由高层准入约束）。
pub const DIRECT_MAX_ROLES: usize = 22;
/// 播放容器通道容量（= `crate::rt::MAX_OUTPUT_CHANNELS` = 22）。
pub const DIRECT_MAX_CONTAINERS: usize = crate::rt::MAX_OUTPUT_CHANNELS;
/// 低带 participants 上限（= `crate::rt::MAX_LOW_BAND_PARTICIPANTS` = 8）。
pub const DIRECT_MAX_PARTICIPANTS: usize = crate::rt::MAX_LOW_BAND_PARTICIPANTS;
/// 每路融合有效支撑上限：完整路由长 L_F = L_A + L_G + L_P - 2 <= 4096
/// （= `crate::rt::TAPS_NORMAL_MAX`）。
///
// NOTE(legacy-defect): 旧 `rt.rs::fused_len_max(l_g, l_p)` 用同一名字 “L_F” 但只算 G/P 段
// （L_G + L_P - 1），不含输入侧 L_A。误用它给 direct 引擎做准入会少算 L_A-1 个 tap。
// 迁移保留旧行为：本直算内核一律按完整路由口径 L_A+L_G+L_P-2 校验，不复用该 helper。
pub const DIRECT_MAX_TAPS: usize = crate::rt::TAPS_NORMAL_MAX as usize;
/// 单 block 样本数上限（生产准入 B in {64,128}；测试覆盖 1..=128 的流式不变性）。
pub const DIRECT_MAX_BLOCK: usize = crate::rt::BLOCK_SIZES[1] as usize;
/// `src_of_role` 的“本角色无源”哨兵值（A_map 整行为 0）。
pub const DIRECT_NO_SOURCE: u16 = u16::MAX;

// ---------------------------------------------------------------------------
// 拓扑与固定缓冲布局（POD；由 [NONRT] 构造，经引用进入 RT）
// ---------------------------------------------------------------------------

/// [NONRT] 直算图拓扑（Copy/POD，无引用/无 Drop）。由编译/适配层从方案语义图构造或测试构造。
///
/// 字段语义对应旧 reference SPEC：`src_of_role[j]` 是 A_map 第 j 行选中的源（或
/// `DIRECT_NO_SOURCE`），`participants` 是 G 的 dense 低带角色序列，`scatter` 是角色到容器
/// 通道单射映射。数组在 n_*/m 之外的条目被忽略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectTopology {
    /// 源通道数 N。
    pub n_src: u16,
    /// 输出角色数 O。
    pub n_role: u16,
    /// 播放容器通道数 C（>= n_role）。
    pub container_channels: u16,
    /// 预声明公共 baseline 延迟 D（样本；计入 G 支撑）。
    pub d_samples: u32,
    /// 输入侧 FIR 声明支撑 L_A（>= 1）。
    pub l_a: u16,
    /// 低带矩阵声明支撑 L_G（>= D+1）。
    pub l_g: u16,
    /// 输出对角 FIR 声明支撑 L_P（>= 1）。
    pub l_p: u16,
    /// 低带 participants 数 M（<= 8）。
    pub m: u16,
    /// 角色到源选择（`DIRECT_NO_SOURCE` = 无源；A_map 行至多一个 1）。
    pub src_of_role: [u16; DIRECT_MAX_ROLES],
    /// G 的 dense 低带角色序列（角色索引升序、唯一）。
    ///
    // NOTE(legacy-defect): 旧文档声称“角色索引升序”，但旧 `validate` 只校验角色范围与唯一性，
    // 不校验升序；dense 顺序只影响 g 系数的语义映射与累加顺序（由构造方保证）。
    // 迁移保留旧行为（validate 不新增升序拒绝，避免拒绝旧实现接受的拓扑），仅在此注明。
    pub participants: [u16; DIRECT_MAX_PARTICIPANTS],
    /// 角色到容器通道单射 scatter。
    pub scatter: [u16; DIRECT_MAX_ROLES],
}

impl DirectTopology {
    /// 1x1 identity 基线（N=O=C=1、D=0、g=0、A=P=delta）。
    pub const fn identity() -> Self {
        let mut src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
        src_of_role[0] = 0;
        let mut participants = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
        participants[0] = 0;
        let mut scatter = [0u16; DIRECT_MAX_ROLES];
        scatter[0] = 0;
        Self {
            n_src: 1,
            n_role: 1,
            container_channels: 1,
            d_samples: 0,
            l_a: 1,
            l_g: 1,
            l_p: 1,
            m: 1,
            src_of_role,
            participants,
            scatter,
        }
    }

    /// [NONRT] 校验拓扑（旧 SPEC 与容量约束）；OK 或固定错误码。
    ///
    /// 校验项：数量/容量、L_G >= D+1、L_F = L_A + L_G + L_P - 2 <= 4096、participants 角色
    /// 范围与唯一、A_map 每源至多被一行选中、scatter 范围与单射、C >= O。
    ///
    // NOTE(legacy-defect): 旧实现不校验 `participants` 升序（见字段文档）；保留旧行为。
    pub fn validate(&self) -> RtStatus {
        self.validate_with_fused_taps(DIRECT_MAX_TAPS)
    }

    /// [NONRT] 引擎相关容量校验：与 `validate` 同一套拓扑检查，仅把融合支撑上限
    /// `L_F = L_A + L_G + L_P - 2` 参数化（不改系数语义）。
    ///
    /// direct 引擎用 `DIRECT_MAX_TAPS`（4096，逐位直算上限）；partitioned 引擎可承载
    /// extended profile 的 `crate::kernels::TAPS_EXTENDED_MAX`（16384；B=64 时 Q = 256）。
    pub fn validate_with_fused_taps(&self, max_fused_taps: usize) -> RtStatus {
        if self.n_src == 0 || self.n_src as usize > DIRECT_MAX_SOURCES {
            return RtStatus::ERR_CHANNELS;
        }
        if self.n_role == 0 || self.n_role as usize > DIRECT_MAX_ROLES {
            return RtStatus::ERR_ROLES;
        }
        if self.container_channels == 0
            || self.container_channels as usize > DIRECT_MAX_CONTAINERS
            || self.container_channels < self.n_role
        {
            return RtStatus::ERR_CHANNELS;
        }
        if self.l_a == 0 || self.l_g == 0 || self.l_p == 0 {
            return RtStatus::ERR_TAPS;
        }
        if self.l_a as usize + self.l_g as usize + self.l_p as usize - 2 > max_fused_taps {
            return RtStatus::ERR_TAPS;
        }
        if self.l_g as u64 <= self.d_samples as u64 {
            return RtStatus::ERR_TAPS;
        }
        if self.m == 0 || self.m as usize > DIRECT_MAX_PARTICIPANTS || self.m > self.n_role {
            return RtStatus::ERR_ROLES;
        }

        let n_src = self.n_src as usize;
        let n_role = self.n_role as usize;
        let container = self.container_channels as usize;
        let m = self.m as usize;

        // participants：范围 + 唯一。
        let mut a = 0;
        while a < m {
            let role = self.participants[a];
            if role as usize >= n_role {
                return RtStatus::ERR_ROLES;
            }
            let mut b = 0;
            while b < a {
                if self.participants[b] == role {
                    return RtStatus::ERR_ROLES;
                }
                b += 1;
            }
            a += 1;
        }

        // A_map：源范围 + 每个源至多被一个角色行选中（只选择/零填，无复制/分发）。
        let mut src_used = [false; DIRECT_MAX_SOURCES];
        let mut j = 0;
        while j < n_role {
            let src = self.src_of_role[j];
            if src != DIRECT_NO_SOURCE {
                if src as usize >= n_src {
                    return RtStatus::ERR_ROLES;
                }
                let slot = src as usize;
                if src_used[slot] {
                    return RtStatus::ERR_ROLES;
                }
                src_used[slot] = true;
            }
            j += 1;
        }

        // scatter：容器范围 + 单射。
        let mut j = 0;
        while j < n_role {
            let c = self.scatter[j];
            if c as usize >= container {
                return RtStatus::ERR_ROLES;
            }
            let mut k = 0;
            while k < j {
                if self.scatter[k] == c {
                    return RtStatus::ERR_ROLES;
                }
                k += 1;
            }
            j += 1;
        }

        RtStatus::OK
    }
}

/// [NONRT] 直算内核固定缓冲布局（Copy/POD；偏移/容量以“样本数”计）。
///
/// 缓冲分区全部在一条 state 切片内：`x_past`（每源 L_A-1 历史）、`u_past`（每角色 L_G-1）、
/// `v_past`（每角色 L_P-1），以及 block 容量为 `DIRECT_MAX_BLOCK` 的 `x_cur/u_cur/v_cur`
/// 当前块中间量。历史约定：`x_past[i][t] = x_i[block_start-1-t]`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectLayout {
    /// 源通道数（= `DirectTopology.n_src`）。
    pub n_src: u16,
    /// 输出角色数（= `DirectTopology.n_role`）。
    pub n_role: u16,
    /// 容器通道数（= `DirectTopology.container_channels`）。
    pub container_channels: u16,
    /// 输入侧 FIR 支撑。
    pub l_a: u16,
    /// G 支撑。
    pub l_g: u16,
    /// 输出对角 FIR 支撑。
    pub l_p: u16,
    /// 公共 baseline 延迟。
    pub d_samples: u32,
    /// x_past 分区起始样本偏移。
    pub x_past_off: u32,
    /// u_past 分区起始样本偏移。
    pub u_past_off: u32,
    /// v_past 分区起始样本偏移。
    pub v_past_off: u32,
    /// x_cur 分区起始样本偏移。
    pub x_cur_off: u32,
    /// u_cur 分区起始样本偏移。
    pub u_cur_off: u32,
    /// v_cur 分区起始样本偏移。
    pub v_cur_off: u32,
    /// 需要的 state 样本总数。
    pub state_samples: u32,
}

impl DirectLayout {
    /// [NONRT] 由拓扑建立固定缓冲布局（校验拓扑；结果与拓扑一一对应）。
    pub fn build(topo: &DirectTopology) -> Result<Self, RtStatus> {
        let status = topo.validate();
        if !status.is_ok() {
            return Err(status);
        }
        let n_src = topo.n_src as u64;
        let n_role = topo.n_role as u64;
        let x_past = n_src * (topo.l_a as u64 - 1);
        let u_past = n_role * (topo.l_g as u64 - 1);
        let v_past = n_role * (topo.l_p as u64 - 1);
        let block = DIRECT_MAX_BLOCK as u64;
        let x_cur = n_src * block;
        let u_cur = n_role * block;
        let v_cur = n_role * block;
        let x_past_off = 0u64;
        let u_past_off = x_past_off + x_past;
        let v_past_off = u_past_off + u_past;
        let x_cur_off = v_past_off + v_past;
        let u_cur_off = x_cur_off + x_cur;
        let v_cur_off = u_cur_off + u_cur;
        let total = v_cur_off + v_cur;
        if total > u32::MAX as u64 {
            return Err(RtStatus::ERR_CAPACITY);
        }
        Ok(Self {
            n_src: topo.n_src,
            n_role: topo.n_role,
            container_channels: topo.container_channels,
            l_a: topo.l_a,
            l_g: topo.l_g,
            l_p: topo.l_p,
            d_samples: topo.d_samples,
            x_past_off: x_past_off as u32,
            u_past_off: u_past_off as u32,
            v_past_off: v_past_off as u32,
            x_cur_off: x_cur_off as u32,
            u_cur_off: u_cur_off as u32,
            v_cur_off: v_cur_off as u32,
            state_samples: total as u32,
        })
    }

    /// 需要的 state 样本总数（usize 视图；调用方按此长度提供并清零 state）。
    pub const fn state_samples(&self) -> usize {
        self.state_samples as usize
    }
}

// ---------------------------------------------------------------------------
// 系数视图（借用；[NONRT] 构造，RT 只读）
// ---------------------------------------------------------------------------

/// f32 系数借用视图（[RT] 生产路径；对应数组为行主序、按声明支撑零填充）。
#[derive(Clone, Copy, Debug)]
pub struct DirectCoeffsF32<'a> {
    /// 输入侧 FIR：`n_src * l_a`，布局 `[src][tap]`。
    pub a_fir: &'a [f32],
    /// 低带 g：`m * m * l_g`，布局 `[dense_row][dense_col][tap]`（不含 delta_D baseline）。
    pub g: &'a [f32],
    /// 输出对角 FIR：`n_role * l_p`，布局 `[role][tap]`。
    pub p: &'a [f32],
}

/// f64 系数借用视图（[TEST]/offline oracle；不进入生产 RT 调用图）。
#[derive(Clone, Copy, Debug)]
pub struct DirectCoeffsF64<'a> {
    /// 输入侧 FIR：`n_src * l_a`。
    pub a_fir: &'a [f64],
    /// 低带 g：`m * m * l_g`。
    pub g: &'a [f64],
    /// 输出对角 FIR：`n_role * l_p`。
    pub p: &'a [f64],
}

/// 私有标量抽象（泛型只在模块内部，公开入口不暴露泛型）。
trait DirectSample: Copy {
    const ZERO: Self;
    const ONE: Self;
    fn add(self, other: Self) -> Self;
    fn mul(self, other: Self) -> Self;
}

impl DirectSample for f32 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }
}

impl DirectSample for f64 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }
}

/// 私有系数视图（泛型内部形态）。
struct CoeffSlices<'a, T> {
    a_fir: &'a [T],
    g: &'a [T],
    p: &'a [T],
}

// ---------------------------------------------------------------------------
// [RT] 生产入口（f32）与 [TEST] oracle 入口（f64）
// ---------------------------------------------------------------------------

/// [RT] 直算一个 block：planar f32 输入到 planar f32 输出（固定缓冲，无分配）。
///
/// 参数：topo/layout 由 [NONRT] 一次构造并校验；coeffs 借用 plan arena 系数；state 为布局
/// 给出长度的可写固定缓冲（首次使用前必须清零，跨 block 保持）；input/output 为 planar
/// 缓冲，in_stride/out_stride 为每通道样本步长（调用方保证 >= block_size；生产准入还要求
/// 16 样本倍数/64B 对齐，属 host 层校验）。
///
/// `valid_frames < block_size` 时只处理前 valid_frames 个样本：输出 `[valid_frames, block_size)`
/// 写 +0.0；历史按 valid_frames 个有效样本推进，因此 partial 块可拼接（两个 V=2 块等价一个
/// V=4 块）；`valid_frames == 0` 只清零输出、不推进历史（不是 drain reset）。
#[allow(clippy::too_many_arguments)]
pub fn direct_block_f32(
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
    let view = CoeffSlices {
        a_fir: coeffs.a_fir,
        g: coeffs.g,
        p: coeffs.p,
    };
    direct_block_scalar(
        topo,
        layout,
        &view,
        state,
        input,
        in_stride,
        output,
        out_stride,
        block_size,
        valid_frames,
    )
}

/// [TEST]/offline oracle 直算一个 block（f64；与旧 reference/夹具对拍用）。
///
/// 语义与 `direct_block_f32` 完全相同（含 partial/flush 契约，见文件头），只换标量精度。
/// f64 不进入生产 RT 调用图，仅供 offline/oracle 与夹具验收。
#[allow(clippy::too_many_arguments)]
pub fn direct_block_f64(
    topo: &DirectTopology,
    layout: &DirectLayout,
    coeffs: &DirectCoeffsF64<'_>,
    state: &mut [f64],
    input: &[f64],
    in_stride: usize,
    output: &mut [f64],
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
) -> RtStatus {
    let view = CoeffSlices {
        a_fir: coeffs.a_fir,
        g: coeffs.g,
        p: coeffs.p,
    };
    direct_block_scalar(
        topo,
        layout,
        &view,
        state,
        input,
        in_stride,
        output,
        out_stride,
        block_size,
        valid_frames,
    )
}

/// [NONRT] 把 state 缓冲清零（新 epoch / DRAIN_TAIL 终止后的 epoch 起点 / 首块前调用；
/// 与 partitioned 终止性 DRAIN 的行为对齐；只写固定缓冲，无分配）。
pub fn direct_state_zero_f32(state: &mut [f32]) {
    for value in state.iter_mut() {
        *value = 0.0;
    }
}

/// [NONRT] 把 f64 oracle state 缓冲清零（测试/offline 用；partial/DRAIN 语义同 f32）。
pub fn direct_state_zero_f64(state: &mut [f64]) {
    for value in state.iter_mut() {
        *value = 0.0;
    }
}

// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn direct_block_scalar<T: DirectSample>(
    topo: &DirectTopology,
    layout: &DirectLayout,
    coeffs: &CoeffSlices<'_, T>,
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

    // 输出块先整体清零：未映射容器通道/valid_frames 之后的样本必须逐样本严格零。
    let mut c = 0;
    while c < containers {
        let base = c * out_stride;
        let mut n = 0;
        while n < block_size {
            output[base + n] = T::ZERO;
            n += 1;
        }
        c += 1;
    }
    if valid_frames == 0 {
        return RtStatus::OK;
    }

    // 角色到 dense participant 位置（非 participant 为 DIRECT_NO_SOURCE）。
    let mut part_pos = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
    let mut a_idx = 0;
    while a_idx < m {
        part_pos[topo.participants[a_idx] as usize] = a_idx as u16;
        a_idx += 1;
    }

    // 阶段 0：把当前块输入拷入 x_cur 固定区（u 阶段与 past 更新共用）。
    let mut i = 0;
    while i < n_src {
        let in_base = i * in_stride;
        let cur_base = x_cur_off + i * block;
        let mut n = 0;
        while n < valid_frames {
            state[cur_base + n] = input[in_base + n];
            n += 1;
        }
        i += 1;
    }

    // 阶段 1：u_j = a_i * x_i（每角色一列；A_map 只选择/零填）。
    let mut j = 0;
    while j < n_role {
        let u_base = u_cur_off + j * block;
        let src = topo.src_of_role[j];
        if src == DIRECT_NO_SOURCE {
            let mut n = 0;
            while n < valid_frames {
                state[u_base + n] = T::ZERO;
                n += 1;
            }
            j += 1;
            continue;
        }
        let si = src as usize;
        let a_base = si * l_a;
        let xp_base = x_past_off + si * x_past_len;
        let xc_base = x_cur_off + si * block;
        let mut acc = [T::ZERO; DIRECT_MAX_BLOCK];
        let c0 = coeffs.a_fir[a_base];
        let mut n = 0;
        while n < valid_frames {
            acc[n] = c0.mul(state[xc_base + n]);
            n += 1;
        }
        let mut t = 1;
        while t < l_a {
            let coef = coeffs.a_fir[a_base + t];
            let lim = if t < valid_frames { t } else { valid_frames };
            let mut n = 0;
            while n < lim {
                acc[n] = acc[n].add(coef.mul(state[xp_base + t - n - 1]));
                n += 1;
            }
            let mut n = t;
            while n < valid_frames {
                acc[n] = acc[n].add(coef.mul(state[xc_base + n - t]));
                n += 1;
            }
            t += 1;
        }
        let mut n = 0;
        while n < valid_frames {
            state[u_base + n] = acc[n];
            n += 1;
        }
        j += 1;
    }

    // 阶段 2：v_j = sum_k G[j][k] * u_k，G 含 identity baseline（D 计入支撑）。
    let mut j = 0;
    while j < n_role {
        let j_pos = part_pos[j];
        let mut acc = [T::ZERO; DIRECT_MAX_BLOCK];
        let mut k = 0;
        while k < n_role {
            let k_pos = part_pos[k];
            let has_g = j_pos != DIRECT_NO_SOURCE && k_pos != DIRECT_NO_SOURCE;
            let g_base = if has_g {
                (j_pos as usize * m + k_pos as usize) * l_g
            } else {
                0
            };
            let u_cur_k = u_cur_off + k * block;
            let u_past_k = u_past_off + k * u_past_len;
            let mut t = 0;
            while t < l_g {
                // coef = (j==k && t==D ? 1 : 0) + g[pos(j)][pos(k)][t]
                let mut coef = T::ZERO;
                if j == k && t == d {
                    coef = coef.add(T::ONE);
                }
                if has_g {
                    coef = coef.add(coeffs.g[g_base + t]);
                }
                if t == 0 {
                    let mut n = 0;
                    while n < valid_frames {
                        acc[n] = acc[n].add(coef.mul(state[u_cur_k + n]));
                        n += 1;
                    }
                } else {
                    let lim = if t < valid_frames { t } else { valid_frames };
                    let mut n = 0;
                    while n < lim {
                        acc[n] = acc[n].add(coef.mul(state[u_past_k + t - n - 1]));
                        n += 1;
                    }
                    let mut n = t;
                    while n < valid_frames {
                        acc[n] = acc[n].add(coef.mul(state[u_cur_k + n - t]));
                        n += 1;
                    }
                }
                t += 1;
            }
            k += 1;
        }
        let v_base = v_cur_off + j * block;
        let mut n = 0;
        while n < valid_frames {
            state[v_base + n] = acc[n];
            n += 1;
        }
        j += 1;
    }

    // 阶段 3：y_j = p_j * v_j，写 scatter 容器通道。
    let mut j = 0;
    while j < n_role {
        let p_base = j * l_p;
        let vc_base = v_cur_off + j * block;
        let vp_base = v_past_off + j * v_past_len;
        let mut acc = [T::ZERO; DIRECT_MAX_BLOCK];
        let c0 = coeffs.p[p_base];
        let mut n = 0;
        while n < valid_frames {
            acc[n] = c0.mul(state[vc_base + n]);
            n += 1;
        }
        let mut t = 1;
        while t < l_p {
            let coef = coeffs.p[p_base + t];
            let lim = if t < valid_frames { t } else { valid_frames };
            let mut n = 0;
            while n < lim {
                acc[n] = acc[n].add(coef.mul(state[vp_base + t - n - 1]));
                n += 1;
            }
            let mut n = t;
            while n < valid_frames {
                acc[n] = acc[n].add(coef.mul(state[vc_base + n - t]));
                n += 1;
            }
            t += 1;
        }
        let out_base = topo.scatter[j] as usize * out_stride;
        let mut n = 0;
        while n < valid_frames {
            output[out_base + n] = acc[n];
            n += 1;
        }
        j += 1;
    }

    // 阶段 4：历史更新（每 block 一次 O(taps)，不做逐样本移位）。
    // 约定 x_past[i][t] = x_i[block_start-1-t]；u_past/v_past 同理。
    let mut i = 0;
    while i < n_src {
        let past_base = x_past_off + i * x_past_len;
        let cur_base = x_cur_off + i * block;
        let mut t = x_past_len;
        while t > 0 {
            t -= 1;
            let value = if t < valid_frames {
                state[cur_base + valid_frames - 1 - t]
            } else {
                state[past_base + (t - valid_frames)]
            };
            state[past_base + t] = value;
        }
        i += 1;
    }
    let mut j = 0;
    while j < n_role {
        let past_base = u_past_off + j * u_past_len;
        let cur_base = u_cur_off + j * block;
        let mut t = u_past_len;
        while t > 0 {
            t -= 1;
            let value = if t < valid_frames {
                state[cur_base + valid_frames - 1 - t]
            } else {
                state[past_base + (t - valid_frames)]
            };
            state[past_base + t] = value;
        }
        j += 1;
    }
    let mut j = 0;
    while j < n_role {
        let past_base = v_past_off + j * v_past_len;
        let cur_base = v_cur_off + j * block;
        let mut t = v_past_len;
        while t > 0 {
            t -= 1;
            let value = if t < valid_frames {
                state[cur_base + valid_frames - 1 - t]
            } else {
                state[past_base + (t - valid_frames)]
            };
            state[past_base + t] = value;
        }
        j += 1;
    }

    RtStatus::OK
}

/// [RT] 入口参数校验（只读标量/长度，不写缓冲）；任一不符返回固定错误码。
///
// NOTE(legacy-defect): 旧实现每个 block 在 RT 路径重跑一次完整拓扑校验 + 布局重算
// （O(taps) 的循环），保留旧行为（不引入缓存/一致性状态），仅记录 RT 时间开销。
#[allow(clippy::too_many_arguments)]
fn validate_call(
    topo: &DirectTopology,
    layout: &DirectLayout,
    coeffs_a_len: usize,
    coeffs_g_len: usize,
    coeffs_p_len: usize,
    state_len: usize,
    input_len: usize,
    in_stride: usize,
    output_len: usize,
    out_stride: usize,
    block_size: usize,
    valid_frames: usize,
) -> RtStatus {
    let status = topo.validate();
    if !status.is_ok() {
        return status;
    }
    let expected = match DirectLayout::build(topo) {
        Ok(expected) => expected,
        Err(err) => return err,
    };
    if *layout != expected {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    let n_src = topo.n_src as usize;
    let n_role = topo.n_role as usize;
    let containers = topo.container_channels as usize;
    let m = topo.m as usize;
    let l_a = topo.l_a as usize;
    let l_g = topo.l_g as usize;
    let l_p = topo.l_p as usize;
    if coeffs_a_len != n_src * l_a || coeffs_g_len != m * m * l_g || coeffs_p_len != n_role * l_p {
        return RtStatus::ERR_PATH_TABLE;
    }
    if state_len < layout.state_samples() {
        return RtStatus::ERR_CAPACITY;
    }
    if block_size == 0 || block_size > DIRECT_MAX_BLOCK {
        return RtStatus::ERR_BLOCK_SIZE;
    }
    if valid_frames > block_size {
        return RtStatus::ERR_BAD_LAYOUT;
    }
    if in_stride < block_size || out_stride < block_size {
        return RtStatus::ERR_STRIDE_ALIGN;
    }
    let need_in = match n_src.checked_mul(in_stride) {
        Some(need) => need,
        None => return RtStatus::ERR_CAPACITY,
    };
    let need_out = match containers.checked_mul(out_stride) {
        Some(need) => need,
        None => return RtStatus::ERR_CAPACITY,
    };
    if input_len < need_in || output_len < need_out {
        return RtStatus::ERR_CAPACITY;
    }
    RtStatus::OK
}

// ---------------------------------------------------------------------------
// 编译期静态断言（needs_drop 与旧容量绑定）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(DIRECT_MAX_SOURCES == 32);
    assert!(DIRECT_MAX_ROLES == 22);
    assert!(DIRECT_MAX_ROLES <= DIRECT_MAX_CONTAINERS);
    assert!(DIRECT_MAX_CONTAINERS == crate::rt::MAX_OUTPUT_CHANNELS);
    assert!(DIRECT_MAX_PARTICIPANTS == crate::rt::MAX_LOW_BAND_PARTICIPANTS);
    assert!(DIRECT_MAX_TAPS as u32 == crate::rt::TAPS_NORMAL_MAX);
    assert!(DIRECT_MAX_BLOCK == crate::rt::BLOCK_SIZES[1] as usize);
    assert!(!core::mem::needs_drop::<DirectTopology>());
    assert!(!core::mem::needs_drop::<DirectLayout>());
    assert!(!core::mem::needs_drop::<DirectCoeffsF32<'static>>());
    assert!(!core::mem::needs_drop::<DirectCoeffsF64<'static>>());
};
