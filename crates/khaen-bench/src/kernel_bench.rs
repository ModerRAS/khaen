//! kernel 级对照 bench（T39a）：同一 workload、同一输入下，标量参考内核 vs 运行时派发的 SIMD 内核。
//!
//! 与 graph 级档位（lib.rs 的 A..E1）不同，这里**绕过 host 图编译**，直接调用
//! `khaen-core` 的内核入口：graph 档位计时的是 PlanGraph::process（host 当前仍接标量
//! direct 入口），kernel 档位回答"内核本身被 SIMD 化后快多少"，并给出两列 p50/p99/min 与
//! 加速比，以及同一输入下两路径输出的最大差异（parity，证明跑的是同一语义）。
//!
//! 档位（默认完整规模）：
//! - `K-direct-crossover`：镜像 host graph/crossover.rs —— lanes 次 1-lane direct 内核调用
//!   （每 lane 一条 config.crossover_taps-tap A-FIR；host 就是这样每 lane 调用一次）；
//! - `K-direct-crossover4096`：同上、taps = DIRECT_MAX_TAPS（4096）；
//! - `K-part-3khz`：3 kHz 低速率域矩阵形态（B = 帧数/factor、M 参与者 m×m G 矩阵）；
//! - `K-part-fft256`：FFT 主导形态（22 lane、L_F=8、Q=1，每块 2×lanes 次 K 点 FFT/IFFT）；
//! - `K-part-1024tap`：中等 taps（L_F=1031、默认 Q=9），FFT + 频域乘加混合负载；
//! - `K-part-4096tap`：partitioned extended 口径（L_F = 4103、Q = 33，镜像校准链 P_post
//!   FIR 融合进 G 的形态）。
//!
//! 规模随 `--lanes/--fir-taps/--matrix-*/--factor` 缩放；两个大档位只在
//! `--fir-taps >= 1024`（即默认完整规模）时启用，避免小配置（含测试）在 debug 构建下过慢。
//!
//! 计时口径与 graph 档位一致：warmup 块丢弃、measure 块逐块 Instant 计时、repeats 次取中位数；
//! 计时器空转开销已扣除；不含 host 图调度/后端开销。

use std::time::Instant;

use khaen_core::kernels::direct::{
    direct_block_f32, DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_BLOCK,
    DIRECT_MAX_TAPS, DIRECT_NO_SOURCE,
};
use khaen_core::kernels::dispatch::{
    direct_block_f32_optimized, partitioned_block_f32_optimized, SimdBackend,
};
use khaen_core::kernels::partitioned::{partitioned_block_f32, PartitionedLayout, PartitionedPlan};

use crate::{
    aggregate, fill_signal, json_string, lowpass_fir, stats_from_samples, BenchConfig, KernelMode,
    TierStats,
};

/// kernel 对照的被测路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KernelPath {
    /// 标量参考内核（direct_block_f32 / partitioned_block_f32）。
    Scalar,
    /// 运行时派发的优化内核（*_optimized）。
    Optimized,
}

/// kernel 级对照档（一档 = 一个 workload × 两列内核统计）。
#[derive(Debug, Clone, PartialEq)]
pub struct KernelTierReport {
    /// 档位名（报告与 JSON 主键）。
    pub name: String,
    /// 该档位要验证什么。
    pub purpose: String,
    /// workload 构造描述（lane 数 / taps / block / FFT 尺寸等）。
    pub workload: String,
    /// 本机派发后端（scalar / avx2+fma / neon）。
    pub backend: String,
    /// 标量参考内核统计（None = 该 `--kernel` 模式下未测）。
    pub scalar: Option<TierStats>,
    /// 优化内核统计（None = 该 `--kernel` 模式下未测）。
    pub optimized: Option<TierStats>,
    /// 加速比 = scalar.p50 / optimized.p50（两列都在时；> 1 表示优化后更快）。
    pub speedup_p50: Option<f64>,
    /// 加速比（p99）。
    pub speedup_p99: Option<f64>,
    /// 加速比（min：共享机器上最接近无争用能力的下界）。
    pub speedup_min: Option<f64>,
    /// 同一输入下两路径输出的最大绝对差异（parity；0 = 逐位一致）。
    pub parity_max_abs: f32,
}

