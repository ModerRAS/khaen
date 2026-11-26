//! crossover 段：每 lane 一条全采样率 FIR。
//!
//! 两种卷积实现，按每 lane 的抽头数在编译期选择（[PARTITIONED_MIN_TAPS]）：
//!
//! 1. **direct 直算**（taps <= PARTITIONED_MIN_TAPS）：复用 khaen_core::kernels::direct
//!    （T56 起默认走 `dispatch::direct_block_f32_optimized` 的 SIMD 派发，标量入口保留为
//!    显式回退）。
//!    每 lane 用 1 源 / 1 角色 / 1 容器的 [DirectTopology] 表达一条 FIR：a_fir 即该 lane
//!    的抽头，g = [0.0]、p = [1.0]，内核对每个样本做 y = a * x（x * 1.0 逐位不扰动）。
//! 2. **UPOLS 分区卷积**（taps > PARTITIONED_MIN_TAPS）：同一语义拓扑交给
//!    khaen_core::kernels::partitioned（共享 FFT 的均匀分区 overlap-save）。NONRT 编译期按
//!    图块长 B 建 [PartitionedLayout] / [PartitionedPlan]，RT 每块只调用
//!    partitioned_block_f32。1 源 / 1 角色 / 1 路由时每块成本约为 2 次 K 点 FFT +
//!    Q = ceil(taps / B) 个分区频谱乘；直算是 B * taps 次 MAC，taps 越大差距越大。
//!
//! taps == [1.0] 的 identity 滤波器在编译期折叠为直通；没有滤波器的 lane 同样直通。
//!
//! # 阈值（为什么 96）
//!
//! 分区路径有固定成本（每块 2 次 K=256 FFT + 入口校验）与按 taps/B 增长的频谱乘成本，直算
//! 成本是 B * taps：taps 很小时直算更快。阈值取在本机 release bench 上两条路径的交叉点附近；
//! 实测数字与扫描表见 T39b 报告，这里记录口径：22 lane、B=128、matrix identity、只跑
//! crossover 段。阈值只影响性能与内存（plan/state），两条路径的**数值语义**同一套系数、
//! 同一 causal 卷积（f32 FFT 与直算按容差对拍）。
//!
//! # 块长 / partial 契约
//!
//! 图的块长 B = chunk_frames = min(block_frames, DIRECT_MAX_BLOCK)（见 graph 模块），
//! 分区 plan 按同一 B 构建。BlockEngine 保证每次 Processor::process 都是整块
//! （帧数 = 配置块长），因此生产路径上 crossover 只会看到 full-B 块。
//!
//! PlanGraph::process 是公开的，也接受任意帧数的输入：内部按 B 切块，只有一次调用的**末块**
//! 可能 frames < B。对分区 lane 这是内核文档里的**终止性 DRAIN**（partitioned 文件头）：
//! 输出 [0, frames) 正确，之后清零历史并置新 epoch（下一次调用从零历史开始）；对 direct lane
//! 则是可拼接的 partial（跨调用保留历史）。因此：
//! - 每次调用帧数是 B 的整数倍时（生产路径与块长不变性测试的口径），分区 lane 与 direct lane
//!   输出一致（f32 FFT 与直算只能按容差对拍，内核文档取相对 L2 <= 1e-5）；
//! - 出现 partial 末块之后再继续喂流，分区 lane 从新 epoch 开始，与 direct lane 的拼接语义
//!   不同。这是内核契约（partitioned vs direct 文件头的 partial 说明），不是本段的自由
//!   选择；测试 crossover_partitioned.rs 的 partial_chunk_is_terminal_drain 把这条差异显式钉住。
//!
//! # 内存 / 容量（编译期 NONRT）
//!
//! 每 lane 常驻缓冲（B=128、K=256、Q = ceil(taps / B)）：
//! - plan：K + Q*2K 个 f32（4096 taps：Q=32 → 16640 f32 = 65.0 KiB）；
//! - state：B + Q*2K + 4K + 1 个 f32（4096 taps：17537 f32 = 68.5 KiB）；
//! - 构建 scratch：2*L_F + 2K 个 f32（4096 taps：8704 f32 = 34 KiB），建完立即释放。
//!
//! 22 lane × 4096 taps 常驻约 2.9 MiB（direct 路径 state 约 0.4 MiB）；准入预算
//! Admission.budget.memory_bytes 只按系数 + 延迟线估算、不含 plan 频谱，所以分区路径的实际
//! 驻留高于准入估算（T39b 报告给出数字）。准入的上限口径（系数 / 延迟 / lane）不变。
//!
//! 最坏情况有界：taps <= DIRECT_MAX_TAPS (4096)、lanes <= 32（准入默认上限）、B <= 128，
//! 因此 Q <= 32、K <= 256，单 lane 常驻 <= 134 KiB、32 lane <= 4.3 MiB，不需要额外容量上限。
//!
//! # 回退
//!
//! 分区 plan 建不出来时（例如块长 B 太小让 Q = ceil(taps / B) > MAX_PARTITIONS = 1500，
//! 见 PartitionedLayout::build），该 lane 回退到 direct 直算：行为与改动前一致，且
//! PlanGraph::compile 不会因为一个性能优化而拒绝原本合法的方案。
//!
//! # 内核派发（T56）
//!
//! 两条路径的内核入口由 [`KernelPath`](super::KernelPath) 在**编译期**选定，运行期只做一次
//! 可预测分支：
//!
//! - `Scalar`：`direct_block_f32` / `partitioned_block_f32`，与 T56 之前逐位一致
//!   （bit-exact 口径，测试 `crossover_partitioned.rs` 的 scalar 用例钉住）；
//! - `Optimized`（默认 `KernelChoice::Auto` 在本机有 AVX2+FMA / NEON 时解析得到）：核心的
//!   `*_optimized` 入口，SIMD 内核与本段的 1 源/1 角色/1 容器拓扑一一对应（direct 路径
//!   即每 lane 一条 SIMD A-FIR；partitioned 路径即 SIMD FFT/复数乘加）。与标量的差异只有
//!   FMA 融合/累加顺序的舍入，容差与 core 对拍口径一致：direct 相对 L2 ≤ 1e-6、单样本绝对
//!   ≤ 1e-6；partitioned 相对 L2 ≤ 1e-5、单样本绝对 ≤ 1e-4。
//!
//! **state 不可跨路径混用**：direct 的标量参考与 SIMD 路径 state 长度相同但历史方向相反
//! （见 `khaen_core::kernels::direct_simd` 模块文档），同一 lane 的 state 一旦开始喂某条路径
//! 就只能在同一条路径上继续。本段把路径钉在 stage 上（编译期固定、运行期只读），因此不存在
//! 混用；要切路径必须重新 `PlanGraph::compile`（新 state、新 epoch）。partitioned 两条路径的
//! state 布局相同、可交替，但本段同样不做运行期切换。

