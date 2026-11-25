//! [RT] MIMO 12 kHz 低频修正支路（抽取 → G12 → 插值 → 与直通支路对齐）。
//!
//! 对应 docs/MIMO-COEFF-PLAN.md §1.2/§4.2 的修正支路拓扑：
//!
//! ~~~text
//! 直通支路:  x_i ─────────────────────────────── z^-Δsys ─┐
//! 修正支路:  x_i ─↓4 (抗混叠 FIR) ─ G12[MxM, 12 kHz] ─↑4 (插值 FIR, gain 4) ─ (+) ─ y_j
//! out_j[n] = x_j[n-Δsys] + Σ_i (h_int * 4↑(g_ji * (h_dec * x_i)))[n]
//! ~~~
//!
//! 12 kHz 域 G 的抽头上限是 24000 点（2.0 s）。每条设计实际用多长，由噪声底以上的尾巴决定。
//! 直通引擎的 4096 点上限不在本模块里。本模块只实现低速率修正支路，直通支路由 host/plan
//! 图按 Δsys 延迟后相加（plan 形式里的 δ_D 由直通支路承担，本模块卷积只跑修正量 g）。
//!
//! 12 kHz 分区卷积复用 T08 的 UPOLS 内核（crates/core/src/partitioned.rs）：
//! - 12 kHz 采样看作“样本域”即可，内核与采样率无关；
//! - 拓扑取 m×m participants、A=P=identity（L_A=L_P=1）、L_G=taps、D=0，
//!   并把设计 g 的对角 tap0 减 1（g_param = g - δ_0 I），使内核融合后的 G_full 恰为设计 g
//!   （见 partitioned.rs 的 delta_D baseline 语义）；因此**不新增第二套 FFT/卷积实现**。
//! - 低速率块 low_block = block48 / 4 ∈ {16,32}（B128/B64），分区数 Q = ceil(taps/low_block)
//!   ≤ 1500。24000 点在块长 64 时 Q = 1500，块长 128 时 Q = 750。
//!   FFT K = next_pow2(2*low_block) ≤ 256。
//!
//! 延迟/相位对齐：[RT] 修正支路固定延迟 = decim 群延迟 + 4*D12 + interp 群延迟（全部为整数
//! 48 kHz 样本，因为抽取/插值 FIR 都取奇数长度）。设计产物把 D12（M2 预延迟，10 ms 级）写进
//! g 的目标时移，本模块用 layout.delay48 报告 Δsys，host 必须把直通支路延迟同一个 Δsys；
//! 单元测试用脉冲响应锁定该延迟（误差 0 样本，前回声 ≤ -60 dB）。
//!
//! 工作区约束（WBS T06/T08）：[RT] process_block 不分配/不加锁/不 syscall/不 panic 分支；
//! plan/state/系数全部在 [NONRT] build 期一次分配，RT 只借用固定切片。partial 块不支持
//! （valid_frames 必须 == block48；流末 flush 由调用方补零后走完整块，或另起 epoch reset）。

use crate::direct::{DirectCoeffsF32, DirectTopology, DIRECT_MAX_PARTICIPANTS, DIRECT_NO_SOURCE};
use crate::partitioned::{
    partitioned_block_f32, partitioned_state_zero_f32, PartitionedLayout, PartitionedPlan,
    PARTITIONED_MAX_BLOCK,
};
use crate::rt::RtStatus;