impl KernelTierReport {
    /// 由两列统计构造（自动算加速比）。
    pub(crate) fn from_columns(
        name: &str,
        purpose: String,
        workload: String,
        backend: String,
        scalar: Option<TierStats>,
        optimized: Option<TierStats>,
        parity_max_abs: f32,
    ) -> Self {
        let ratio = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) if b > 0.0 => Some(a / b),
            _ => None,
        };
        let speedup_p50 = ratio(
            scalar.as_ref().map(|stats| stats.p50_us),
            optimized.as_ref().map(|stats| stats.p50_us),
        );
        let speedup_p99 = ratio(
            scalar.as_ref().map(|stats| stats.p99_us),
            optimized.as_ref().map(|stats| stats.p99_us),
        );
        let speedup_min = ratio(
            scalar.as_ref().map(|stats| stats.min_us),
            optimized.as_ref().map(|stats| stats.min_us),
        );
        Self {
            name: name.to_string(),
            purpose,
            workload,
            backend,
            scalar,
            optimized,
            speedup_p50,
            speedup_p99,
            speedup_min,
            parity_max_abs,
        }
    }

    /// 人类可读表的一行（时间单位 µs）。
    pub(crate) fn human_line(&self) -> String {
        let cell = |value: Option<f64>| match value {
            Some(value) => format!("{value:>10.2}"),
            None => format!("{:>10}", "-"),
        };
        let ratio = |value: Option<f64>| match value {
            Some(value) => format!("{value:>8.2}x"),
            None => format!("{:>9}", "-"),
        };
        let stats_cell =
            |stats: &Option<TierStats>, pick: fn(&TierStats) -> f64| cell(stats.as_ref().map(pick));
        format!(
            "{:<26} {} {} {} {} {} {} {} {} {:>10.2e}",
            self.name,
            stats_cell(&self.scalar, |stats| stats.p50_us),
            stats_cell(&self.scalar, |stats| stats.p99_us),
            stats_cell(&self.scalar, |stats| stats.min_us),
            stats_cell(&self.optimized, |stats| stats.p50_us),
            stats_cell(&self.optimized, |stats| stats.p99_us),
            stats_cell(&self.optimized, |stats| stats.min_us),
            ratio(self.speedup_p50),
            ratio(self.speedup_p99),
            self.parity_max_abs,
        )
    }

    /// 机器可读 JSON（手写，schema 与人类可读表一致）。
    pub(crate) fn to_json(&self) -> String {
        let column = |stats: &Option<TierStats>| {
            match stats {
            Some(stats) => format!(
                "{{\"blocks\": {}, \"mean_us\": {:.3}, \"p50_us\": {:.3}, \"p99_us\": {:.3},                  \"p999_us\": {:.3}, \"min_us\": {:.3}, \"max_us\": {:.3},                  \"budget_pct_mean\": {:.3}, \"budget_pct_p99\": {:.3}}}",
                stats.blocks,
                stats.mean_us,
                stats.p50_us,
                stats.p99_us,
                stats.p999_us,
                stats.min_us,
                stats.max_us,
                stats.budget_pct_mean,
                stats.budget_pct_p99
            ),
            None => "null".to_string(),
        }
        };
        let opt_ratio = |value: Option<f64>| match value {
            Some(value) => format!("{value:.3}"),
            None => "null".to_string(),
        };
        format!(
            "{{\"name\": {}, \"purpose\": {}, \"workload\": {}, \"backend\": {},              \"scalar\": {}, \"optimized\": {}, \"speedup_p50\": {}, \"speedup_p99\": {},              \"speedup_min\": {}, \"parity_max_abs\": {:.6e}}}",
            json_string(&self.name),
            json_string(&self.purpose),
            json_string(&self.workload),
            json_string(&self.backend),
            column(&self.scalar),
            column(&self.optimized),
            opt_ratio(self.speedup_p50),
            opt_ratio(self.speedup_p99),
            opt_ratio(self.speedup_min),
            self.parity_max_abs
        )
    }
}

// ===========================================================================
// 档位构造参数（随 BenchConfig 缩放）
// ===========================================================================