use khaen_core::kernels::direct::{
    direct_block_f32, DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_TAPS,
};
use khaen_core::kernels::dispatch::{direct_block_f32_optimized, partitioned_block_f32_optimized};
use khaen_core::kernels::partitioned::{partitioned_block_f32, PartitionedLayout, PartitionedPlan};
use khaen_core::plan::CrossoverSection;

use super::{CompileError, KernelPath};

/// taps 超过该值的 lane 改用 UPOLS 分区卷积，其余（含等于）继续走 direct 直算。
///
/// 依据（T39b 本机 release bench，22 lane、B=128、matrix identity、khaen-bench --quick，
/// 单块 µs 取 min/p50）：
///
/// ~~~text
/// taps     direct min/p50      partitioned min/p50
/// 32       16.9 / 17.0        87.6 / 89.9
/// 64       37.8 / 38.4        88.4 / 88.9
/// 96       67.6 / 69.6        88.2 / 89.9
/// 128     108.3 / 111.1       88.5 / 90.2      <- 交叉点在 96..128 之间（约 100-110 taps）
/// 256     283.6 / 293.4       88.6 / 93.6
/// 1024   1329.3 / 1369.2      98.5 / 103.3
/// 4096   5524.3 / 5817.1     145.3 / 159.1
/// ~~~
///
/// 阈值取 96：taps <= 96 直算明显更快（96 taps 时 27%），taps >= 128 分区明显更快
/// （128 taps 时 19%），97..127 两条路径接近、差异在采样噪声量级。阈值按 B=128 标定；B 更小
/// 时两条路径的成本近似同比例缩小，交叉点量级不变。阈值只决定性能/内存取舍，不改变数值语义
/// （同一系数、同一 causal 卷积）。
///
/// # T56 备注：上表是**标量内核**的标定，SIMD 默认路径下交叉点已右移
///
/// T56 起 `KernelChoice::Auto` 在 x86_64 AVX2+FMA / aarch64 上走核心优化入口，两个内核的
/// 提速比不同：直算 SIMD 相比标量约 6~10x（每 lane 一条 A-FIR 的形态），分区 SIMD 相比标量
/// 约 1.3~1.4x（FFT 固定成本占比高）。因此 SIMD 口径下的交叉点从 ~96-110 taps 右移到
/// **约 300-450 taps**。本机 release `khaen-bench --quick`（22 lane、B=128、48 kHz，
/// 单块预算 2.667 ms，p50）实测：
///
/// ~~~text
/// taps    直算 SIMD（K-direct 内核档）   分区 SIMD（B 档 graph）
/// 96      27.9 µs（B0 档整图）           —（< 阈值，直算）
/// 128     49.7 µs                        61.4 µs
/// 256     47.9 µs                        64.2 µs
/// 512     78.3 µs                        68.3 µs   <- 交叉点在 256..512 之间
/// ~~~
///
/// （这台机器上 quick 模式噪声较大：128/256 的直算列非单调，只能定位量级。）
///
/// **T56 不修改这个常量**：① 96 是 T39b 已发布的标定，改阈值会让 97..400 taps 的方案改用
/// 另一条内核路径（两条路径数值只在容差内一致：partitioned f32 FFT 与直算不同位），属于
/// 行为变更，应由专门任务连同测试/报告一起做；② 真实 crossover 抽头多在 1024~4096，落在
/// 阈值两侧的明显区间，96 不会把它们分错；③ 重新标定需要稳定测量（非 quick）、B=64/96 kHz
/// 与内存预算（直算 state 远小于分区 plan/state）的复核。记为后续建议：SIMD 口径的阈值
/// 重标定（预计 ~384）可让 97..400 taps 的方案少走分区固定成本，并降低这区间的驻留内存。
pub const PARTITIONED_MIN_TAPS: usize = 96;

