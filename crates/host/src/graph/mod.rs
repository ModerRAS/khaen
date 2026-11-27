//! 方案图执行器：把方案 ABI v3 编译成可执行的实时处理图。
//!
//! 链序严格固定为 content -> crossover -> matrix -> protection -> routing（见
//! `docs/DESIGN.md` 第 3 节的运行时数据面）：
//!
//! 1. **content**：容器通道到 lane 的静态映射与折叠增益；运行期不做动态混音。
//! 2. **crossover**：每 lane 一条全采样率 FIR。taps <= `CROSSOVER_PARTITIONED_MIN_TAPS`（96）
//!    时复用 `khaen_core::kernels::direct` 直算；taps > 阈值时复用
//!    `khaen_core::kernels::partitioned` 的 UPOLS 分区卷积（共享 FFT + overlap-save，T39b）。
//!    两条路径同一套系数、同一 causal 卷积，只是数值精度不同（f32 FFT 对直算按容差对拍）。
//! 3. **matrix**：跨 lane 满矩阵。`decimation == interpolation == 1` 时走全速率直接卷积（identity
//!    判据下逐位直通）；`decimation == interpolation == F > 1` 时走低速率域：每列抽取到
//!    `fs / F`、在低速率域做满矩阵 FIR、每行插值回全速率（T22；见 `matrix` 模块）。
//!    变比（抽取 ≠ 插值，即 ASRC）在编译期返回 `CompileError::UnsupportedRates`。
//! 4. **protection**：每 lane 延迟 / 极性 / 电平 / PEQ（Direct Form I biquad 级联）。
//! 5. **routing**：lane 到 DAC 通道的静态路由；未映射的 DAC 通道逐样本输出 +0.0。
//!
//! # 实时约束
//!
//! `PlanGraph::compile` 是唯一的分配 / 校验 / 预计算阶段：planar scratch、各段卷积与滤波
//! 状态、延迟线全部在这里分配完毕。`Processor::process` 只读写这些预分配缓冲，零分配、零锁、
//! 无 IO、无 panic 分支；方案越界、尺寸不符、非法系数、超预算全部在编译期 fail-closed 拒绝。
//! 帧交错输入先解交错到 planar scratch，处理完再交错回输出。
//!
//! 块内按不超过 `khaen_core::kernels::direct::DIRECT_MAX_BLOCK` 帧的子块推进；direct 内核与
//! 各段状态都满足块长无关，因此同一图在任意块长切分下输出逐位一致。
//!
//! 例外是 crossover 段的分区 lane：UPOLS 的 partial 块是**终止性 DRAIN**（内核契约）。生产
//! 路径（`BlockEngine` 固定块长）与"每次调用帧数是子块帧数整数倍"的调用都只产生 full-B 块，
//! 此时 block-length 不变性成立；若公开的 `PlanGraph::process` 收到非整块末块，分区 lane 的
//! 输出在该块仍是正确的（valid_frames 内），但之后从新 epoch（零历史）开始，与 direct lane
//! 可拼接 partial 的语义不同。差异与理由见 `graph::crossover` 模块文档。
//!
//! # identity 判据
//!
//! 满足以下条件的方案在全链上逐位等于输入（夹具见 `crates/host/tests/graph_executor.rs`）：
//! content `in_channels == lanes` 且每 lane 单 fold(`src == dst`, `gain == 1.0`)；crossover
//! 每 lane `taps == [1.0]`；matrix `rows == cols`、`taps == 1`、`decimation == interpolation == 1`、
//! 系数对角 1.0 其余 0；protection `delay == 0`、`polarity == 1`、`gain_db == 0`、无 PEQ（或每段
//! PEQ `b0 == 1.0` 其余 0）；routing `lane i -> dac i` 且 `dac_channels == lanes`。这些判据在
//! 编译期被折成直通段 / 恒等段，因此逐位相等不依赖浮点舍入。
//!
//! # 延迟上报
//!
//! [`PlanGraph::estimated_latency_samples`] 上报 matrix 段自身的估算延迟（全采样率样本）：
//! 重采样滤波器群延迟 + 低速率域矩阵 FIR 群延迟。crossover / protection 的延迟不在内。
//!
//! # T22 / T39b / T56 边界
//!
//! 已实现（T22）：矩阵 3 kHz 低速率域（每列抽取 / 每行插值的有理数重采样）与 matrix 段延迟估算。
//! 已实现（T39b）：crossover 段的分区卷积（UPOLS，按每 lane taps 阈值在编译期选择）。
//! 已实现（T56）：卷积内核的 SIMD 派发接线。crossover 的两条路径（direct 直算 / partitioned
//! 分区卷积，按每 lane taps 阈值划分）都改为调 `khaen_core::kernels::dispatch` 的优化入口；
//! 内核路径在编译期按 [`KernelChoice`] 一次确定（默认 `Auto`：本机有 SIMD 后端就走优化
//! 入口），标量参考内核保留为可显式选择的 bit-exact 口径。本机派发后端与实际路径通过
//! [`PlanGraph::kernel_dispatch`] 暴露（真实探测值，供 status / 日志 / bench 读取）。
//!
//! T56 的覆盖范围只到 crossover：matrix 段的成本几乎全在低速率域重采样器（`kernels::resample`
//! 没有 optimized 入口），全速率矩阵 FIR 又只出现在非生产形态，实测拆分与"暂不接线"的依据
//! 见 `graph::matrix` 模块文档；crossover 分区阈值（96）是按标量内核标定的，SIMD 口径下
//! 交叉点右移约到 300-450 taps，实测与不改的理由见 `graph::crossover` 的
//! [`CROSSOVER_PARTITIONED_MIN_TAPS`] 文档。
//! 未实现（留给后续）：变比重采样（ASRC）、系数在块边界的原子切换、矩阵低速率域的
//! 分区卷积、矩阵段的 SIMD 格子卷积、SIMD 口径的 crossover 阈值重标定。