/// direct kernel 档：lanes 次 1-lane taps-tap A-FIR（镜像 host graph/crossover.rs）。
struct DirectKernelBench {
    lanes: usize,
    taps: usize,
    block: usize,
    topologies: Vec<DirectTopology>,
    layouts: Vec<DirectLayout>,
    a_fir: Vec<f32>,
    g: Vec<f32>,
    p: Vec<f32>,
    states_scalar: Vec<Vec<f32>>,
    states_optimized: Vec<Vec<f32>>,
    input: Vec<f32>,
    out_scalar: Vec<f32>,
    out_optimized: Vec<f32>,
}

impl DirectKernelBench {
    fn new(lanes: usize, taps: usize, block: usize) -> Self {
        let lanes = lanes.max(1);
        let taps = taps.clamp(1, DIRECT_MAX_TAPS);
        let block = block.clamp(1, DIRECT_MAX_BLOCK);
        // 与 host graph/crossover.rs 相同的系数形状：每 lane 一条低通 A-FIR（g=[0]、p=[1]）。
        let taps_coeffs = lowpass_fir(taps, 0.1);
        let mut a_fir = Vec::with_capacity(lanes * taps);
        for _ in 0..lanes {
            a_fir.extend_from_slice(&taps_coeffs);
        }
        let mut topologies = Vec::with_capacity(lanes);
        let mut layouts = Vec::with_capacity(lanes);
        let mut states_scalar = Vec::with_capacity(lanes);
        let mut states_optimized = Vec::with_capacity(lanes);
        for _ in 0..lanes {
            let mut topo = DirectTopology::identity();
            topo.l_a = taps as u16;
            let layout = DirectLayout::build(&topo).expect("crossover kernel layout");
            states_scalar.push(vec![0.0f32; layout.state_samples()]);
            states_optimized.push(vec![0.0f32; layout.state_samples()]);
            topologies.push(topo);
            layouts.push(layout);
        }
        let mut input = vec![0.0f32; lanes * block];
        fill_signal(&mut input, 0x51ed_0421);
        Self {
            lanes,
            taps,
            block,
            topologies,
            layouts,
            a_fir,
            g: vec![0.0f32; lanes],
            p: vec![1.0f32; lanes],
            states_scalar,
            states_optimized,
            input,
            out_scalar: vec![0.0f32; lanes * block],
            out_optimized: vec![0.0f32; lanes * block],
        }
    }

    fn workload(&self, mirror_lanes: usize) -> String {
        format!(
            "{mirror_lanes} lane × {}-tap 全采样率 A-FIR，block={}（每 lane 一次 1-lane direct 内核，镜像 graph/crossover.rs）",
            self.taps, self.block
        )
    }

    fn reset_states(&mut self) {
        for state in &mut self.states_scalar {
            state.fill(0.0);
        }
        for state in &mut self.states_optimized {
            state.fill(0.0);
        }
    }

    /// 跑 phase 个块（前 warmup 个不计时），返回统计。
    fn measure(
        &mut self,
        path: KernelPath,
        warmup: usize,
        measure: usize,
        budget_us: f64,
        timer_overhead_us: f64,
    ) -> TierStats {
        let Self {
            lanes,
            taps,
            block,
            topologies,
            layouts,
            a_fir,
            g,
            p,
            states_scalar,
            states_optimized,
            input,
            out_scalar,
            out_optimized,
        } = self;
        let states = match path {
            KernelPath::Scalar => states_scalar,
            KernelPath::Optimized => states_optimized,
        };
        let output = match path {
            KernelPath::Scalar => out_scalar,
            KernelPath::Optimized => out_optimized,
        };
        let total = warmup + measure;
        let mut samples = Vec::with_capacity(measure);
        for phase in 0..total {
            let timed = phase >= warmup;
            let start = if timed { Some(Instant::now()) } else { None };
            for lane in 0..*lanes {
                let coeffs = DirectCoeffsF32 {
                    a_fir: &a_fir[lane * *taps..(lane + 1) * *taps],
                    g: &g[lane..lane + 1],
                    p: &p[lane..lane + 1],
                };
                let state = &mut states[lane];
                let inp = &input[lane * *block..(lane + 1) * *block];
                let out = &mut output[lane * *block..(lane + 1) * *block];
                let status = match path {
                    KernelPath::Scalar => direct_block_f32(
                        &topologies[lane],
                        &layouts[lane],
                        &coeffs,
                        state,
                        inp,
                        *block,
                        out,
                        *block,
                        *block,
                        *block,
                    ),
                    KernelPath::Optimized => direct_block_f32_optimized(
                        &topologies[lane],
                        &layouts[lane],
                        &coeffs,
                        state,
                        inp,
                        *block,
                        out,
                        *block,
                        *block,
                        *block,
                    ),
                };
                assert!(status.is_ok(), "kernel bench: direct status {}", status.0);
            }
            if let Some(start) = start {
                let elapsed_us = start.elapsed().as_secs_f64() * 1.0e6 - timer_overhead_us;
                samples.push(elapsed_us.max(0.0));
            }
        }
        // 让编译器不能把输出当死代码消掉。
        std::hint::black_box(output[0]);
        stats_from_samples(samples, budget_us)
    }