/// 单 lane 的 direct 直算状态与系数。
#[derive(Debug)]
struct DirectFir {
    /// 1 源 / 1 角色 / 1 容器的 direct 拓扑。
    topo: DirectTopology,
    /// 固定缓冲布局（含 block 中间量）。
    layout: DirectLayout,
    /// 输入侧 FIR = 该 lane 的抽头。
    a: Vec<f32>,
    /// 低带矩阵系数（固定为 0）。
    g: Vec<f32>,
    /// 输出对角 FIR（固定为 delta）。
    p: Vec<f32>,
    /// 跨块保留的卷积状态（编译期零初始化）。
    state: Vec<f32>,
}

/// 单 lane 的 UPOLS 分区卷积状态与预分配 plan。
#[derive(Debug)]
struct PartitionedFir {
    /// 同一 1 源 / 1 角色 / 1 容器拓扑（RT 入口每次复算布局用）。
    topo: DirectTopology,
    /// 固定布局 + 路由表（RT 入口校验 plan 用）。
    plan: PartitionedPlan,
    /// 分区频谱 + twiddle 表（编译期填好后只读）。
    plan_data: Vec<f32>,
    /// 跨块保留的历史（x_prev / x_hist / work / slot；编译期零初始化）。
    state: Vec<f32>,
}