/// 低速率域采样率（Hz）：48 kHz ÷ 4 = 12 kHz（96 kHz 时同理 ÷ 8，本版只开 48k）。
pub const MIMO12K_LOWRATE_HZ: u32 = 12_000;
/// 抽取/插值倍数（48 kHz -> 12 kHz）。
pub const MIMO12K_GROUP: usize = 4;
/// participants 上限（= core-v1 MAX_LOW_BAND_PARTICIPANTS；P1 实际 5）。
pub const MIMO12K_MAX_PARTICIPANTS: usize = DIRECT_MAX_PARTICIPANTS;
/// 每路 12 kHz G 修正 FIR 支撑上限。24000 / 12 kHz = 2.0 s。
/// 直通引擎 `DIRECT_MAX_TAPS` 仍是 4096。
pub const MIMO12K_MAX_TAPS: usize = 24_000;
/// 抽取/插值 FIR 抽头上限（奇数；实测设计 95 taps -> 群延迟 47 @48k）。
pub const MIMO12K_MAX_RESAMP_TAPS: usize = 255;
/// 48 kHz 块长上限（生产 B∈{64,128}）。
pub const MIMO12K_MAX_BLOCK48: usize = 128;
/// 生产 48 kHz 块长（64/128 均支持；low_block = 16/32）。
pub const MIMO12K_BLOCK48S: [usize; 2] = [64, 128];
/// [NONRT] 12 kHz 修正支路布局（Copy/POD；尺寸/延迟/容量自洽）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mimo12kLayout {
    /// participants 数 M（G12 为 M×M）。
    pub m: u16,
    /// 每路 12 kHz G 修正 FIR 支撑（= L_G，样本 @12k）。
    pub taps: u32,
    /// 48 kHz 块长 B48。
    pub block48: u16,
    /// 12 kHz 块长 B12 = B48/4。
    pub low_block: u16,
    /// 抽取/插值 FIR 抽头数（同一个对称低通）。
    pub resamp_taps: u16,
    /// 抽取 FIR 群延迟（48 kHz 样本，(N-1)/2）。
    pub decim_delay: u16,
    /// 插值 FIR 群延迟（48 kHz 样本，(N-1)/2）。
    pub interp_delay: u16,
    /// 设计目标时移 D12（12 kHz 样本，M2 预延迟；写进 g 的目标对齐）。
    pub predelay_low: u16,
    /// 修正支路总延迟 Δsys（48 kHz 样本）= decim_delay + 4*D12 + interp_delay。
    pub delay48: u32,
    /// T08 共享 FFT 长度 K = next_pow2(2*B12)。
    pub fft_size: u16,
    /// 每路分区数 Q = ceil(taps/B12)。
    pub partitions: u16,
    /// 融合路由数（= M*M）。
    pub n_routes: u16,
    /// T08 plan 缓冲样本数。
    pub conv_plan_samples: u32,
    /// T08 state 缓冲样本数。
    pub conv_state_samples: u32,
}

impl Mimo12kLayout {
    /// [NONRT] 由 (M, taps, block48, resamp_taps, predelay_low) 建立布局并校验容量。
    pub fn build(
        m: usize,
        taps: usize,
        block48: usize,
        resamp_taps: usize,
        predelay_low: usize,
    ) -> Result<Self, RtStatus> {
        if m == 0 || m > MIMO12K_MAX_PARTICIPANTS {
            return Err(RtStatus::ERR_CHANNELS);
        }
        if taps == 0 || taps > MIMO12K_MAX_TAPS {
            return Err(RtStatus::ERR_TAPS);
        }
        if block48 < MIMO12K_GROUP || block48 > MIMO12K_MAX_BLOCK48 || block48 % MIMO12K_GROUP != 0
        {
            return Err(RtStatus::ERR_BLOCK_SIZE);
        }
        if predelay_low >= taps {
            return Err(RtStatus::ERR_TAPS);
        }
        if resamp_taps < 3 || resamp_taps > MIMO12K_MAX_RESAMP_TAPS || resamp_taps % 2 == 0 {
            return Err(RtStatus::ERR_TAPS);
        }
        let low_block = block48 / MIMO12K_GROUP;
        if low_block == 0 || low_block > PARTITIONED_MAX_BLOCK {
            return Err(RtStatus::ERR_BLOCK_SIZE);
        }
        let topo = conv_topology(m, taps)?;
        let conv = PartitionedLayout::build_with_fused_taps(&topo, low_block, MIMO12K_MAX_TAPS)?;
        let delay = (resamp_taps - 1) / 2;
        let delay48 = delay as u32 * 2 + MIMO12K_GROUP as u32 * predelay_low as u32;
        Ok(Self {
            m: m as u16,
            taps: taps as u32,
            block48: block48 as u16,
            low_block: low_block as u16,
            resamp_taps: resamp_taps as u16,
            decim_delay: delay as u16,
            interp_delay: delay as u16,
            predelay_low: predelay_low as u16,
            delay48,
            fft_size: conv.fft_size,
            partitions: conv.partitions,
            n_routes: conv.n_routes,
            conv_plan_samples: conv.plan_samples,
            conv_state_samples: conv.state_samples,
        })
    }

    /// 12 kHz 修正支路总延迟（48 kHz 样本）。
    pub const fn delay48(&self) -> u32 {
        self.delay48
    }