    /// parity：两列各自从清零 state 预热 warmup 块后，再跑同一块处理并比较输出最大绝对差异。
    fn parity(&mut self, warmup: usize) -> f32 {
        self.reset_states();
        let warmup = warmup.max(1);
        for _ in 0..warmup {
            let _ = self.measure(KernelPath::Scalar, 0, 1, 1.0, 0.0);
            let _ = self.measure(KernelPath::Optimized, 0, 1, 1.0, 0.0);
        }
        self.out_scalar
            .iter()
            .zip(self.out_optimized.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max)
    }
}

/// partitioned kernel 档：N 源/角色、M 参与者 m×m G 矩阵 + delta baseline 的融合路由。
struct PartitionedKernelBench {
    topo: DirectTopology,
    block: usize,
    plan: PartitionedPlan,
    plan_data: Vec<f32>,
    state_scalar: Vec<f32>,
    state_optimized: Vec<f32>,
    input: Vec<f32>,
    out_scalar: Vec<f32>,
    out_optimized: Vec<f32>,
}

impl PartitionedKernelBench {
    fn new(topo: DirectTopology, block: usize, extended: bool) -> Self {
        let block = block.clamp(1, DIRECT_MAX_BLOCK);
        let (a_fir, g, p) = identity_ac_p_with_g(&topo);
        let coeffs = DirectCoeffsF32 {
            a_fir: &a_fir,
            g: &g,
            p: &p,
        };
        let layout = if extended {
            PartitionedLayout::build_extended(&topo, block).expect("partitioned extended layout")
        } else {
            PartitionedLayout::build(&topo, block).expect("partitioned layout")
        };
        let mut plan_data = vec![0.0f32; layout.plan_samples()];
        let mut scratch = vec![0.0f32; layout.build_scratch_samples()];
        let plan = if extended {
            PartitionedPlan::build_f32_extended(&topo, block, &coeffs, &mut plan_data, &mut scratch)
                .expect("partitioned extended plan")
        } else {
            PartitionedPlan::build_f32(&topo, block, &coeffs, &mut plan_data, &mut scratch)
                .expect("partitioned plan")
        };
        let n_src = topo.n_src as usize;
        let containers = topo.container_channels as usize;
        let mut input = vec![0.0f32; n_src * block];
        fill_signal(&mut input, 0x9e37_79b9);
        Self {
            topo,
            block,
            plan,
            plan_data,
            state_scalar: vec![0.0f32; layout.state_samples()],
            state_optimized: vec![0.0f32; layout.state_samples()],
            input,
            out_scalar: vec![0.0f32; containers * block],
            out_optimized: vec![0.0f32; containers * block],
        }
    }

    fn workload(&self) -> String {
        let layout = self.plan.layout;
        format!(
            "{} 源/角色 × {} 容器，M={} 参与者 m×m G，L_F={}，B={}，K={}，Q={}，routed={}",
            layout.n_src,
            layout.container_channels,
            self.topo.m,
            layout.taps,
            layout.block_size,
            layout.fft_size,
            layout.partitions,
            layout.n_routes
        )
    }

    fn reset_states(&mut self) {
        self.state_scalar.fill(0.0);
        self.state_optimized.fill(0.0);
    }