/// 单 lane 的卷积实现（编译期按 taps 选定，运行期不切换）。
#[derive(Debug)]
enum LaneFir {
    /// 两个变体都装箱：DirectFir 264 B / PartitionedFir 736 B，装箱后每 lane 的枚举只占一个
    /// 指针（22 lane 省 ~16 KiB）；解析开销在 RT 路径上只是一次指针解引用。
    Direct(Box<DirectFir>),
    Partitioned(Box<PartitionedFir>),
}

/// crossover 段状态：每 lane 可选的一条 FIR。
#[derive(Debug)]
pub(crate) struct CrossoverStage {
    /// None 表示该 lane 直通。
    filters: Vec<Option<LaneFir>>,
    /// 卷积内核入口（编译期按 [`KernelChoice`](super::KernelChoice) 固定，运行期不切换）。
    path: KernelPath,
}

impl CrossoverStage {
    /// 编译 crossover 段；缺少区段时整段直通。
    ///
    /// block_size 是图的子块帧数 B（min(block_frames, DIRECT_MAX_BLOCK)），分区 plan
    /// 按它构建；B 也是 RT 每块调用的固定块长。
    pub(crate) fn compile(
        section: Option<&CrossoverSection>,
        lanes: usize,
        block_size: usize,
        path: KernelPath,
    ) -> Result<Self, CompileError> {
        let mut filters: Vec<Option<LaneFir>> = (0..lanes).map(|_| None).collect();
        if let Some(section) = section {
            for filter in &section.filters {
                let lane = filter.lane as usize;
                if lane >= lanes {
                    return Err(CompileError::InvalidSection(
                        "crossover filter lane out of range",
                    ));
                }
                if filters[lane].is_some() {
                    return Err(CompileError::InvalidSection("duplicate crossover lane"));
                }
                if filter.coeffs.is_empty() {
                    return Err(CompileError::InvalidSection("crossover filter has no taps"));
                }
                let taps = filter.coeffs.len();
                if taps > DIRECT_MAX_TAPS {
                    return Err(CompileError::TapsTooLong {
                        stage: "crossover",
                        taps,
                        limit: DIRECT_MAX_TAPS,
                    });
                }
                for coeff in &filter.coeffs {
                    if !coeff.is_finite() {
                        return Err(CompileError::NonFiniteCoefficient("crossover"));
                    }
                }
                // identity 判据：taps == [1.0] 在编译期折叠为直通。
                if taps == 1 && filter.coeffs[0] == 1.0 {
                    continue;
                }
                let mut topo = DirectTopology::identity();
                topo.l_a = taps as u16;
                let fir = if taps > PARTITIONED_MIN_TAPS {
                    match Self::build_partitioned(&topo, &filter.coeffs, block_size) {
                        Some(fir) => LaneFir::Partitioned(Box::new(fir)),
                        // 分区 plan 建不出来时回退 direct；见模块文档「回退」。
                        None => {
                            LaneFir::Direct(Box::new(Self::build_direct(&topo, &filter.coeffs)?))
                        }
                    }
                } else {
                    LaneFir::Direct(Box::new(Self::build_direct(&topo, &filter.coeffs)?))
                };
                filters[lane] = Some(fir);
            }
        }
        Ok(Self { filters, path })
    }

    /// [NONRT] 建 direct 直算 lane（与改动前完全一致）。
    fn build_direct(topo: &DirectTopology, taps: &[f32]) -> Result<DirectFir, CompileError> {
        let layout = DirectLayout::build(topo).map_err(|_| {
            CompileError::InvalidSection("crossover filter rejected by direct kernel")
        })?;
        Ok(DirectFir {
            topo: *topo,
            layout,
            a: taps.to_vec(),
            g: vec![0.0],
            p: vec![1.0],
            state: vec![0.0f32; layout.state_samples()],
        })
    }