use khaen_core::kernels::direct::DIRECT_MAX_BLOCK;
use khaen_core::kernels::dispatch::SimdBackend;
use khaen_core::plan::Plan;

use crate::admission::{admit, Admission, AdmissionLimits};
use crate::engine::{EngineConfig, EngineError, Processor};

mod content;
mod crossover;
mod matrix;
mod protection;
mod routing;

use content::ContentStage;
use crossover::CrossoverStage;
use matrix::MatrixStage;
use protection::ProtectionStage;
use routing::RoutingStage;

/// crossover 段切换到 UPOLS 分区卷积的 taps 阈值（含等于时仍走 direct）。
///
/// 每 lane 独立判定：taps 大于 CROSSOVER_PARTITIONED_MIN_TAPS 的 lane 用分区卷积，其余用
/// direct 直算；identity（taps == [1.0]）继续在编译期折叠为直通。阈值只决定性能/内存取舍，
/// 不改变数值语义（同一系数、同一 causal 卷积）。取值依据与扫描见 graph::crossover 模块
/// 文档与 T39b 报告。
pub const CROSSOVER_PARTITIONED_MIN_TAPS: usize = crossover::PARTITIONED_MIN_TAPS;

/// 卷积内核路径的编译期选择（[`PlanGraph::compile_with_kernel`] 的入参）。
///
/// 运行期**不切换**：direct 内核的标量参考路径与 SIMD 路径 state 长度/偏移相同，但历史方向
/// 相反（见 `khaen_core::kernels::direct_simd` 的「状态表示」），同一 epoch 内混用会读到对方
/// 的镜像历史。因此路径在 `PlanGraph::compile` 时按本机探测结果一次确定，之后只读。
///
/// 两条路径的数值关系：`Scalar` 与 T56 之前的实现逐位一致（bit-exact 口径，保留给对拍与
/// 回退）；`Optimized` 走核心的 SIMD 入口，与标量的差异只有 FMA 融合/累加顺序的舍入，
/// 容差与 core 的对拍口径一致（direct 单样本绝对 ≤ 1e-6 / 相对 L2 ≤ 1e-6；partitioned
/// 单样本绝对 ≤ 1e-4 / 相对 L2 ≤ 1e-5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KernelChoice {
    /// 默认：本机探测到可用 SIMD 后端时走优化入口，否则标量参考。
    #[default]
    Auto,
    /// 强制标量参考内核（bit-exact 口径 / 显式回退）。
    Scalar,
    /// 强制核心优化入口；本机无 SIMD 时该入口内部回退标量参考（行为与 Scalar 相同）。
    Optimized,
}

impl KernelChoice {
    /// 稳定名字（报告 / JSON / 日志）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Scalar => "scalar",
            Self::Optimized => "optimized",
        }
    }

    /// 按当前本机探测结果解析实际路径（等价于 `KernelDispatch::detect(self).path`）。
    pub fn resolve_path(self) -> KernelPath {
        resolve_kernel_path(self, SimdBackend::detect())
    }
}