    fn measure(
        &mut self,
        path: KernelPath,
        warmup: usize,
        measure: usize,
        budget_us: f64,
        timer_overhead_us: f64,
    ) -> TierStats {
        let Self {
            topo,
            block,
            plan,
            plan_data,
            state_scalar,
            state_optimized,
            input,
            out_scalar,
            out_optimized,
        } = self;
        let state = match path {
            KernelPath::Scalar => state_scalar,
            KernelPath::Optimized => state_optimized,
        };
        let output = match path {
            KernelPath::Scalar => out_scalar,
            KernelPath::Optimized => out_optimized,
        };
        let total = warmup + measure;
        let mut samples = Vec::with_capacity(measure);
        for phase in 0..total {
            let timed = phase >= warmup;
            let start = if timed { Some(Instant::now()) } else { None };
            let status = match path {
                KernelPath::Scalar => partitioned_block_f32(
                    topo, plan, plan_data, state, input, *block, output, *block, *block, *block,
                ),
                KernelPath::Optimized => partitioned_block_f32_optimized(
                    topo, plan, plan_data, state, input, *block, output, *block, *block, *block,
                ),
            };
            assert!(
                status.is_ok(),
                "kernel bench: partitioned status {}",
                status.0
            );
            if let Some(start) = start {
                let elapsed_us = start.elapsed().as_secs_f64() * 1.0e6 - timer_overhead_us;
                samples.push(elapsed_us.max(0.0));
            }
        }
        std::hint::black_box(output[0]);
        stats_from_samples(samples, budget_us)
    }

    fn parity(&mut self, warmup: usize) -> f32 {
        self.reset_states();
        let warmup = warmup.max(1);
        for _ in 0..warmup {
            let _ = self.measure(KernelPath::Scalar, 0, 1, 1.0, 0.0);
            let _ = self.measure(KernelPath::Optimized, 0, 1, 1.0, 0.0);
        }
        self.out_scalar
            .iter()
            .zip(self.out_optimized.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max)
    }
}

/// 构造 kernel bench 的拓扑：N 源/角色、M 参与者的 m×m G（A=delta、P=delta）。
#[allow(clippy::needless_range_loop)]
fn matrix_topology(
    roles: usize,
    container: usize,
    m: usize,
    l_g: usize,
    l_p: usize,
) -> DirectTopology {
    let roles = roles.clamp(1, 22);
    let container = container.clamp(roles, 22);
    let m = m.clamp(1, 8.min(roles));
    let l_g = l_g.clamp(1, u16::MAX as usize);
    let l_p = l_p.clamp(1, DIRECT_MAX_TAPS);
    let mut src_of_role = [DIRECT_NO_SOURCE; 22];
    let mut participants = [DIRECT_NO_SOURCE; 8];
    let mut scatter = [0u16; 22];
    for role in 0..roles {
        src_of_role[role] = role as u16;
        scatter[role] = role as u16;
    }
    for participant in 0..m {
        participants[participant] = participant as u16;
    }
    DirectTopology {
        n_src: roles as u16,
        n_role: roles as u16,
        container_channels: container as u16,
        d_samples: 0,
        l_a: 1,
        l_g: l_g as u16,
        l_p: l_p as u16,
        m: m as u16,
        src_of_role,
        participants,
        scatter,
    }
}

/// 矩阵拓扑的系数：A = delta（1 源 1 tap），P = 每角色一条 l_p-tap 低通 FIR（l_p=1 时即 delta），
/// G = 对角 2 抽头低通 + 非对角延迟混音（同 bench 的矩阵系数构造思路；量级只影响信号不影响计时）。
#[allow(clippy::needless_range_loop)]
fn identity_ac_p_with_g(topo: &DirectTopology) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n_role = topo.n_role as usize;
    let m = topo.m as usize;
    let l_g = topo.l_g as usize;
    let l_p = topo.l_p as usize;
    let a_fir = vec![1.0f32; topo.n_src as usize];
    let p_fir = lowpass_fir(l_p, 0.1);
    let mut p = Vec::with_capacity(n_role * l_p);
    for _ in 0..n_role {
        p.extend_from_slice(&p_fir);
    }
    let mut g = vec![0.0f32; m * m * l_g];
    for row in 0..m {
        for col in 0..m {
            let base = (row * m + col) * l_g;
            if row == col {
                // 对角：低通 2-tap（0.5, 0.5），重叠相加。
                g[base] = 0.5;
                if l_g > 1 {
                    g[base + 1] = 0.25;
                }
            } else if l_g > 1 {
                g[base + 1] = 0.02;
            }
        }
    }
    (a_fir, g, p)
}

