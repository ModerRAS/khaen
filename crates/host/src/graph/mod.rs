//! 方案图执行器：把方案 ABI v3 编译成可执行的实时处理图。
//!
//! 链序严格固定为 content -> crossover -> matrix -> protection -> routing（见
//! `docs/DESIGN.md` 第 3 节的运行时数据面）：
//!
//! 1. **content**：容器通道到 lane 的静态映射与折叠增益；运行期不做动态混音。
//! 2. **crossover**：每 lane 一条全采样率 FIR，复用 `khaen_core::kernels::direct` 内核。
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
//! # T22 边界
//!
//! 已实现（T22）：矩阵 3 kHz 低速率域（每列抽取 / 每行插值的有理数重采样）与 matrix 段延迟估算。
//! 未实现（留给后续）：变比重采样（ASRC）、系数在块边界的原子切换、低速率域分区卷积
//! （`khaen_core::kernels::partitioned`）。本模块的低速率域只接受整倍因子。

use khaen_core::kernels::direct::DIRECT_MAX_BLOCK;
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
    /// 帧交错输入 / 输出通道数（= 方案输入通道数 = DAC 通道数）。
    channels: usize,
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
        let channels = config.channels;

        // lane 链：content -> crossover -> matrix -> protection -> routing；缺段按恒等段处理。
        let (in_channels, lanes_after_content) = match &plan.content {
            Some(content) => (content.in_channels as usize, content.out_lanes as usize),
            None => {
                let lanes = match &plan.crossover {
                    Some(crossover) => crossover.lanes as usize,
                    None => matrix_section.cols as usize,
                };
                (channels, lanes)
            }
        };
        if in_channels != channels {
            return Err(CompileError::ChannelMismatch {
                stage: "content",
                expected: channels,
                actual: in_channels,
            });
        }
        if plan.content.is_none() && lanes_after_content != channels {
            return Err(CompileError::ChannelMismatch {
                stage: "content",
                expected: channels,
                actual: lanes_after_content,
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
        if out_channels != channels {
            return Err(CompileError::ChannelMismatch {
                stage: "routing",
                expected: channels,
                actual: out_channels,
            });
        }

        // 每块最多处理的帧数；低速率域的每块样本数由它除以抽取因子得到（编译期确定）。
        let chunk_frames = config.block_frames.min(DIRECT_MAX_BLOCK);
        let content =
            ContentStage::compile(plan.content.as_ref(), in_channels, lanes_after_content)?;
        let crossover = CrossoverStage::compile(plan.crossover.as_ref(), lanes_after_crossover)?;
        let matrix =
            MatrixStage::compile(matrix_section, lanes_after_crossover, rows, chunk_frames)?;
        let protection = ProtectionStage::compile(plan.protection.as_ref(), rows)?;
        let routing = RoutingStage::compile(plan.routing.as_ref(), rows, out_channels)?;

        let planes_in = vec![0.0f32; in_channels * chunk_frames];
        let planes_a = vec![0.0f32; lanes_after_content * chunk_frames];
        let planes_b = vec![0.0f32; lanes_after_crossover * chunk_frames];
        let planes_c = vec![0.0f32; rows * chunk_frames];
        Ok(Self {
            channels,
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
}

impl Processor for PlanGraph {
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let channels = self.channels;
        let usable = core::cmp::min(input.len(), output.len());
        let mut frames_left = usable / channels;
        if frames_left == 0 {
            return;
        }
        let stride = self.chunk_frames;
        let mut cursor = 0usize;
        while frames_left > 0 {
            let frames = core::cmp::min(frames_left, stride);
            let in_chunk = &input[cursor * channels..(cursor + frames) * channels];
            let out_chunk = &mut output[cursor * channels..(cursor + frames) * channels];

            // 先解交错：帧交错输入 -> 每通道一个 planar 平面。
            let mut channel = 0;
            while channel < channels {
                let plane = &mut self.planes_in[channel * stride..channel * stride + frames];
                let mut frame = 0;
                while frame < frames {
                    plane[frame] = in_chunk[frame * channels + channel];
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
                .process(&self.planes_c, out_chunk, channels, stride, frames);

            cursor += frames;
            frames_left -= frames;
        }
    }
}