/// 解析后实际使用的卷积内核入口（编译期固定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelPath {
    /// 标量参考内核（`khaen_core::kernels::direct::direct_block_f32` 等）。
    Scalar,
    /// 核心优化入口（`*_block_f32_optimized`：本机有 SIMD 时执行 SIMD 内核）。
    Optimized,
}

impl KernelPath {
    /// 稳定名字（报告 / JSON / 日志）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Optimized => "optimized",
        }
    }

    /// 是否走核心优化入口。
    pub const fn is_optimized(self) -> bool {
        matches!(self, Self::Optimized)
    }
}

/// 把编译期选择解析成实际路径：`Auto` 由本机探测后端决定，其余原样。
pub fn resolve_kernel_path(choice: KernelChoice, backend: SimdBackend) -> KernelPath {
    match choice {
        KernelChoice::Scalar => KernelPath::Scalar,
        KernelChoice::Optimized => KernelPath::Optimized,
        KernelChoice::Auto => {
            if backend.is_simd() {
                KernelPath::Optimized
            } else {
                KernelPath::Scalar
            }
        }
    }
}

/// 内核派发状态快照（status / 日志 / bench 读的那份字段）。
///
/// 三个字段都是**本机真实值**：`backend` 来自 `SimdBackend::detect()` 的运行期 CPU 探测，
/// `path` 是本次编译真正接的入口，`choice` 是编译期请求。没有数据来源的字段不要在这里补
/// （例如 xrun 计数不归本层，不属于派发状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelDispatch {
    /// 本机实际可用的 SIMD 后端（标量 / AVX2+FMA / NEON）。
    pub backend: SimdBackend,
    /// 本图请求的路径选择。
    pub choice: KernelChoice,
    /// 本图实际使用的内核入口。
    pub path: KernelPath,
}

impl KernelDispatch {
    /// 按当前本机探测结果与请求解析出一份派发快照（不依赖已编译的图）。
    pub fn detect(choice: KernelChoice) -> Self {
        let backend = SimdBackend::detect();
        Self {
            backend,
            choice,
            path: resolve_kernel_path(choice, backend),
        }
    }

    /// 后端稳定名字（`scalar` / `avx2+fma` / `neon`）。
    pub fn backend_name(self) -> &'static str {
        self.backend.name()
    }

    /// 路径稳定名字（`scalar` / `optimized`）。
    pub fn path_name(self) -> &'static str {
        self.path.name()
    }

    /// 选择稳定名字（`auto` / `scalar` / `optimized`）。
    pub fn choice_name(self) -> &'static str {
        self.choice.name()
    }

    /// 单行人类可读描述（日志用）。
    pub fn describe(self) -> String {
        format!(
            "backend={} path={} choice={}",
            self.backend_name(),
            self.path_name(),
            self.choice_name()
        )
    }
}