// ===========================================================================
// 档位运行
// ===========================================================================

/// 跑一个 direct kernel 档：两列（按 KernelMode 裁剪）+ 加速比 + parity。
#[allow(clippy::too_many_arguments)]
fn run_direct_tier(
    name: &str,
    purpose: String,
    lanes: usize,
    taps: usize,
    block: usize,
    config: &BenchConfig,
    backend: &str,
    budget_us: f64,
    timer_overhead_us: f64,
) -> KernelTierReport {
    let mut bench = DirectKernelBench::new(lanes, taps, block);
    let workload = bench.workload(lanes);
    let parity_max_abs = bench.parity(config.warmup_blocks);

    let mut scalar = None;
    if matches!(config.kernel_mode, KernelMode::Auto | KernelMode::Scalar) {
        let mut repeats = Vec::with_capacity(config.repeats);
        for _ in 0..config.repeats {
            bench.reset_states();
            repeats.push(bench.measure(
                KernelPath::Scalar,
                config.warmup_blocks,
                config.measure_blocks,
                budget_us,
                timer_overhead_us,
            ));
        }
        scalar = Some(aggregate(&repeats, budget_us));
    }
    let mut optimized = None;
    if matches!(config.kernel_mode, KernelMode::Auto | KernelMode::Optimized) {
        let mut repeats = Vec::with_capacity(config.repeats);
        for _ in 0..config.repeats {
            bench.reset_states();
            repeats.push(bench.measure(
                KernelPath::Optimized,
                config.warmup_blocks,
                config.measure_blocks,
                budget_us,
                timer_overhead_us,
            ));
        }
        optimized = Some(aggregate(&repeats, budget_us));
    }
    KernelTierReport::from_columns(
        name,
        purpose,
        workload,
        backend.to_string(),
        scalar,
        optimized,
        parity_max_abs,
    )
}

/// 跑一个 partitioned kernel 档。
#[allow(clippy::too_many_arguments)]
fn run_partitioned_tier(
    name: &str,
    purpose: String,
    topo: DirectTopology,
    block: usize,
    extended: bool,
    config: &BenchConfig,
    backend: &str,
    budget_us: f64,
    timer_overhead_us: f64,
) -> KernelTierReport {
    let mut bench = PartitionedKernelBench::new(topo, block, extended);
    let workload = bench.workload();
    let parity_max_abs = bench.parity(config.warmup_blocks);
    let mut scalar = None;
    if matches!(config.kernel_mode, KernelMode::Auto | KernelMode::Scalar) {
        let mut repeats = Vec::with_capacity(config.repeats);
        for _ in 0..config.repeats {
            bench.reset_states();
            repeats.push(bench.measure(
                KernelPath::Scalar,
                config.warmup_blocks,
                config.measure_blocks,
                budget_us,
                timer_overhead_us,
            ));
        }
        scalar = Some(aggregate(&repeats, budget_us));
    }
    let mut optimized = None;
    if matches!(config.kernel_mode, KernelMode::Auto | KernelMode::Optimized) {
        let mut repeats = Vec::with_capacity(config.repeats);
        for _ in 0..config.repeats {
            bench.reset_states();
            repeats.push(bench.measure(
                KernelPath::Optimized,
                config.warmup_blocks,
                config.measure_blocks,
                budget_us,
                timer_overhead_us,
            ));
        }
        optimized = Some(aggregate(&repeats, budget_us));
    }
    KernelTierReport::from_columns(
        name,
        purpose,
        workload,
        backend.to_string(),
        scalar,
        optimized,
        parity_max_abs,
    )
}