    /// [NONRT] 建 UPOLS 分区 lane；plan 建不出来（块长太小 / 容量不符）返回 None 由调用方回退。
    fn build_partitioned(
        topo: &DirectTopology,
        taps: &[f32],
        block_size: usize,
    ) -> Option<PartitionedFir> {
        let layout = PartitionedLayout::build(topo, block_size).ok()?;
        // plan 与构建 scratch 一次性分配（NONRT）；scratch 只在本函数内使用。
        let mut plan_data = vec![0.0f32; layout.plan_samples()];
        let mut build_scratch = vec![0.0f32; layout.build_scratch_samples()];
        // g/p 与 direct 路径同口径：G = delta_D（D=0）、P = delta。
        let g = [0.0f32];
        let p = [1.0f32];
        let coeffs = DirectCoeffsF32 {
            a_fir: taps,
            g: &g,
            p: &p,
        };
        let plan = PartitionedPlan::build_f32(
            topo,
            block_size,
            &coeffs,
            &mut plan_data,
            &mut build_scratch,
        )
        .ok()?;
        let state = vec![0.0f32; layout.state_samples()];
        Some(PartitionedFir {
            topo: *topo,
            plan,
            plan_data,
            state,
        })
    }

    /// 处理一个子块：input / output 均为 lanes 个平面。
    ///
    /// stride 是平面跨度（= 编译期块长 B），frames 是本块有效帧数（<= B）。生产路径
    /// frames == B；frames < B 只在公开 PlanGraph::process 的非整块调用末块出现，对分区
    /// lane 是终止性 DRAIN（见模块文档）。
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        // 拷贝出 Copy 的路径标记：循环里 filters 被可变借用，不能同时读 self.path。
        let path = self.path;
        for (lane, filter) in self.filters.iter_mut().enumerate() {
            let base = lane * stride;
            match filter {
                None => {
                    output[base..base + frames].copy_from_slice(&input[base..base + frames]);
                }
                Some(LaneFir::Direct(fir)) => {
                    let inp = &input[base..base + frames];
                    let out = &mut output[base..base + frames];
                    let coeffs = DirectCoeffsF32 {
                        a_fir: &fir.a,
                        g: &fir.g,
                        p: &fir.p,
                    };
                    let status = match path {
                        KernelPath::Scalar => direct_block_f32(
                            &fir.topo,
                            &fir.layout,
                            &coeffs,
                            &mut fir.state,
                            inp,
                            frames,
                            out,
                            frames,
                            frames,
                            frames,
                        ),
                        KernelPath::Optimized => direct_block_f32_optimized(
                            &fir.topo,
                            &fir.layout,
                            &coeffs,
                            &mut fir.state,
                            inp,
                            frames,
                            out,
                            frames,
                            frames,
                            frames,
                        ),
                    };
                    // 编译期已保证参数合法；若内核仍报错则本 lane 静音（fail-safe，不 panic）。
                    if !status.is_ok() {
                        out.fill(0.0);
                    }
                }
                Some(LaneFir::Partitioned(fir)) => {
                    let block = fir.plan.layout.block_size as usize;
                    // 图只以编译期块长调用；stride 小于 plan 块长时无法安全喂内核（防御性静音，
                    // 不 panic）。正常路径 stride == block。
                    if stride < block {
                        output[base..base + frames].fill(0.0);
                        continue;
                    }
                    // 内核要求输入 / 输出缓冲长度 >= block；图平面每 lane 恰有 stride 个样本，
                    // 且 stride >= block，因此取整块切片不会越过本 lane 的平面。
                    let inp = &input[base..base + block];
                    let out = &mut output[base..base + block];
                    let status = match path {
                        KernelPath::Scalar => partitioned_block_f32(
                            &fir.topo,
                            &fir.plan,
                            &fir.plan_data,
                            &mut fir.state,
                            inp,
                            block,
                            out,
                            block,
                            block,
                            frames,
                        ),
                        KernelPath::Optimized => partitioned_block_f32_optimized(
                            &fir.topo,
                            &fir.plan,
                            &fir.plan_data,
                            &mut fir.state,
                            inp,
                            block,
                            out,
                            block,
                            block,
                            frames,
                        ),
                    };
                    // 同上：内核报错时 fail-safe 静音本 lane 的有效帧。
                    if !status.is_ok() {
                        output[base..base + frames].fill(0.0);
                    }
                }
            }
        }
    }
}