    /// 12 kHz 修正支路总延迟（ms）。
    pub fn delay_ms(&self) -> f64 {
        self.delay48 as f64 * 1000.0 / 48_000.0
    }

    /// 12 kHz G 的时间跨度（ms）。
    pub fn g_span_ms(&self) -> f64 {
        self.taps as f64 * 1000.0 / MIMO12K_LOWRATE_HZ as f64
    }

    /// 12 kHz 修正量有效修正支撑（去掉 predelay 后，ms）。
    pub fn correction_span_ms(&self) -> f64 {
        (self.taps.saturating_sub(self.predelay_low as u32)) as f64 * 1000.0
            / MIMO12K_LOWRATE_HZ as f64
    }

    /// 每 48 kHz 块的处理成本模型（不做实测；实测见 tests/mimo12k）。
    pub fn budget(&self) -> Mimo12kBudget {
        let m = self.m as u64;
        let taps = self.taps as u64;
        let low_block = self.low_block as u64;
        let fft = self.fft_size as u64;
        let parts = self.partitions as u64;
        let routes = self.n_routes as u64;
        let resamp = self.resamp_taps as u64;
        let fs48 = 48_000.0f64;
        let fs12 = MIMO12K_LOWRATE_HZ as f64;
        // T08 UPOLS：每低速率块 = M 次输入 FFT + 每路由 Q 个分区 * K 个复数 bin 的频域 MAC
        // + M 次 IFFT；复数 MAC = 4 乘 + 4 加 = 8 flops。
        let complex_macs = routes * parts * fft;
        let conv_flops_per_block = (complex_macs * 8) as f64
            + 2.0 * m as f64 * (fft as f64) * (fft as f64).log2() * 5.0;
        let low_blocks_per_sec = fs12 / low_block as f64;
        let conv_flops_per_sec = conv_flops_per_block * low_blocks_per_sec;
        let decim_macs_per_sec = m as f64 * resamp as f64 * fs12;
        let interp_macs_per_sec = m as f64 * resamp as f64 * fs48;
        let total_macs_per_sec =
            conv_flops_per_sec / 2.0 + decim_macs_per_sec + interp_macs_per_sec;
        // 被替代的 48 kHz 全速融合路径：M*M*L_G*fs48 MACs/s（G-4096 直算/融合口径）。
        let fullrate_48k_macs_per_sec = (m * m * taps) as f64 * fs48;
        let ratio = if total_macs_per_sec > 0.0 {
            fullrate_48k_macs_per_sec / total_macs_per_sec
        } else {
            0.0
        };
        Mimo12kBudget {
            conv_complex_macs_per_low_block: complex_macs,
            conv_flops_per_sec,
            decim_macs_per_sec,
            interp_macs_per_sec,
            total_macs_per_sec,
            fullrate_48k_macs_per_sec,
            lowrate_vs_fullrate_ratio: ratio,
        }
    }
}

/// [NONRT] 12 kHz 修正支路成本模型（公式估算，不是实测；T46/T30 正式性能门不在此卡）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mimo12kBudget {
    /// 每低速率块的复数 MAC 数（路由×分区×FFT bin）。
    pub conv_complex_macs_per_low_block: u64,
    /// 分区卷积 flops/s（含 IFFT 粗估）。
    pub conv_flops_per_sec: f64,
    /// 抽取 MACs/s。
    pub decim_macs_per_sec: f64,
    /// 插值 MACs/s。
    pub interp_macs_per_sec: f64,
    /// 修正支路总 MACs/s（含卷积等价 MAC 折算）。
    pub total_macs_per_sec: f64,
    /// 被替代的 48 kHz 全速融合 G 路径 MACs/s。
    pub fullrate_48k_macs_per_sec: f64,
    /// 全速 / 12k 修正支路 成本比（>1 表示 12k 更省）。
    pub lowrate_vs_fullrate_ratio: f64,
}

/// [NONRT] 12 kHz MIMO 修正支路运行时（plan/state/历史全部预分配）。
pub struct Mimo12kRuntime {
    layout: Mimo12kLayout,
    conv_topo: DirectTopology,
    conv_plan: PartitionedPlan,
    conv_plan_data: Vec<f32>,
    conv_state: Vec<f32>,
    decim_coeffs: Vec<f32>,
    interp_coeffs: Vec<f32>,
    decim_hist: Vec<f32>,
    interp_hist: Vec<f32>,
    decim_scratch: Vec<f32>,
    interp_scratch: Vec<f32>,
    low_in: Vec<f32>,
    low_out: Vec<f32>,
}