/// 构造并跑全部 kernel 级对照档（规模随 BenchConfig 缩放）。
#[allow(clippy::manual_is_multiple_of)]
pub(crate) fn run_kernel_tiers(
    config: &BenchConfig,
    budget_us: f64,
    timer_overhead_us: f64,
) -> Vec<KernelTierReport> {
    let backend = SimdBackend::detect().name();
    let lanes = config.lanes.max(1);
    let block = config.block_frames.clamp(1, DIRECT_MAX_BLOCK);
    let taps = config.crossover_taps.clamp(1, DIRECT_MAX_TAPS);
    let mut tiers = Vec::with_capacity(4);

    tiers.push(run_direct_tier(
        "K-direct-crossover",
        "预算杀手：每 lane 一条 taps-tap 全速率 A-FIR（host graph/crossover.rs 的真实调用形态）"
            .to_string(),
        lanes,
        taps,
        block,
        config,
        backend,
        budget_us,
        timer_overhead_us,
    ));
    // 4096-tap（DIRECT_MAX_TAPS）档：只在完整规模运行（小配置下 debug 测试会慢）。
    if taps >= 1024 {
        tiers.push(run_direct_tier(
            "K-direct-crossover4096",
            "DIRECT_MAX_TAPS=4096 的 crossover FIR（T38 基线 322% 预算的那档）".to_string(),
            lanes,
            DIRECT_MAX_TAPS,
            block,
            config,
            backend,
            budget_us,
            timer_overhead_us,
        ));
    }

    // 3 kHz 低速率域矩阵形态：B = chunk_frames / factor，M 参与者 m×m G。
    let chunk = config.block_frames.min(DIRECT_MAX_BLOCK);
    let factor = usize::from(config.matrix_factor.max(1));
    let low_block = if chunk % factor == 0 && chunk / factor > 0 {
        chunk / factor
    } else {
        chunk
    };
    let matrix_roles = lanes.min(22);
    let m = config.matrix_active_lanes.clamp(1, matrix_roles.min(8));
    let low_topo = matrix_topology(
        matrix_roles,
        lanes.min(22),
        m,
        config.matrix_taps as usize,
        1,
    );
    tiers.push(run_partitioned_tier(
        "K-part-3khz",
        format!(
            "matrix 低速率域形态（factor={}）：{}×{}、M={} 参与者 m×m G 的共享 FFT 分区卷积",
            factor, matrix_roles, matrix_roles, m
        ),
        low_topo,
        low_block,
        false,
        config,
        backend,
        budget_us,
        timer_overhead_us,
    ));

    // FFT 主导形态：22 lane、L_F=8、B=block（Q=1）：每块 2*lanes 次 K 点 FFT，验证蝶形 SIMD。
    let fft_roles = lanes.min(22);
    let fft_m = config.matrix_active_lanes.clamp(1, fft_roles.min(8));
    let fft_topo = matrix_topology(fft_roles, lanes.min(22), fft_m, 8, 1);
    tiers.push(run_partitioned_tier(
        "K-part-fft256",
        format!(
            "FFT 主导形态：{} lane、L_F=8、Q=1，每块 {} 次 K 点 FFT + IFFT（验证蝶形/复数乘 SIMD）",
            fft_roles,
            2 * fft_roles
        ),
        fft_topo,
        block,
        false,
        config,
        backend,
        budget_us,
        timer_overhead_us,
    ));

    // 中等 taps：L_F = 1 + 8 + 1024 - 2 = 1031，B=block（默认 Q=9）：FFT 与频域乘加各占一半。
    let medium_roles = lanes.min(8);
    let medium_m = config.matrix_active_lanes.clamp(1, medium_roles.min(8));
    let medium_topo = matrix_topology(medium_roles, lanes.min(22), medium_m, 8, 1024);
    tiers.push(run_partitioned_tier(
        "K-part-1024tap",
        "中等 taps：L_F=1031（默认 Q=9）、8 源/角色的融合路由，FFT + 频域乘加混合负载".to_string(),
        medium_topo,
        block,
        false,
        config,
        backend,
        budget_us,
        timer_overhead_us,
    ));

    // partitioned extended 大 taps：L_F = 1 + 8 + 4096 - 2 = 4103（生产校准链 P_post 融合形态）。
    if taps >= 1024 {
        let roles = lanes.min(8);
        let m = config.matrix_active_lanes.clamp(1, roles.min(8));
        let extended_topo = matrix_topology(roles, lanes.min(22), m, 8, DIRECT_MAX_TAPS);
        tiers.push(run_partitioned_tier(
            "K-part-4096tap",
            "partitioned extended：L_F=4103（Q=33）、64 条融合路由，镜像校准链 4096-tap P_post 融合进 G"
                .to_string(),
            extended_topo,
            block,
            true,
            config,
            backend,
            budget_us,
            timer_overhead_us,
        ));
    }
    tiers
}