/// 图编译错误。
///
/// 所有错误都在 `PlanGraph::compile` 阶段返回：实时路径不返回错误，也不会因为方案问题中断。
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    /// 引擎配置非法（块长 / 通道 / 采样率）。
    Engine(EngineError),
    /// 准入拒绝：超预算或方案内部不自洽，携带完整拒绝理由（fail-closed）。
    AdmissionRejected(Admission),
    /// 方案声明的块大小与运行时配置不符。
    BlockSizeMismatch {
        /// 方案文件声明的块大小（帧）。
        plan: u32,
        /// 运行时配置的块大小（帧）。
        config: usize,
    },
    /// 方案声明的采样率与运行时配置不符。
    SampleRateMismatch {
        /// 方案文件声明的采样率（Hz）。
        plan: u32,
        /// 运行时配置的采样率（Hz）。
        config: u32,
    },
    /// 通道数不符：`EngineConfig::channels` 必须同时等于方案输入通道数与 DAC 通道数。
    ChannelMismatch {
        /// 出错的段名。
        stage: &'static str,
        /// 运行时配置的通道数。
        expected: usize,
        /// 方案在该段声明的通道数。
        actual: usize,
    },
    /// lane 数在相邻段之间不衔接。
    LaneMismatch {
        /// 出错的段名。
        stage: &'static str,
        /// 上游段给出的 lane 数。
        expected: usize,
        /// 该段声明的 lane 数。
        actual: usize,
    },
    /// 矩阵抽取 / 插值因子不受支持：固定速率图只接受 `decimation == interpolation`（含 1），
    /// 变比（抽取 ≠ 插值，即 ASRC）无法在同等输入 / 输出帧数下表示；超过因子上限的组合同样拒绝。
    /// 0 因子另行按非法区段拒绝（准入先于本段编译）。
    UnsupportedRates {
        /// 方案声明的抽取因子。
        decimation: u8,
        /// 方案声明的插值因子。
        interpolation: u8,
    },
    /// 滤波器抽头数超过 direct 内核上限。
    TapsTooLong {
        /// 出错的段名。
        stage: &'static str,
        /// 方案声明的抽头数。
        taps: usize,
        /// direct 内核允许的上限。
        limit: usize,
    },
    /// 区段内容不自洽（零 taps、重复 lane、重复 DAC、引用越界等）。
    InvalidSection(&'static str),
    /// 系数含 NaN / ±inf。
    NonFiniteCoefficient(&'static str),
}

impl core::fmt::Display for CompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CompileError::Engine(err) => write!(f, "engine config: {err}"),
            CompileError::AdmissionRejected(admission) => {
                write!(f, "admission rejected: {}", admission.summary())
            }
            CompileError::BlockSizeMismatch { plan, config } => {
                write!(f, "plan block_size {plan} != config block_frames {config}")
            }
            CompileError::SampleRateMismatch { plan, config } => {
                write!(f, "plan sample_rate {plan} != config sample_rate {config}")
            }
            CompileError::ChannelMismatch {
                stage,
                expected,
                actual,
            } => write!(
                f,
                "{stage} channel mismatch: config {expected}, plan {actual}"
            ),
            CompileError::LaneMismatch {
                stage,
                expected,
                actual,
            } => write!(f, "{stage} lane mismatch: upstream {expected}, plan {actual}"),
            CompileError::UnsupportedRates {
                decimation,
                interpolation,
            } => write!(
                f,
                "matrix decimation {decimation} / interpolation {interpolation} unsupported (fixed-rate graph needs decimation == interpolation; ASRC is not implemented)"
            ),
            CompileError::TapsTooLong { stage, taps, limit } => {
                write!(f, "{stage} taps {taps} exceed direct kernel limit {limit}")
            }
            CompileError::InvalidSection(message) => write!(f, "invalid plan section: {message}"),
            CompileError::NonFiniteCoefficient(stage) => {
                write!(f, "{stage} has a non-finite coefficient")
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// 编译后的方案图，实现 `Processor`。
///
/// 构造见 `PlanGraph::compile`；构造完成后 `process` 只读写预分配缓冲，零分配、零锁、无 IO。
#[derive(Debug)]
pub struct PlanGraph {
    /// 帧交错输入通道数（= content.in_channels 或 EngineConfig.input_channels）。
    input_channels: usize,
    /// 帧交错输出通道数（= routing.dac_channels 或 matrix.rows）。
    output_channels: usize,
    /// 子块帧数上限：`min(config.block_frames, DIRECT_MAX_BLOCK)`。
    chunk_frames: usize,
    /// 解交错后的输入平面（`channels * chunk_frames`）。
    planes_in: Vec<f32>,
    /// content 输出 / crossover 输入平面（`lanes * chunk_frames`）。
    planes_a: Vec<f32>,
    /// crossover 输出 / matrix 输入平面（`lanes * chunk_frames`）。
    planes_b: Vec<f32>,
    /// matrix 输出 / protection 输入输出 / routing 输入平面（`rows * chunk_frames`）。
    planes_c: Vec<f32>,
    /// content 段。
    content: ContentStage,
    /// crossover 段。
    crossover: CrossoverStage,
    /// matrix 段。
    matrix: MatrixStage,
    /// protection 段。
    protection: ProtectionStage,
    /// routing 段。
    routing: RoutingStage,
    /// 卷积内核派发状态（编译期确定，运行期只读）。
    kernel: KernelDispatch,
}

impl PlanGraph {
    /// 编译一份方案为可执行图。
    ///
    /// 顺序：引擎配置校验 → 准入 `admit`（超预算或方案不自洽即 fail-closed）→ 方案与运行时
    /// 配置一致性校验（块大小 / 采样率 / 通道数 / lane 链）→ 各段校验与状态分配。返回后
    /// `process` 不再分配、不加锁、不做 IO。
    pub fn compile(
        plan: &Plan,
        config: &EngineConfig,
        limits: &AdmissionLimits,
    ) -> Result<Self, CompileError> {
        Self::compile_with_kernel(plan, config, limits, KernelChoice::Auto)
    }

    /// 编译一份方案为可执行图，并显式指定卷积内核路径（[`KernelChoice`]）。
    ///
    /// 与 [`Self::compile`] 的唯一差别是卷积内核入口的选择：`Scalar` 强制标量参考内核
    /// （与 T56 之前逐位一致，bit-exact 口径）；`Optimized` 强制核心优化入口；`Auto`
    /// 按本机探测后端决定（默认）。选择在编译期一次固定：已编译的图不会在运行期换内核
    /// （direct 标量/SIMD 两条路径的 state 历史方向不同，混用会读错历史，见 [`KernelChoice`]）。
    pub fn compile_with_kernel(
        plan: &Plan,
        config: &EngineConfig,
        limits: &AdmissionLimits,
        kernel_choice: KernelChoice,
    ) -> Result<Self, CompileError> {
        config.validate().map_err(CompileError::Engine)?;
        let admission = admit(plan, limits);
        if !admission.accepted {
            return Err(CompileError::AdmissionRejected(admission));
        }
        if plan.block_size != config.block_frames as u32 {
            return Err(CompileError::BlockSizeMismatch {
                plan: plan.block_size,
                config: config.block_frames,
            });
        }
        if plan.sample_rate != config.sample_rate {
            return Err(CompileError::SampleRateMismatch {
                plan: plan.sample_rate,
                config: config.sample_rate,
            });
        }
        let matrix_section = plan
            .matrix
            .as_ref()
            .ok_or(CompileError::InvalidSection("matrix section is required"))?;
        let input_channels = config.input_channels;
        let output_channels = config.output_channels;

        // lane 链：content -> crossover -> matrix -> protection -> routing；缺段按恒等段处理。
        // 输入宽度（content.in_channels）与输出宽度（routing.dac_channels）分别校验；
        // 中间 lane 宽度由各段自行衔接（D-009：三个宽度不合并成一个数）。
        let (in_channels, lanes_after_content) = match &plan.content {
            Some(content) => (content.in_channels as usize, content.out_lanes as usize),
            None => {
                let lanes = match &plan.crossover {
                    Some(crossover) => crossover.lanes as usize,
                    None => matrix_section.cols as usize,
                };
                (input_channels, lanes)
            }
        };
        if in_channels != input_channels {
            return Err(CompileError::ChannelMismatch {
                stage: "content",
                expected: input_channels,
                actual: in_channels,
            });
        }
        let lanes_after_crossover = match &plan.crossover {
            Some(crossover) => {
                if crossover.lanes as usize != lanes_after_content {
                    return Err(CompileError::LaneMismatch {
                        stage: "crossover",
                        expected: lanes_after_content,
                        actual: crossover.lanes as usize,
                    });
                }
                lanes_after_content
            }
            None => lanes_after_content,
        };
        if matrix_section.cols as usize != lanes_after_crossover {
            return Err(CompileError::LaneMismatch {
                stage: "matrix",
                expected: lanes_after_crossover,
                actual: matrix_section.cols as usize,
            });
        }
        let rows = matrix_section.rows as usize;
        if let Some(protection) = &plan.protection {
            if protection.lanes as usize != rows {
                return Err(CompileError::LaneMismatch {
                    stage: "protection",
                    expected: rows,
                    actual: protection.lanes as usize,
                });
            }
        }
        let out_channels = match &plan.routing {
            Some(routing) => routing.dac_channels as usize,
            None => rows,
        };
        if out_channels != output_channels {
            return Err(CompileError::ChannelMismatch {
                stage: "routing",
                expected: output_channels,
                actual: out_channels,
            });
        }

        // 每块最多处理的帧数；低速率域的每块样本数由它除以抽取因子得到（编译期确定）。
        let chunk_frames = config.block_frames.min(DIRECT_MAX_BLOCK);
        // 卷积内核派发：探测 + 解析一次，编译期固定（运行期不再探测/切换）。
        let kernel = KernelDispatch::detect(kernel_choice);
        let content =
            ContentStage::compile(plan.content.as_ref(), in_channels, lanes_after_content)?;
        let crossover = CrossoverStage::compile(
            plan.crossover.as_ref(),
            lanes_after_crossover,
            chunk_frames,
            kernel.path,
        )?;
        let matrix =
            MatrixStage::compile(matrix_section, lanes_after_crossover, rows, chunk_frames)?;
        let protection = ProtectionStage::compile(plan.protection.as_ref(), rows)?;
        let routing = RoutingStage::compile(plan.routing.as_ref(), rows, out_channels)?;

        let planes_in = vec![0.0f32; in_channels * chunk_frames];
        let planes_a = vec![0.0f32; lanes_after_content * chunk_frames];
        let planes_b = vec![0.0f32; lanes_after_crossover * chunk_frames];
        let planes_c = vec![0.0f32; rows * chunk_frames];
        Ok(Self {
            input_channels,
            output_channels,
            chunk_frames,
            planes_in,
            planes_a,
            planes_b,
            planes_c,
            content,
            crossover,
            matrix,
            protection,
            routing,
            kernel,
        })
    }

    /// 估算的 matrix 段延迟（全采样率样本）。
    ///
    /// - 全速率形态（`decimation == interpolation == 1`）：`(taps - 1) / 2`（假设系数线性相位）；
    /// - 低速率形态（因子 `F > 1`）：列抽取器群延迟 + 低速率域矩阵 FIR 群延迟（乘 `F` 换算回
    ///   全速率）+ 行插值器群延迟。公式见 `matrix` 模块与 `khaen_core::kernels::resample`。
    ///
    /// 本方法**只**算 matrix 段；crossover / protection 的延迟不在内。
    pub fn estimated_latency_samples(&self) -> u64 {
        self.matrix.estimated_latency_samples()
    }

    /// 本图的卷积内核派发状态快照（后端 / 选择 / 实际路径）。
    ///
    /// 三个字段都是真实值：`backend` 是编译时 `SimdBackend::detect()` 的运行期 CPU 探测结果，
    /// `path` 是本图实际接的内核入口。status / 日志 / bench 应从这里读，不要自己再猜一个
    /// 后端名（也不要把"本机有 AVX2"写成"本图在跑 AVX2"——若图里没有卷积 lane 或选择了
    /// `Scalar`，`path` 会如实是 `scalar`）。
    pub fn kernel_dispatch(&self) -> KernelDispatch {
        self.kernel
    }

    /// 本图的编译期选择（`auto` / `scalar` / `optimized`）。
    pub fn kernel_choice(&self) -> KernelChoice {
        self.kernel.choice
    }

    /// 本图实际使用的卷积内核入口（`scalar` / `optimized`）。
    pub fn kernel_path(&self) -> KernelPath {
        self.kernel.path
    }

    /// 本机实际可用的 SIMD 后端（编译时探测值；`scalar` / `avx2+fma` / `neon`）。
    pub fn simd_backend(&self) -> SimdBackend {
        self.kernel.backend
    }
}

impl Processor for PlanGraph {
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let input_channels = self.input_channels;
        let output_channels = self.output_channels;
        // 输入块与输出块的样本总数不同（2 输入 / 22 输出时），帧数取两侧都能容纳的最小值。
        let input_frames = input.len() / input_channels;
        let output_frames = output.len() / output_channels;
        let mut frames_left = core::cmp::min(input_frames, output_frames);
        if frames_left == 0 {
            return;
        }
        let stride = self.chunk_frames;
        let mut cursor = 0usize;
        while frames_left > 0 {
            let frames = core::cmp::min(frames_left, stride);
            let in_chunk = &input[cursor * input_channels..(cursor + frames) * input_channels];
            let out_chunk =
                &mut output[cursor * output_channels..(cursor + frames) * output_channels];

            // 先解交错：帧交错输入 -> 每通道一个 planar 平面。
            let mut channel = 0;
            while channel < input_channels {
                let plane = &mut self.planes_in[channel * stride..channel * stride + frames];
                let mut frame = 0;
                while frame < frames {
                    plane[frame] = in_chunk[frame * input_channels + channel];
                    frame += 1;
                }
                channel += 1;
            }

            self.content
                .process(&self.planes_in, &mut self.planes_a, stride, frames);
            self.crossover
                .process(&self.planes_a, &mut self.planes_b, stride, frames);
            self.matrix
                .process(&self.planes_b, &mut self.planes_c, stride, frames);
            self.protection.process(&mut self.planes_c, stride, frames);
            // 再交错回写：routing 直接写帧交错输出块。
            self.routing
                .process(&self.planes_c, out_chunk, output_channels, stride, frames);

            cursor += frames;
            frames_left -= frames;
        }
    }
}