impl Mimo12kRuntime {
    /// [NONRT] 构建运行时：g = 设计修正量（M×M×taps，行主序 j,i,tap，**不含** δ_D baseline），
    /// decim/interp = 48 kHz 抽取/插值低通（同一奇数长度对称 FIR；interp 需按 GROUP 归一以获得
    /// 单位通带增益）。interp 传入的是**已乘以 GROUP** 的插值系数（即 h_int），便于与
    /// MIMO-COEFF-PLAN 的 h_int 定义一致。
    pub fn build(
        layout: Mimo12kLayout,
        g: &[f32],
        decim: &[f32],
        interp: &[f32],
    ) -> Result<Self, RtStatus> {
        let m = layout.m as usize;
        let taps = layout.taps as usize;
        let n = layout.resamp_taps as usize;
        if g.len() != m * m * taps {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if g.iter().any(|v| !v.is_finite()) {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if decim.len() != n || interp.len() != n {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if decim.iter().chain(interp.iter()).any(|v| !v.is_finite()) {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        let topo = conv_topology(m, taps)?;
        let conv = PartitionedLayout::build_extended(&topo, layout.low_block as usize)?;
        let mut plan_data = vec![0.0f32; conv.plan_samples()];
        let mut build_scratch = vec![0.0f32; conv.build_scratch_samples()];
        // 对角 tap0 减 1：内核融合出 G_full = δ_0 I + (g - δ_0 I) = g（只有修正量，无 baseline）。
        let mut g_param = g.to_vec();
        let mut j = 0usize;
        while j < m {
            let idx = (j * m + j) * taps;
            g_param[idx] -= 1.0;
            j += 1;
        }
        let a_fir = vec![1.0f32; m]; // L_A = 1（A = identity）
        let p_fir = vec![1.0f32; m]; // L_P = 1（P = identity）
        let coeffs = DirectCoeffsF32 {
            a_fir: &a_fir,
            g: &g_param,
            p: &p_fir,
        };
        let conv_plan = PartitionedPlan::build_f32_extended(
            &topo,
            layout.low_block as usize,
            &coeffs,
            &mut plan_data,
            &mut build_scratch,
        )?;
        drop(build_scratch);
        let conv_state = vec![0.0f32; conv.state_samples()];
        let decim_hist_len = n - 1;
        let interp_hist_len = (n + MIMO12K_GROUP - 2) / MIMO12K_GROUP;
        let low_block = layout.low_block as usize;
        let block48 = layout.block48 as usize;
        Ok(Self {
            layout,
            conv_topo: topo,
            conv_plan,
            conv_plan_data: plan_data,
            conv_state,
            decim_coeffs: decim.to_vec(),
            interp_coeffs: interp.to_vec(),
            decim_hist: vec![0.0f32; m * decim_hist_len],
            interp_hist: vec![0.0f32; m * interp_hist_len],
            decim_scratch: vec![0.0f32; m * (decim_hist_len + block48)],
            interp_scratch: vec![0.0f32; m * (interp_hist_len + low_block)],
            low_in: vec![0.0f32; m * low_block],
            low_out: vec![0.0f32; m * low_block],
        })
    }

    /// [NONRT] 便捷入口：由布局 + 设计产物直接构建。
    pub fn build_from_design(
        m: usize,
        taps: usize,
        block48: usize,
        resamp_taps: usize,
        predelay_low: usize,
        g: &[f32],
        decim: &[f32],
        interp: &[f32],
    ) -> Result<Self, RtStatus> {
        let layout = Mimo12kLayout::build(m, taps, block48, resamp_taps, predelay_low)?;
        Self::build(layout, g, decim, interp)
    }

    /// 布局。
    pub const fn layout(&self) -> &Mimo12kLayout {
        &self.layout
    }

    /// 修正支路固定延迟（48 kHz 样本；直通支路必须延迟同一值）。
    pub const fn delay48(&self) -> u32 {
        self.layout.delay48
    }

    /// 分区卷积布局（报告/证据用）。
    pub const fn conv_layout(&self) -> &PartitionedLayout {
        &self.conv_plan.layout
    }

    /// runtime 预分配字节估算（plan+state+历史+工作缓冲）。
    pub fn allocated_samples(&self) -> usize {
        self.conv_plan_data.len()
            + self.conv_state.len()
            + self.decim_coeffs.len()
            + self.interp_coeffs.len()
            + self.decim_hist.len()
            + self.interp_hist.len()
            + self.decim_scratch.len()
            + self.interp_scratch.len()
            + self.low_in.len()
            + self.low_out.len()
    }

    /// 抽取+插值低通的单边频响（幅度 dB，20*log10|H|），用于取证/测试（[NONRT]）。
    pub fn resampler_gain_db(&self, freq_hz: f64, combined: bool) -> f64 {
        let df = |coeffs: &[f32]| -> f64 {
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            for (k, c) in coeffs.iter().enumerate() {
                let phase = -2.0 * core::f64::consts::PI * freq_hz * k as f64 / 48_000.0;
                re += *c as f64 * phase.cos();
                im += *c as f64 * phase.sin();
            }
            (re * re + im * im).sqrt()
        };
        let d = df(&self.decim_coeffs);
        let i = df(&self.interp_coeffs);
        if combined {
            // 零插值（zero-stuffing）引入 1/GROUP 的平均增益，插值滤波器按 GROUP 归一 -> 合并增益除 GROUP。
            20.0 * (d * i / MIMO12K_GROUP as f64).log10()
        } else {
            20.0 * d.log10()
        }
    }

    /// 复位全部历史/state（新 epoch；不分配）。
    pub fn reset(&mut self) {
        partitioned_state_zero_f32(&mut self.conv_state);
        for v in self.decim_hist.iter_mut() {
            *v = 0.0;
        }
        for v in self.interp_hist.iter_mut() {
            *v = 0.0;
        }
        for v in self.low_in.iter_mut() {
            *v = 0.0;
        }
        for v in self.low_out.iter_mut() {
            *v = 0.0;
        }
    }

    /// [RT] 处理一个 48 kHz 块：输入 M 路 feeds48（平面，步长 in_stride），输出修正量
    /// corr48（平面，步长 out_stride）。只做修正支路；直通 δ_Δsys 由调用方相加。
    ///
    /// 契约：valid_frames 必须 == block48（不支持 partial；流末补零后作为完整块处理）。
    /// 不分配、不加锁、不 syscall；输出 [0, block48) 全部有效。
    pub fn process_block_f32(
        &mut self,
        feeds48: &[f32],
        in_stride: usize,
        corr48: &mut [f32],
        out_stride: usize,
        valid_frames: usize,
    ) -> RtStatus {
        let m = self.layout.m as usize;
        let block48 = self.layout.block48 as usize;
        let low_block = self.layout.low_block as usize;
        let n = self.layout.resamp_taps as usize;
        if valid_frames != block48 {
            return RtStatus::ERR_BLOCK_SIZE;
        }
        if in_stride < block48 || out_stride < block48 {
            return RtStatus::ERR_STRIDE_ALIGN;
        }
        if feeds48.len() < (m - 1) * in_stride + block48
            || corr48.len() < (m - 1) * out_stride + block48
        {
            return RtStatus::ERR_CAPACITY;
        }
        let hist_len = n - 1;
        // 1) 抽取：y[mout] = sum_k h_dec[k] * x[4*mout - k]（因果，历史来自 decim_hist）。
        for ch in 0..m {
            let x = &feeds48[ch * in_stride..ch * in_stride + block48];
            let hist = &mut self.decim_hist[ch * hist_len..(ch + 1) * hist_len];
            let low = &mut self.low_in[ch * low_block..(ch + 1) * low_block];
            for (mout, out) in low.iter_mut().enumerate() {
                let n0 = (mout * MIMO12K_GROUP) as isize;
                let mut acc = 0.0f32;
                let mut k = 0usize;
                while k < n {
                    let j = n0 - k as isize;
                    let v = if j < 0 {
                        hist[(hist_len as isize + j) as usize]
                    } else {
                        x[j as usize]
                    };
                    acc += self.decim_coeffs[k] * v;
                    k += 1;
                }
                *out = acc;
            }
            // 滚动更新历史：new_hist = last hist_len of (old_hist ++ x)
            let scratch = &mut self.decim_scratch[ch * (hist_len + block48)..(ch + 1) * (hist_len + block48)];
            scratch[..hist_len].copy_from_slice(hist);
            scratch[hist_len..].copy_from_slice(x);
            let start = hist_len + block48 - hist_len;
            hist.copy_from_slice(&scratch[start..start + hist_len]);
        }
        // 2) 12 kHz 分区卷积（T08 UPOLS；M 输入 / M 输出，融合 G = g）。
        let status = partitioned_block_f32(
            &self.conv_topo,
            &self.conv_plan,
            &self.conv_plan_data,
            &mut self.conv_state,
            &self.low_in,
            low_block,
            &mut self.low_out,
            low_block,
            low_block,
            low_block,
        );
        if !status.is_ok() {
            return status;
        }
        // 3) 插值：corr[n] = sum_k h_int[k] * u[n-k], u[4m] = z[m]，其它为 0。
        let ihl = (n + MIMO12K_GROUP - 2) / MIMO12K_GROUP;
        for ch in 0..m {
            let z = &self.low_out[ch * low_block..(ch + 1) * low_block];
            let hist = &mut self.interp_hist[ch * ihl..(ch + 1) * ihl];
            let out = &mut corr48[ch * out_stride..ch * out_stride + block48];
            for (ni, sample) in out.iter_mut().enumerate() {
                let nn = ni as isize;
                let mut acc = 0.0f32;
                let mut k = 0usize;
                while k < n {
                    let j = nn - k as isize;
                    k += 1;
                    if j.rem_euclid(MIMO12K_GROUP as isize) != 0 {
                        continue;
                    }
                    let zi = j.div_euclid(MIMO12K_GROUP as isize);
                    let v = if zi < 0 {
                        hist[(ihl as isize + zi) as usize]
                    } else {
                        z[zi as usize]
                    };
                    acc += self.interp_coeffs[k - 1] * v;
                }
                *sample = acc;
            }
            // 滚动更新插值历史：new_hist = last ihl of (old_hist ++ z)
            let scratch = &mut self.interp_scratch[ch * (ihl + low_block)..(ch + 1) * (ihl + low_block)];
            scratch[..ihl].copy_from_slice(hist);
            scratch[ihl..].copy_from_slice(z);
            let start = ihl + low_block - ihl;
            hist.copy_from_slice(&scratch[start..start + ihl]);
        }
        RtStatus::OK
    }
}

/// [NONRT] 12 kHz 卷积用 T08 拓扑：M 源 / M 角色 / M 容器，A=P=identity，L_G=taps，D=0。
fn conv_topology(m: usize, taps: usize) -> Result<DirectTopology, RtStatus> {
    let mut src_of_role = [DIRECT_NO_SOURCE; 22];
    let mut participants = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
    let mut scatter = [0u16; 22];
    let mut j = 0usize;
    while j < m {
        src_of_role[j] = j as u16;
        participants[j] = j as u16;
        scatter[j] = j as u16;
        j += 1;
    }
    let topo = DirectTopology {
        n_src: m as u16,
        n_role: m as u16,
        container_channels: m as u16,
        d_samples: 0,
        l_a: 1,
        l_g: taps as u16,
        l_p: 1,
        m: m as u16,
        src_of_role,
        participants,
        scatter,
    };
    let status = topo.validate_with_fused_taps(MIMO12K_MAX_TAPS);
    if status.is_ok() {
        Ok(topo)
    } else {
        Err(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direct::DIRECT_MAX_TAPS;

    #[test]
    fn side_path_24000_keeps_delay_and_direct_stays_4096() {
        assert_eq!(DIRECT_MAX_TAPS, 4096);
        assert_eq!(MIMO12K_MAX_TAPS, 24_000);
        for block in [64usize, 128] {
            let layout = Mimo12kLayout::build(1, 24_000, block, 95, 120).expect("24000 点应能建布局");
            assert_eq!(layout.delay48(), 574, "块长 {block}");
            assert_eq!(layout.taps, 24_000);
        }
        assert!(Mimo12kLayout::build(1, 24_001, 128, 95, 120).is_err());
        let mut wide = conv_topology(1, 24_000).unwrap();
        assert!(!wide.validate().is_ok(), "直通校验仍拒绝 24000 点");
        wide.l_g = 4096;
        assert!(wide.validate().is_ok());
    }
}
