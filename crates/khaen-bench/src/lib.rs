//! khaen 性能基线 bench（T38 基线 → T56 host 接 SIMD 派发）。
//!
//! 用 host 的图编译 / 执行 API（PlanGraph::compile_with_kernel + Processor::process）构造并
//! 计时实时链路，回答三个问题：
//!
//! 1. **3 kHz 低速率矩阵域（T22）到底省了多少**：同一组矩阵系数在全速率（F = 1）与
//!    3 kHz 域（F = 16，48 kHz / 16）下每 128 帧块的实测成本对比；
//! 2. **graph/crossover.rs 的卷积占多少预算**：每 lane 一条 ≤ 4096 taps 的全采样率 FIR，
//!    在 128 帧 / 2.667 ms 块预算下的占比；
//! 3. **host 图接上核心 SIMD 入口（T56）后整条链快多少**：同一方案分别用
//!    `KernelChoice::Scalar`（标量参考内核，bit-exact 口径）与 `KernelChoice::Optimized`
//!    （core 的 SIMD 入口）编译同一个 PlanGraph，报告两列耗时、加速比、budget 占比与
//!    两列输出的 parity（最大绝对差异，证明同一语义）。graph 档位与 kernel 档位都受
//!    `KernelMode` 控制：auto 给两列 + 加速比，scalar / optimized 只测一列。
//!
//! # 计时口径
//!
//! 计时对象是 PlanGraph::process（DSP 链本体）每块耗时，单位 µs：预热 warmup_blocks
//! 块丢弃，再逐块用 std::time::Instant 测 measure_blocks 块；每个档位重复 repeats 次，
//! 报告每次的 mean / p50 / p99 / p999 与跨次中位数。预算 = block_frames / sample_rate。
//! **不含**后端 capture / playout、线程调度、daemon 与设备开销，因此是链路成本的下界。
//!
//! # 构造
//!
//! 全部构造只用 khaen-core 的方案 ABI v3 结构与 khaen-host 的图 API；不引入第三方依赖。
//! 方案过 host 的准入预算（AdmissionLimits 默认值），超预算的构造会在编译期 fail-closed
//! （BenchError::Compile）。FIR 系数用窗函数法低通（Hann 窗、DC 归一化），只用于让链路有
//! 真实卷积负载，不影响计时结论。

use std::fmt;
use std::fmt::Write as _;
use std::time::Instant;

use khaen_core::kernels::direct::{DIRECT_MAX_BLOCK, DIRECT_MAX_TAPS};
use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_core::rt::SAMPLE_RATES_HZ;
use khaen_host::graph::{KernelChoice, KernelDispatch, KernelPath, CROSSOVER_PARTITIONED_MIN_TAPS};
use khaen_host::{
    admit, AdmissionLimits, Budget, CompileError, EngineConfig, PlanGraph, Processor,
    MAX_BLOCK_FRAMES,
};

pub mod kernel_bench;

pub use kernel_bench::KernelTierReport;

/// 输出数据的 schema 标识（人类可读报告与 --json 是同一份数据）。
///
/// v2（T56）：graph 档位从单列（当前接的内核）改成 scalar / optimized 双列 + 加速比 +
/// parity，新增 `graph_kernel` 段与 `B0-crossover96` 档；kernel 档位结构不变。
pub const SCHEMA: &str = "khaen-bench/v2";

/// 矩阵抽取 / 插值因子上限，与 host graph::matrix::MAX_MATRIX_RATE_FACTOR 一致；那里是
/// crate 内部常量，bench 只做入参校验，不复制实现。
const MAX_MATRIX_FACTOR: u8 = 64;

/// 3 kHz 域的代表因子：48 kHz / 16 = 3 kHz。
const THREE_KHZ_FACTOR: u8 = 16;

/// 默认非恒等矩阵子块边长：22 lane 链里取 16×16 子块，其余 lane 走 delta 直通。
const DEFAULT_ACTIVE_LANES: usize = 16;

/// crossover FIR 低通截止（相对采样率；0.1 = 48 kHz 下 4.8 kHz）。
const CROSSOVER_CUTOFF: f64 = 0.1;

/// 矩阵格 FIR 低通截止（相对低速率域采样率）。
const MATRIX_CUTOFF: f64 = 0.1;

/// 非对角矩阵格的混音增益：只用于让格子非零（真实矩阵混合），不影响计时。
const CROSS_GAIN: f32 = 0.02;

/// 计时器空转基线的测量次数。
const TIMER_OVERHEAD_SAMPLES: usize = 1000;

// ===========================================================================
// 配置
// ===========================================================================

/// 内核列模式（`--kernel`）：控制 graph 档位与 kernel 级对照段各测哪一列。
///
/// `Auto`（默认）同时测标量参考内核（graph: `KernelChoice::Scalar`；kernel: 标量入口）与
/// SIMD 优化内核（graph: `KernelChoice::Optimized`；kernel: `*_optimized` 入口）两列并给出
/// 加速比；`Scalar` / `Optimized` 只测一列（用于只想看一侧或在不便同时跑两列的机器上采集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KernelMode {
    /// 同时测标量参考与优化内核（默认；输出两列 + 加速比）。
    #[default]
    Auto,
    /// 只测标量参考内核。
    Scalar,
    /// 只测运行时派发的优化内核。
    Optimized,
}

impl KernelMode {
    /// 稳定名字（报告/JSON/CLI）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Scalar => "scalar",
            Self::Optimized => "optimized",
        }
    }

    /// 解析 CLI 值。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "scalar" => Ok(Self::Scalar),
            "optimized" => Ok(Self::Optimized),
            other => Err(format!(
                "kernel 参数 {other:?} 非法（支持 auto / scalar / optimized）"
            )),
        }
    }
}

/// bench 配置（默认即任务基线：128 帧 / 48 kHz / 22 lane / 2048-tap crossover / 3 kHz 矩阵）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchConfig {
    /// 每块帧数（默认 128）。
    pub block_frames: usize,
    /// 采样率（默认 48 kHz；支持 48 k / 96 k）。
    pub sample_rate: u32,
    /// 通道数 = lane 数（默认 22）。
    pub lanes: usize,
    /// crossover 每 lane 抽头数（默认 2048，上限 4096 = direct 内核上限）。
    pub crossover_taps: usize,
    /// matrix 每格抽头数（默认 8）。
    pub matrix_taps: u32,
    /// matrix 抽取 / 插值因子（默认 16 = 3 kHz 域；1 = 全速率对照）。
    pub matrix_factor: u8,
    /// 非恒等矩阵子块边长（默认 16，会 clamp 到 lanes）。
    pub matrix_active_lanes: usize,
    /// 每档预热块数（丢弃）。
    pub warmup_blocks: usize,
    /// 每档每次重复的测量块数。
    pub measure_blocks: usize,
    /// 每档重复次数。
    pub repeats: usize,
    /// kernel 级对照模式（默认 auto：同时测标量参考与优化内核两列）。
    pub kernel_mode: KernelMode,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            block_frames: 128,
            sample_rate: 48_000,
            lanes: 22,
            crossover_taps: 2048,
            matrix_taps: 8,
            matrix_factor: THREE_KHZ_FACTOR,
            matrix_active_lanes: DEFAULT_ACTIVE_LANES,
            warmup_blocks: 50,
            measure_blocks: 300,
            repeats: 3,
            kernel_mode: KernelMode::Auto,
        }
    }
}

impl BenchConfig {
    /// 单块时间预算（µs）= block_frames / sample_rate。
    pub fn budget_us(&self) -> f64 {
        self.block_frames as f64 * 1.0e6 / f64::from(self.sample_rate)
    }

    /// 交给 host 的引擎配置（通道数 = lane 数）。
    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            block_frames: self.block_frames,
            sample_rate: self.sample_rate,
            input_channels: self.lanes,
            output_channels: self.lanes,
        }
    }

    /// 每块样本数（帧交错）。
    pub fn samples_per_block(&self) -> usize {
        self.block_frames * self.lanes
    }

    /// 校验配置；错误信息给人看（命令行参数错误直接回显）。
    pub fn validate(&self) -> Result<(), String> {
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate) {
            return Err(format!(
                "sample_rate {} 不受支持（支持 {:?}）",
                self.sample_rate, SAMPLE_RATES_HZ
            ));
        }
        if self.block_frames == 0 || self.block_frames > MAX_BLOCK_FRAMES {
            return Err(format!(
                "block_frames {} 超出 1..={MAX_BLOCK_FRAMES}",
                self.block_frames
            ));
        }
        if self.lanes == 0 || self.lanes > 32 {
            return Err(format!("lanes {} 超出 1..=32", self.lanes));
        }
        if self.crossover_taps == 0 || self.crossover_taps > DIRECT_MAX_TAPS {
            return Err(format!(
                "crossover_taps {} 超出 1..={DIRECT_MAX_TAPS}（direct 内核上限）",
                self.crossover_taps
            ));
        }
        if self.matrix_taps == 0 || self.matrix_taps > 4096 {
            return Err(format!("matrix_taps {} 超出 1..=4096", self.matrix_taps));
        }
        if self.matrix_factor == 0 || self.matrix_factor > MAX_MATRIX_FACTOR {
            return Err(format!(
                "matrix_factor {} 超出 1..={MAX_MATRIX_FACTOR}",
                self.matrix_factor
            ));
        }
        // host 的 matrix 低速率域按 chunk_frames = min(block_frames, DIRECT_MAX_BLOCK) 切块，
        // 要求 chunk_frames % factor == 0；这里提前给出可读错误，而不是等图编译失败。
        let chunk_frames = self.block_frames.min(DIRECT_MAX_BLOCK);
        if self.matrix_factor > 1 && !is_multiple(chunk_frames, usize::from(self.matrix_factor)) {
            return Err(format!(
                "chunk_frames {chunk_frames} 不能被 matrix_factor {} 整除（低速率域要求整倍块）",
                self.matrix_factor
            ));
        }
        if self.measure_blocks == 0 {
            return Err("measure_blocks 必须 >= 1".to_string());
        }
        if self.repeats == 0 {
            return Err("repeats 必须 >= 1".to_string());
        }
        Ok(())
    }
}

/// `value % factor == 0`。
///
/// workspace MSRV 1.80：`usize::is_multiple_of` 自 1.87 才稳定，这里保持取模写法并关掉
/// 对应的 clippy 建议（与 host `graph::matrix` 的处理一致）。
#[allow(clippy::manual_is_multiple_of)]
fn is_multiple(value: usize, factor: usize) -> bool {
    value % factor == 0
}

// ===========================================================================
// 构造信息
// ===========================================================================

/// 一个档位的构造信息（打印 / JSON 都从这里取，保证"报出的数字和构造对应"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Construction {
    /// lane 数。
    pub lanes: usize,
    /// 每块帧数。
    pub block_frames: usize,
    /// 采样率。
    pub sample_rate: u32,
    /// crossover 声明的 lane 数。
    pub crossover_lanes: usize,
    /// crossover 每 lane 抽头数（第一条滤波器的长度）。
    pub crossover_taps_per_lane: usize,
    /// crossover 全部抽头数（各 lane 之和）。
    pub crossover_total_taps: usize,
    /// matrix 行数。
    pub matrix_rows: usize,
    /// matrix 列数。
    pub matrix_cols: usize,
    /// matrix 每格抽头数。
    pub matrix_taps: u32,
    /// matrix 抽取 / 插值因子。
    pub matrix_factor: u8,
    /// 非零矩阵格数（编译期会跳过全零格）。
    pub matrix_active_cells: usize,
    /// host 准入预算（估算值，不是实测）。
    pub budget: Budget,
}

impl Construction {
    /// 单行人类可读描述。
    pub fn describe(&self) -> String {
        format!(
            "lanes={} block={} frames @ {} Hz | crossover: {} lanes × {} taps（total {} taps） | \
             matrix: {}×{} taps={} factor={} active_cells={} | admission: coefficients={} \
             conv_taps={} state_samples={} latency_est={} samples plan_bytes={} memory_est={} B",
            self.lanes,
            self.block_frames,
            self.sample_rate,
            self.crossover_lanes,
            self.crossover_taps_per_lane,
            self.crossover_total_taps,
            self.matrix_rows,
            self.matrix_cols,
            self.matrix_taps,
            self.matrix_factor,
            self.matrix_active_cells,
            self.budget.coefficients,
            self.budget.convolution_taps,
            self.budget.state_samples,
            self.budget.latency_samples,
            self.budget.plan_bytes,
            self.budget.memory_bytes,
        )
    }
}

// ===========================================================================
// 档位
// ===========================================================================

/// 一个基准档位：名字、目的、方案、编译配置与构造信息。
#[derive(Debug, Clone)]
pub struct Scenario {
    /// 稳定名字（报告与 JSON 的主键）。
    pub name: &'static str,
    /// 该档位要验证什么。
    pub purpose: String,
    /// 方案 ABI v3。
    pub plan: Plan,
    /// 图编译配置。
    pub engine_config: EngineConfig,
    /// 构造信息。
    pub construction: Construction,
}

impl Scenario {
    /// 编译为可执行图（准入失败 / 超预算在这里 fail-closed；`KernelChoice::Auto`）。
    pub fn compile(&self, limits: &AdmissionLimits) -> Result<PlanGraph, CompileError> {
        PlanGraph::compile(&self.plan, &self.engine_config, limits)
    }

    /// 按显式内核选择编译为可执行图（标量列 / SIMD 列对照用）。
    pub fn compile_with_kernel(
        &self,
        limits: &AdmissionLimits,
        choice: KernelChoice,
    ) -> Result<PlanGraph, CompileError> {
        PlanGraph::compile_with_kernel(&self.plan, &self.engine_config, limits, choice)
    }
}

/// 构造全部基准档位（A / B0 / B / B2 / C / D / E / C1 / E1，见各档 purpose）。
pub fn scenarios(config: &BenchConfig) -> Vec<Scenario> {
    let taps = config.matrix_taps;
    let factor = config.matrix_factor;
    let low_rate_hz = config.sample_rate / u32::from(factor.max(1));
    let active = config.matrix_active_lanes.min(config.lanes);
    let mut list = Vec::with_capacity(9);

    list.push(make_scenario(
        "A-identity",
        "identity 全链路（content / crossover / matrix / protection / routing 全部折叠为逐样本直通），对照基线"
            .to_string(),
        identity_plan(config),
        config,
    ));
    // 阈值（96 taps）上的 direct 直算路径：T56 起接 core 的 SIMD A-FIR（之前是标量 direct）。
    list.push(make_scenario(
        "B0-crossover96",
        format!(
            "每 lane 一条 {CROSSOVER_PARTITIONED_MIN_TAPS}-tap FIR（= 分区阈值，graph/crossover.rs 的 \
             direct 直算路径），matrix identity：T56 的 direct 路径 SIMD 收益档"
        ),
        crossover_plan(config, CROSSOVER_PARTITIONED_MIN_TAPS),
        config,
    ));
    list.push(make_scenario(
        "B-crossover2048",
        format!(
            "每 lane 一条 {}-tap crossover FIR（> 阈值：分区 UPOLS 路径），matrix identity",
            config.crossover_taps
        ),
        crossover_plan(config, config.crossover_taps),
        config,
    ));
    list.push(make_scenario(
        "B2-crossover4096",
        format!(
            "每 lane 一条 {DIRECT_MAX_TAPS}-tap crossover FIR（> 阈值：分区 UPOLS 路径，DIRECT_MAX_TAPS 上限），matrix identity"
        ),
        crossover_plan(config, DIRECT_MAX_TAPS),
        config,
    ));
    list.push(make_scenario(
        "C-matrix3k",
        format!(
            "matrix 低速率域 {} Hz（factor={}）：{}×{} 矩阵，{}×{} 子块非恒等（{}-tap/格），其余 lane delta 直通",
            low_rate_hz, factor, config.lanes, config.lanes, active, active, taps
        ),
        matrix_plan(config, taps, factor),
        config,
    ));
    list.push(make_scenario(
        "D-crossover+matrix3k",
        format!(
            "B + C：{}-tap crossover FIR 与 {} Hz 矩阵同时在场（真实候选链路）",
            config.crossover_taps, low_rate_hz
        ),
        combined_plan(config, config.crossover_taps, taps, factor),
        config,
    ));
    list.push(make_scenario(
        "E-matrix-full-rate",
        format!(
            "C 的全速率对照（同一组矩阵系数、taps={}、F=1）：量化 3 kHz 域收益",
            taps
        ),
        matrix_plan(config, taps, 1),
        config,
    ));
    list.push(make_scenario(
        "C1-matrix3k-1tap",
        format!(
            "matrix {} Hz（factor={}）、每格 1 tap 的静态混音（非恒等）：验证小矩阵下 3 kHz 域是否仍划算",
            low_rate_hz, factor
        ),
        matrix_plan(config, 1, factor),
        config,
    ));
    list.push(make_scenario(
        "E1-matrix-full-rate-1tap",
        "C1 的全速率对照（taps=1、F=1）：与 C1 成对，量化重采样固定成本".to_string(),
        matrix_plan(config, 1, 1),
        config,
    ));
    list
}

fn make_scenario(
    name: &'static str,
    purpose: String,
    plan: Plan,
    config: &BenchConfig,
) -> Scenario {
    Scenario {
        name,
        purpose,
        construction: construction_of(&plan, config),
        engine_config: config.engine_config(),
        plan,
    }
}

fn construction_of(plan: &Plan, config: &BenchConfig) -> Construction {
    let budget = admit(plan, &AdmissionLimits::default()).budget;
    let (crossover_lanes, crossover_taps_per_lane, crossover_total_taps) = match &plan.crossover {
        Some(crossover) => (
            crossover.lanes as usize,
            crossover
                .filters
                .first()
                .map_or(0, |filter| filter.coeffs.len()),
            crossover
                .filters
                .iter()
                .map(|filter| filter.coeffs.len())
                .sum(),
        ),
        None => (0, 0, 0),
    };
    let (matrix_rows, matrix_cols, matrix_taps, matrix_factor, matrix_active_cells) =
        match &plan.matrix {
            Some(matrix) => (
                matrix.rows as usize,
                matrix.cols as usize,
                matrix.taps,
                matrix.decimation,
                count_active_cells(matrix),
            ),
            None => (0, 0, 0, 1, 0),
        };
    Construction {
        lanes: config.lanes,
        block_frames: config.block_frames,
        sample_rate: config.sample_rate,
        crossover_lanes,
        crossover_taps_per_lane,
        crossover_total_taps,
        matrix_rows,
        matrix_cols,
        matrix_taps,
        matrix_factor,
        matrix_active_cells,
        budget,
    }
}

fn count_active_cells(matrix: &MatrixSection) -> usize {
    let taps = matrix.taps as usize;
    if taps == 0 {
        return 0;
    }
    matrix
        .coeffs
        .chunks_exact(taps)
        .filter(|cell| cell.iter().any(|coeff| *coeff != 0.0))
        .count()
}

// ===========================================================================
// 方案构造
// ===========================================================================

fn identity_content(lanes: usize) -> ContentSection {
    ContentSection {
        in_channels: lanes as u16,
        out_lanes: lanes as u16,
        folds: (0..lanes)
            .map(|lane| Fold {
                src: lane as u16,
                dst: lane as u16,
                gain: 1.0,
            })
            .collect(),
    }
}

fn identity_crossover(lanes: usize) -> CrossoverSection {
    CrossoverSection {
        lanes: lanes as u16,
        filters: (0..lanes)
            .map(|lane| CrossoverFilter {
                lane: lane as u16,
                coeffs: vec![1.0],
            })
            .collect(),
    }
}

fn identity_matrix(lanes: usize) -> MatrixSection {
    let mut coeffs = vec![0.0f32; lanes * lanes];
    for lane in 0..lanes {
        coeffs[lane * lanes + lane] = 1.0;
    }
    MatrixSection {
        rows: lanes as u16,
        cols: lanes as u16,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs,
    }
}

fn identity_protection(lanes: usize) -> ProtectionSection {
    ProtectionSection {
        lanes: lanes as u16,
        entries: (0..lanes)
            .map(|lane| ProtectionEntry {
                lane: lane as u16,
                delay: 0,
                polarity: 1,
                gain_db: 0.0,
                peq: Vec::new(),
            })
            .collect(),
    }
}

fn identity_routing(lanes: usize) -> RoutingSection {
    RoutingSection {
        dac_channels: lanes as u16,
        routes: (0..lanes).map(|lane| (lane as u16, lane as u16)).collect(),
    }
}

fn base_plan(config: &BenchConfig, crossover: CrossoverSection, matrix: MatrixSection) -> Plan {
    Plan {
        block_size: config.block_frames as u32,
        sample_rate: config.sample_rate,
        content: Some(identity_content(config.lanes)),
        crossover: Some(crossover),
        matrix: Some(matrix),
        protection: Some(identity_protection(config.lanes)),
        routing: Some(identity_routing(config.lanes)),
    }
}

/// identity 全链路（用于 A 档，也验证 identity 判据）。
pub fn identity_plan(config: &BenchConfig) -> Plan {
    base_plan(
        config,
        identity_crossover(config.lanes),
        identity_matrix(config.lanes),
    )
}

/// 每 lane 一条 taps 抽头全采样率 FIR，matrix identity。
pub fn crossover_plan(config: &BenchConfig, taps: usize) -> Plan {
    let coeffs = lowpass_fir(taps, CROSSOVER_CUTOFF);
    let filters = (0..config.lanes)
        .map(|lane| CrossoverFilter {
            lane: lane as u16,
            coeffs: coeffs.clone(),
        })
        .collect();
    base_plan(
        config,
        CrossoverSection {
            lanes: config.lanes as u16,
            filters,
        },
        identity_matrix(config.lanes),
    )
}

/// 矩阵档：lanes × lanes 满矩阵，matrix_active_lanes 子块非恒等、其余 lane delta 直通。
pub fn matrix_plan(config: &BenchConfig, taps: u32, factor: u8) -> Plan {
    let active = config.matrix_active_lanes.min(config.lanes);
    let (coeffs, _) = matrix_coefficients(config.lanes, taps, active);
    base_plan(
        config,
        identity_crossover(config.lanes),
        MatrixSection {
            rows: config.lanes as u16,
            cols: config.lanes as u16,
            decimation: factor,
            interpolation: factor,
            taps,
            coeffs,
        },
    )
}

/// B + C 组合档。
pub fn combined_plan(
    config: &BenchConfig,
    crossover_taps: usize,
    matrix_taps: u32,
    factor: u8,
) -> Plan {
    let coeffs = lowpass_fir(crossover_taps, CROSSOVER_CUTOFF);
    let filters = (0..config.lanes)
        .map(|lane| CrossoverFilter {
            lane: lane as u16,
            coeffs: coeffs.clone(),
        })
        .collect();
    let active = config.matrix_active_lanes.min(config.lanes);
    let (matrix_coeffs, _) = matrix_coefficients(config.lanes, matrix_taps, active);
    base_plan(
        config,
        CrossoverSection {
            lanes: config.lanes as u16,
            filters,
        },
        MatrixSection {
            rows: config.lanes as u16,
            cols: config.lanes as u16,
            decimation: factor,
            interpolation: factor,
            taps: matrix_taps,
            coeffs: matrix_coeffs,
        },
    )
}

/// 窗函数法低通 FIR：Hann 窗、DC 归一化为 1。系数只影响信号、不影响计时。
pub fn lowpass_fir(taps: usize, cutoff_cycles_per_sample: f64) -> Vec<f32> {
    if taps == 0 {
        return Vec::new();
    }
    if taps == 1 {
        return vec![1.0];
    }
    let middle = (taps - 1) as f64 / 2.0;
    let mut coeffs = Vec::with_capacity(taps);
    let mut sum = 0.0f64;
    for index in 0..taps {
        let offset = index as f64 - middle;
        let angle = 2.0 * std::f64::consts::PI * cutoff_cycles_per_sample * offset;
        let sinc = if angle.abs() < 1.0e-12 {
            1.0
        } else {
            angle.sin() / angle
        };
        let window =
            0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / (taps - 1) as f64).cos();
        let value = 2.0 * cutoff_cycles_per_sample * sinc * window;
        coeffs.push(value);
        sum += value;
    }
    if sum.abs() > f64::EPSILON {
        for value in &mut coeffs {
            *value /= sum;
        }
    }
    coeffs.into_iter().map(|value| value as f32).collect()
}

/// 矩阵系数（行优先、rows*cols*taps）：子块内对角低通 + 非对角延迟混音；子块外对角 delta、
/// 其余 0（编译期按 silent 跳过）。返回系数与活跃格数。
fn matrix_coefficients(lanes: usize, taps: u32, active: usize) -> (Vec<f32>, usize) {
    let taps = taps as usize;
    let diagonal = lowpass_fir(taps, MATRIX_CUTOFF);
    let mut coeffs = vec![0.0f32; lanes * lanes * taps];
    let mut active_cells = 0usize;
    for row in 0..lanes {
        for col in 0..lanes {
            let base = (row * lanes + col) * taps;
            if row == col {
                if row < active {
                    coeffs[base..base + taps].copy_from_slice(&diagonal);
                } else {
                    coeffs[base] = 1.0;
                }
                active_cells += 1;
            } else if row < active && col < active {
                let delay = if taps > 1 { 1 } else { 0 };
                coeffs[base + delay] = CROSS_GAIN;
                active_cells += 1;
            }
        }
    }
    (coeffs, active_cells)
}

// ===========================================================================
// 平台
// ===========================================================================

/// 平台信息（OS / 架构 / CPU 型号 / 逻辑核数 / 构建 profile），不引入第三方 crate。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    /// std::env::consts::OS。
    pub os: &'static str,
    /// std::env::consts::ARCH。
    pub arch: &'static str,
    /// CPU 型号（Windows 取 PROCESSOR_IDENTIFIER，Linux 取 /proc/cpuinfo）。
    pub cpu: String,
    /// 逻辑核数（NUMBER_OF_PROCESSORS 或 available_parallelism）。
    pub logical_cores: Option<usize>,
    /// 构建 profile（debug / release）。
    pub build_profile: &'static str,
}

impl Platform {
    /// 探测本机平台。
    pub fn detect() -> Self {
        let cpu = std::env::var("PROCESSOR_IDENTIFIER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(cpu_model_from_proc)
            .unwrap_or_else(|| "unknown".to_string());
        let logical_cores = std::env::var("NUMBER_OF_PROCESSORS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .or_else(|| {
                std::thread::available_parallelism()
                    .ok()
                    .map(|count| count.get())
            });
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cpu,
            logical_cores,
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        }
    }
}

#[cfg(target_os = "linux")]
fn cpu_model_from_proc() -> Option<String> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            if let Some((_, model)) = rest.split_once(':') {
                let model = model.trim();
                if !model.is_empty() {
                    return Some(model.to_string());
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn cpu_model_from_proc() -> Option<String> {
    None
}

// ===========================================================================
// 统计
// ===========================================================================

/// 一个档位一次重复的统计（µs/块）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TierStats {
    /// 测量块数。
    pub blocks: usize,
    /// 均值。
    pub mean_us: f64,
    /// 中位数（p50）。
    pub p50_us: f64,
    /// p99。
    pub p99_us: f64,
    /// p999。
    pub p999_us: f64,
    /// 最小。
    pub min_us: f64,
    /// 最大。
    pub max_us: f64,
    /// 均值占单块预算的百分比。
    pub budget_pct_mean: f64,
    /// p99 占单块预算的百分比。
    pub budget_pct_p99: f64,
}

impl TierStats {
    /// 空统计（测量块数为 0 时用；正常路径不会出现）。
    pub(crate) fn empty() -> Self {
        Self {
            blocks: 0,
            mean_us: 0.0,
            p50_us: 0.0,
            p99_us: 0.0,
            p999_us: 0.0,
            min_us: 0.0,
            max_us: 0.0,
            budget_pct_mean: 0.0,
            budget_pct_p99: 0.0,
        }
    }
}

/// graph 档位的一列（scalar 或 optimized）：每次重复的统计 + 跨次汇总 + 离散度。
#[derive(Debug, Clone, PartialEq)]
pub struct GraphColumn {
    /// 该列实际使用的内核入口名（`scalar` / `optimized`）。
    pub path: &'static str,
    /// 每次重复的统计。
    pub repeats: Vec<TierStats>,
    /// 跨次中位数统计。
    pub aggregate: TierStats,
    /// 各次 p50 的离散度（max-min）/ median（%）。
    pub spread_pct: f64,
}

/// 一个 graph 档位的完整报告：同一方案的 scalar 列 / optimized 列 + 加速比 + parity。
///
/// 两列是**同一个 Plan、同一输入**、只换编译期内核选择（`KernelChoice::Scalar` vs
/// `KernelChoice::Optimized`）得到的；parity 是两列从零 state 起跑同一段流后同一块的
/// 输出最大绝对差异（证明只差浮点舍入，不是换了语义）。
#[derive(Debug, Clone, PartialEq)]
pub struct TierReport {
    /// 档位名。
    pub name: String,
    /// 该档位验证什么。
    pub purpose: String,
    /// 构造信息。
    pub construction: Construction,
    /// matrix 段估算延迟（全采样率样本；crossover / protection 不在内）。
    pub estimated_latency_samples: u64,
    /// 本机派发后端（真实探测值：scalar / avx2+fma / neon）。
    pub backend: String,
    /// [`KernelMode::Auto`] 在本机解析出的优化列路径（scalar 机器上仍会回退标量）。
    pub auto_path: &'static str,
    /// 标量参考内核列（`KernelChoice::Scalar`）。
    pub scalar: Option<GraphColumn>,
    /// 核心 SIMD 优化入口列（`KernelChoice::Optimized`）。
    pub optimized: Option<GraphColumn>,
    /// scalar.p50 / optimized.p50（> 1 表示优化后更快）。
    pub speedup_p50: Option<f64>,
    /// 同上，p99。
    pub speedup_p99: Option<f64>,
    /// 同上，min。
    pub speedup_min: Option<f64>,
    /// 两列输出最大绝对差异（0 = 逐位相同，例如 identity 档或矩阵段未接 SIMD）。
    pub parity_max_abs: f32,
}

impl TierReport {
    /// 某一列的跨次汇总（没有测该列时为 None）。
    pub fn column(&self, path: KernelPath) -> Option<&GraphColumn> {
        match path {
            KernelPath::Scalar => self.scalar.as_ref(),
            KernelPath::Optimized => self.optimized.as_ref(),
        }
    }
}

/// 一次完整 bench 的报告。
#[derive(Debug, Clone, PartialEq)]
pub struct BenchReport {
    /// 平台。
    pub platform: Platform,
    /// 配置。
    pub config: BenchConfig,
    /// 单块预算（µs）。
    pub budget_us: f64,
    /// 计时器空转基线（µs/块，已从每块样本中扣除）。
    pub timer_overhead_us: f64,
    /// 本机内核派发后端（scalar / avx2+fma / neon；host graph 的 `KernelDispatch` 真实探测值）。
    pub kernel_backend: String,
    /// `KernelChoice::Auto` 在本机解析出的路径（scalar / optimized）。
    pub graph_auto_path: String,
    /// 各 graph 档位报告（按 scenarios 顺序；计时对象 PlanGraph::process）。
    pub tiers: Vec<TierReport>,
    /// kernel 级对照（同一 workload 的标量参考内核 vs SIMD 优化内核）。
    pub kernel_tiers: Vec<KernelTierReport>,
}

// ===========================================================================
// 运行
// ===========================================================================

/// bench 错误。
#[derive(Debug)]
pub enum BenchError {
    /// 配置非法。
    Config(String),
    /// 某个档位的图编译失败（含准入拒绝）。
    Compile {
        /// 档位名。
        tier: &'static str,
        /// host 编译错误（含准入拒绝理由）。
        error: CompileError,
    },
}

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(formatter, "配置错误: {message}"),
            Self::Compile { tier, error } => write!(formatter, "档位 {tier} 图编译失败: {error}"),
        }
    }
}

impl std::error::Error for BenchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Compile { error, .. } => Some(error),
            Self::Config(_) => None,
        }
    }
}

/// 跑全部档位，返回报告。计时对象是 PlanGraph::process 每块耗时。
///
/// graph 档位按 [`KernelMode`] 测一列或两列：两列是同一个 Plan、同一输入，只换编译期的
/// 内核选择（`KernelChoice::Scalar` / `KernelChoice::Optimized`）。parity 用两列各自的
/// 同一条输入流（从零 state 起跑）最后一块输出求最大绝对差异，mouth 证明只差浮点舍入。
pub fn run(config: &BenchConfig) -> Result<BenchReport, BenchError> {
    config.validate().map_err(BenchError::Config)?;
    let limits = AdmissionLimits::default();
    let budget_us = config.budget_us();
    let platform = Platform::detect();
    let timer_overhead_us = measure_timer_overhead(TIMER_OVERHEAD_SAMPLES);
    // 本机派发快照从 host graph 读（T56 ③：真实探测值，不自己编）。
    let dispatch = KernelDispatch::detect(KernelChoice::Auto);
    let measure_scalar = matches!(config.kernel_mode, KernelMode::Auto | KernelMode::Scalar);
    let measure_optimized = matches!(config.kernel_mode, KernelMode::Auto | KernelMode::Optimized);
    let mut tiers = Vec::with_capacity(9);
    for (index, scenario) in scenarios(config).into_iter().enumerate() {
        // 两列用同一条确定性输入（同一 seed、同一波形），parity 才有意义。
        let seed = 0x9e37_79b9_u32.wrapping_add((index as u32).wrapping_mul(0x85eb_ca6b));
        let estimated_latency_samples = scenario
            .compile_with_kernel(&limits, KernelChoice::Scalar)
            .map_err(|error| BenchError::Compile {
                tier: scenario.name,
                error,
            })?
            .estimated_latency_samples();
        let scalar = if measure_scalar {
            Some(run_graph_column(
                &scenario,
                KernelChoice::Scalar,
                config,
                seed,
                budget_us,
                timer_overhead_us,
                &limits,
            )?)
        } else {
            None
        };
        let optimized = if measure_optimized {
            Some(run_graph_column(
                &scenario,
                KernelChoice::Optimized,
                config,
                seed,
                budget_us,
                timer_overhead_us,
                &limits,
            )?)
        } else {
            None
        };
        let parity_max_abs = graph_parity(&scenario, config, seed, &limits)?;
        let (speedup_p50, speedup_p99, speedup_min) = match (scalar.as_ref(), optimized.as_ref()) {
            (Some(scalar), Some(optimized)) => (
                Some(scalar.aggregate.p50_us / optimized.aggregate.p50_us),
                Some(scalar.aggregate.p99_us / optimized.aggregate.p99_us),
                Some(scalar.aggregate.min_us / optimized.aggregate.min_us),
            ),
            _ => (None, None, None),
        };
        tiers.push(TierReport {
            name: scenario.name.to_string(),
            purpose: scenario.purpose,
            construction: scenario.construction,
            estimated_latency_samples,
            backend: dispatch.backend_name().to_string(),
            auto_path: dispatch.path_name(),
            scalar,
            optimized,
            speedup_p50,
            speedup_p99,
            speedup_min,
            parity_max_abs,
        });
    }
    let kernel_tiers = kernel_bench::run_kernel_tiers(config, budget_us, timer_overhead_us);
    Ok(BenchReport {
        platform,
        config: *config,
        budget_us,
        timer_overhead_us,
        kernel_backend: dispatch.backend_name().to_string(),
        graph_auto_path: dispatch.path_name().to_string(),
        tiers,
        kernel_tiers,
    })
}

/// 跑 graph 档位的一列：按 `choice` 编译图，预热 + 逐块计时，重复 `config.repeats` 次。
#[allow(clippy::too_many_arguments)]
fn run_graph_column(
    scenario: &Scenario,
    choice: KernelChoice,
    config: &BenchConfig,
    seed: u32,
    budget_us: f64,
    timer_overhead_us: f64,
    limits: &AdmissionLimits,
) -> Result<GraphColumn, BenchError> {
    let mut input = vec![0.0f32; config.samples_per_block()];
    fill_signal(&mut input, seed);
    let mut output = vec![0.0f32; config.samples_per_block()];
    let mut repeats = Vec::with_capacity(config.repeats);
    for _ in 0..config.repeats {
        // 每次重复重新编译：state 从零开始，跨次互不影响。
        let mut graph = scenario
            .compile_with_kernel(limits, choice)
            .map_err(|error| BenchError::Compile {
                tier: scenario.name,
                error,
            })?;
        repeats.push(measure_graph(
            &mut graph,
            &input,
            &mut output,
            config.warmup_blocks,
            config.measure_blocks,
            budget_us,
            timer_overhead_us,
        ));
    }
    let aggregate = aggregate(&repeats, budget_us);
    let spread_pct = spread_pct(&repeats);
    Ok(GraphColumn {
        path: choice.resolve_path().name(),
        repeats,
        aggregate,
        spread_pct,
    })
}

/// 两列 parity：同一输入流、同一块数，从零 state 起跑后比较最后一块输出。
///
/// 预热 `warmup_blocks` 块让历史进入稳态，再处理一块并比较（图内无反馈，两列只差内核的
/// 浮点舍入，因此逐块差异有界；identity 档两列同一实现，差异为 0）。
fn graph_parity(
    scenario: &Scenario,
    config: &BenchConfig,
    seed: u32,
    limits: &AdmissionLimits,
) -> Result<f32, BenchError> {
    let mut input = vec![0.0f32; config.samples_per_block()];
    fill_signal(&mut input, seed);
    let mut scalar_graph = scenario
        .compile_with_kernel(limits, KernelChoice::Scalar)
        .map_err(|error| BenchError::Compile {
            tier: scenario.name,
            error,
        })?;
    let mut optimized_graph = scenario
        .compile_with_kernel(limits, KernelChoice::Optimized)
        .map_err(|error| BenchError::Compile {
            tier: scenario.name,
            error,
        })?;
    let mut scalar_output = vec![0.0f32; config.samples_per_block()];
    let mut optimized_output = vec![0.0f32; config.samples_per_block()];
    let warmup = config.warmup_blocks.max(1);
    for _ in 0..warmup {
        scalar_graph.process(&input, &mut scalar_output);
        optimized_graph.process(&input, &mut optimized_output);
    }
    scalar_graph.process(&input, &mut scalar_output);
    optimized_graph.process(&input, &mut optimized_output);
    let parity = scalar_output
        .iter()
        .zip(optimized_output.iter())
        .map(|(scalar, optimized)| (scalar - optimized).abs())
        .fold(0.0f32, f32::max);
    std::hint::black_box(parity);
    Ok(parity)
}

/// 确定性伪随机输入（LCG；只用整数运算与 f32 除法，跨平台一致）。
pub(crate) fn fill_signal(buffer: &mut [f32], seed: u32) {
    let mut state = seed | 1;
    for sample in buffer.iter_mut() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = (state >> 8) as f32 / (1u32 << 24) as f32;
        *sample = unit * 2.0 - 1.0;
    }
}

/// 计时器空转基线：Instant::now() + elapsed() 的成对开销中位数（µs）。
fn measure_timer_overhead(iterations: usize) -> f64 {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let elapsed = start.elapsed();
        samples.push(elapsed.as_secs_f64() * 1.0e6);
    }
    median_of(samples)
}

/// 计时一个图：预热 warmup 块丢弃，再逐块测 measure 块。
#[allow(clippy::too_many_arguments)]
fn measure_graph(
    graph: &mut PlanGraph,
    input: &[f32],
    output: &mut [f32],
    warmup: usize,
    measure: usize,
    budget_us: f64,
    timer_overhead_us: f64,
) -> TierStats {
    for _ in 0..warmup {
        graph.process(input, output);
    }
    let mut samples = Vec::with_capacity(measure);
    for _ in 0..measure {
        let start = Instant::now();
        graph.process(input, output);
        let elapsed_us = start.elapsed().as_secs_f64() * 1.0e6 - timer_overhead_us;
        samples.push(elapsed_us.max(0.0));
    }
    // 让编译器不能把最后一块输出当死代码消掉。
    std::hint::black_box(output.first().copied().unwrap_or(0.0));
    stats_from_samples(samples, budget_us)
}

pub(crate) fn stats_from_samples(mut samples: Vec<f64>, budget_us: f64) -> TierStats {
    if samples.is_empty() {
        return TierStats::empty();
    }
    samples.sort_by(f64::total_cmp);
    let count = samples.len();
    let mean_us = samples.iter().sum::<f64>() / count as f64;
    let p50_us = percentile(&samples, 0.50);
    let p99_us = percentile(&samples, 0.99);
    let p999_us = percentile(&samples, 0.999);
    TierStats {
        blocks: count,
        mean_us,
        p50_us,
        p99_us,
        p999_us,
        min_us: samples[0],
        max_us: samples[count - 1],
        budget_pct_mean: mean_us / budget_us * 100.0,
        budget_pct_p99: p99_us / budget_us * 100.0,
    }
}

/// 最近秩百分位（sorted 必须已升序）：index = ceil(fraction * n) - 1。
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

pub(crate) fn aggregate(repeats: &[TierStats], budget_us: f64) -> TierStats {
    if repeats.is_empty() {
        return TierStats::empty();
    }
    let mean_us = median_of(repeats.iter().map(|stats| stats.mean_us).collect());
    let p50_us = median_of(repeats.iter().map(|stats| stats.p50_us).collect());
    let p99_us = median_of(repeats.iter().map(|stats| stats.p99_us).collect());
    let p999_us = median_of(repeats.iter().map(|stats| stats.p999_us).collect());
    let min_us = repeats
        .iter()
        .map(|stats| stats.min_us)
        .fold(f64::INFINITY, f64::min);
    let max_us = repeats
        .iter()
        .map(|stats| stats.max_us)
        .fold(f64::NEG_INFINITY, f64::max);
    TierStats {
        blocks: repeats[0].blocks,
        mean_us,
        p50_us,
        p99_us,
        p999_us,
        min_us,
        max_us,
        budget_pct_mean: mean_us / budget_us * 100.0,
        budget_pct_p99: p99_us / budget_us * 100.0,
    }
}

#[allow(clippy::manual_is_multiple_of)]
fn median_of(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

pub(crate) fn spread_pct(repeats: &[TierStats]) -> f64 {
    if repeats.len() < 2 {
        return 0.0;
    }
    let p50s: Vec<f64> = repeats.iter().map(|stats| stats.p50_us).collect();
    let median = median_of(p50s.clone());
    if median <= 0.0 {
        return 0.0;
    }
    let min = p50s.iter().copied().fold(f64::INFINITY, f64::min);
    let max = p50s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (max - min) / median * 100.0
}

// ===========================================================================
// 报告输出
// ===========================================================================

impl BenchReport {
    /// 人类可读表（平台 / 配置 / 计时口径 / 各档统计 / 构造信息）。
    pub fn human_report(&self) -> String {
        let mut out = String::with_capacity(8192);
        let cores = self
            .platform
            .logical_cores
            .map_or_else(|| "unknown".to_string(), |cores| cores.to_string());
        let _ = writeln!(
            out,
            "khaen 性能基线 bench（T38 基线 + T39a kernel SIMD + T56 host graph SIMD 派发对照） \
             — schema {SCHEMA}（--json 输出同一份数据）"
        );
        let _ = writeln!(
            out,
            "platform: os={} arch={} cpu={} logical_cores={} build={}",
            self.platform.os,
            self.platform.arch,
            self.platform.cpu,
            cores,
            self.platform.build_profile
        );
        let _ = writeln!(
            out,
            "config: block={} frames @ {} Hz（预算 {:.3} ms/块）；lanes={}；crossover={} taps/lane；\
             matrix taps={} factor={} active_lanes={}；kernel={}（backend {}）",
            self.config.block_frames,
            self.config.sample_rate,
            self.budget_us / 1000.0,
            self.config.lanes,
            self.config.crossover_taps,
            self.config.matrix_taps,
            self.config.matrix_factor,
            self.config.matrix_active_lanes,
            self.config.kernel_mode.name(),
            self.kernel_backend
        );
        let _ = writeln!(
            out,
            "method: PlanGraph::process 每块耗时；warmup={} 块丢弃、measure={} 块/次、repeats={}；\
             计时器开销 {:.3} µs/块（已扣除）；不含后端 capture/playout、线程调度与 daemon 开销；\
             min 列是本次最安静单块（能力下界，共享机器上比 p50 更接近无争用成本）",
            self.config.warmup_blocks,
            self.config.measure_blocks,
            self.config.repeats,
            self.timer_overhead_us
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "\ngraph 档位（计时对象 PlanGraph::process 每块；scalar = KernelChoice::Scalar \
             （khaen-core 标量参考内核，bit-exact 口径），optimized = KernelChoice::Optimized \
             （core 的 SIMD 派发入口）；backend={}，Auto 解析出的路径={}）：",
            self.kernel_backend, self.graph_auto_path
        );
        let cell = |column: Option<GraphColumn>, field: fn(&TierStats) -> f64| match column {
            Some(column) => format!("{:.2}", field(&column.aggregate)),
            None => "-".to_string(),
        };
        let pct = |column: Option<GraphColumn>| match column {
            Some(column) => format!("{:.2}%", column.aggregate.budget_pct_mean),
            None => "-".to_string(),
        };
        let _ = writeln!(
            out,
            "{:<22} {:>9} {:>9} {:>9} {:>8} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8} {:>10}",
            "tier",
            "sc p50",
            "sc p99",
            "sc min",
            "sc mean%",
            "op p50",
            "op p99",
            "op min",
            "op mean%",
            "spd p50",
            "spd min",
            "parity"
        );
        let _ = writeln!(out, "{}", "-".repeat(131));
        for tier in &self.tiers {
            let ratio =
                |value: Option<f64>| value.map_or_else(|| "-".to_string(), |v| format!("{v:.3}"));
            let _ = writeln!(
                out,
                "{:<22} {:>9} {:>9} {:>9} {:>8} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8} {:>10.3e}",
                tier.name,
                cell(tier.scalar.clone(), |stats| stats.p50_us),
                cell(tier.scalar.clone(), |stats| stats.p99_us),
                cell(tier.scalar.clone(), |stats| stats.min_us),
                pct(tier.scalar.clone()),
                cell(tier.optimized.clone(), |stats| stats.p50_us),
                cell(tier.optimized.clone(), |stats| stats.p99_us),
                cell(tier.optimized.clone(), |stats| stats.min_us),
                pct(tier.optimized.clone()),
                ratio(tier.speedup_p50),
                ratio(tier.speedup_min),
                tier.parity_max_abs
            );
        }
        let _ = writeln!(
            out,
            "\nspd p50 / spd min = scalar 的 p50 / min ÷ optimized 的同统计（> 1 表示 optimized 更快；\
             仅 Auto 模式下给出）。parity = 两列从零 state 起跑同一输入流后最后一块输出的最大绝对\
             差异（0 = 逐位相同）。\n\
             **读法**：parity ≠ 0 且 spd > 1 才是该档位真实的 SIMD 收益；parity = 0 意味着两列走\
             完全相同的实现（identity 档、matrix 档），那里的 spd 只反映测量噪声（应接近 1，\
             偏差即这次运行的噪声量级，min 列比 p50 稳）。T56 只接了 crossover 的两条卷积路径\
             （direct 直算 / partitioned 分区卷积）；matrix 低速率域的重采样器与 matrix FIR\
             仍是标量实现（实测拆分见 graph::matrix 模块文档）。"
        );
        for tier in &self.tiers {
            let repeat_p50 = |column: &Option<GraphColumn>| match column {
                Some(column) => column
                    .repeats
                    .iter()
                    .map(|repeat| format!("{:.2}", repeat.p50_us))
                    .collect::<Vec<_>>()
                    .join("/"),
                None => "-".to_string(),
            };
            let _ = writeln!(
                out,
                "  {:<20} repeat p50 (µs): scalar [{}] optimized [{}]",
                tier.name,
                repeat_p50(&tier.scalar),
                repeat_p50(&tier.optimized)
            );
        }
        let _ = writeln!(
            out,
            "\n预算判定：mean% / p99 对应的预算 = {:.3} ms/块（单线程、无后端开销；真实 xrun 判定\
             还需端到端运行，NOT_RUN）。",
            self.budget_us / 1000.0
        );
        let _ = writeln!(out, "\n构造信息（含 host 准入预算估算）：");
        for tier in &self.tiers {
            let _ = writeln!(out, "  {}", tier.name);
            let _ = writeln!(out, "    目的: {}", tier.purpose);
            let _ = writeln!(out, "    构造: {}", tier.construction.describe());
            let _ = writeln!(
                out,
                "    matrix 段估算延迟 {} 样本（全采样率）",
                tier.estimated_latency_samples
            );
        }
        let _ = writeln!(
            out,
            "\nkernel 级对照（同一 workload、同一输入；scalar = 标量参考内核，optimized = 运行时派发 \
             SIMD 内核；p50/p99/min 单位 µs；spd = scalar/optimized，> 1 表示优化后更快；backend={}）：",
            self.kernel_backend
        );
        let _ = writeln!(
            out,
            "{:<26} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10}",
            "kernel tier",
            "sc p50",
            "sc p99",
            "sc min",
            "op p50",
            "op p99",
            "op min",
            "spd p50",
            "spd p99",
            "parity"
        );
        let _ = writeln!(out, "{}", "-".repeat(122));
        for tier in &self.kernel_tiers {
            let _ = writeln!(out, "{}", tier.human_line());
        }
        let _ = writeln!(
            out,
            "\nkernel 构造信息（时间口径：内核入口每块耗时，不含 host 图调度/后端；parity = 同输入下 \
             两路径输出的最大绝对差异，用于证明跑的是同一语义）："
        );
        for tier in &self.kernel_tiers {
            let _ = writeln!(out, "  {}", tier.name);
            let _ = writeln!(out, "    目的: {}", tier.purpose);
            let _ = writeln!(out, "    workload: {}", tier.workload);
        }
        out
    }

    /// 机器可读 JSON（字段与人类可读表一致；不引入 serde，手写且经测试校验）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(8192);
        let _ = writeln!(out, "{{");
        let _ = writeln!(out, "  \"schema\": {},", json_string(SCHEMA));
        let _ = writeln!(out, "  \"platform\": {{");
        let _ = writeln!(out, "    \"os\": {},", json_string(self.platform.os));
        let _ = writeln!(out, "    \"arch\": {},", json_string(self.platform.arch));
        let _ = writeln!(out, "    \"cpu\": {},", json_string(&self.platform.cpu));
        match self.platform.logical_cores {
            Some(cores) => {
                let _ = writeln!(out, "    \"logical_cores\": {cores},");
            }
            None => {
                let _ = writeln!(out, "    \"logical_cores\": null,");
            }
        }
        let _ = writeln!(
            out,
            "    \"build_profile\": {}",
            json_string(self.platform.build_profile)
        );
        let _ = writeln!(out, "  }},");
        let _ = writeln!(
            out,
            "  \"config\": {{\"block_frames\": {}, \"sample_rate\": {}, \"lanes\": {}, \
             \"crossover_taps\": {}, \"matrix_taps\": {}, \"matrix_factor\": {}, \
             \"matrix_active_lanes\": {}, \"warmup_blocks\": {}, \"measure_blocks\": {}, \
             \"repeats\": {}, \"kernel_mode\": {}}},",
            self.config.block_frames,
            self.config.sample_rate,
            self.config.lanes,
            self.config.crossover_taps,
            self.config.matrix_taps,
            self.config.matrix_factor,
            self.config.matrix_active_lanes,
            self.config.warmup_blocks,
            self.config.measure_blocks,
            self.config.repeats,
            json_string(self.config.kernel_mode.name())
        );
        let _ = writeln!(out, "  \"budget_us\": {:.6},", self.budget_us);
        let _ = writeln!(
            out,
            "  \"timer_overhead_us\": {:.6},",
            self.timer_overhead_us
        );
        let _ = writeln!(out, "  \"graph_kernel\": {{");
        let _ = writeln!(
            out,
            "    \"backend\": {}, \"auto_path\": {}",
            json_string(&self.kernel_backend),
            json_string(&self.graph_auto_path)
        );
        let _ = writeln!(out, "  }},");
        let _ = writeln!(out, "  \"tiers\": [");
        for (index, tier) in self.tiers.iter().enumerate() {
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      \"name\": {},", json_string(&tier.name));
            let _ = writeln!(out, "      \"purpose\": {},", json_string(&tier.purpose));
            let _ = writeln!(
                out,
                "      \"construction\": {},",
                construction_json(&tier.construction)
            );
            let _ = writeln!(
                out,
                "      \"estimated_latency_samples\": {},",
                tier.estimated_latency_samples
            );
            let _ = writeln!(out, "      \"backend\": {},", json_string(&tier.backend));
            let _ = writeln!(out, "      \"auto_path\": {},", json_string(tier.auto_path));
            let _ = writeln!(
                out,
                "      \"parity_max_abs\": {:.6e},",
                tier.parity_max_abs
            );
            let ratio = |value: Option<f64>| match value {
                Some(value) => format!("{value:.3}"),
                None => "null".to_string(),
            };
            let _ = writeln!(
                out,
                "      \"speedup_p50\": {}, \"speedup_p99\": {}, \"speedup_min\": {},",
                ratio(tier.speedup_p50),
                ratio(tier.speedup_p99),
                ratio(tier.speedup_min)
            );
            let column_json = |column: &Option<GraphColumn>| match column {
                Some(column) => graph_column_json(column),
                None => String::from("null"),
            };
            let _ = writeln!(out, "      \"scalar\": {},", column_json(&tier.scalar));
            let _ = writeln!(out, "      \"optimized\": {}", column_json(&tier.optimized));
            let comma = if index + 1 < self.tiers.len() {
                ","
            } else {
                ""
            };
            let _ = writeln!(out, "    }}{comma}");
        }
        let _ = writeln!(out, "  ],");
        let _ = writeln!(
            out,
            "  \"kernel_backend\": {},",
            json_string(&self.kernel_backend)
        );
        let _ = writeln!(out, "  \"kernel_tiers\": [");
        for (index, tier) in self.kernel_tiers.iter().enumerate() {
            let comma = if index + 1 < self.kernel_tiers.len() {
                ","
            } else {
                ""
            };
            let _ = writeln!(out, "    {}{comma}", tier.to_json());
        }
        let _ = writeln!(out, "  ]");
        let _ = writeln!(out, "}}");
        out
    }
}

/// graph 档位一列的 JSON（path + spread + aggregate + repeats）。
fn graph_column_json(column: &GraphColumn) -> String {
    let mut out = String::with_capacity(1024);
    let _ = write!(
        out,
        "{{\"path\": {}, \"spread_pct\": {:.3}, \"aggregate\": {}, \"repeats\": [",
        json_string(column.path),
        column.spread_pct,
        stats_json(&column.aggregate)
    );
    for (repeat_index, stats) in column.repeats.iter().enumerate() {
        if repeat_index > 0 {
            out.push_str(", ");
        }
        out.push_str(&stats_json(stats));
    }
    out.push_str("]}");
    out
}

fn stats_json(stats: &TierStats) -> String {
    format!(
        "{{\"blocks\": {}, \"mean_us\": {:.3}, \"p50_us\": {:.3}, \"p99_us\": {:.3}, \
         \"p999_us\": {:.3}, \"min_us\": {:.3}, \"max_us\": {:.3}, \"budget_pct_mean\": {:.3}, \
         \"budget_pct_p99\": {:.3}}}",
        stats.blocks,
        stats.mean_us,
        stats.p50_us,
        stats.p99_us,
        stats.p999_us,
        stats.min_us,
        stats.max_us,
        stats.budget_pct_mean,
        stats.budget_pct_p99
    )
}

fn construction_json(construction: &Construction) -> String {
    format!(
        "{{\"lanes\": {}, \"block_frames\": {}, \"sample_rate\": {}, \"crossover_lanes\": {}, \
         \"crossover_taps_per_lane\": {}, \"crossover_total_taps\": {}, \"matrix_rows\": {}, \
         \"matrix_cols\": {}, \"matrix_taps\": {}, \"matrix_factor\": {}, \
         \"matrix_active_cells\": {}, \"admission\": {{\"coefficients\": {}, \
         \"convolution_taps\": {}, \"state_samples\": {}, \"latency_samples\": {}, \
         \"plan_bytes\": {}, \"memory_bytes\": {}}}}}",
        construction.lanes,
        construction.block_frames,
        construction.sample_rate,
        construction.crossover_lanes,
        construction.crossover_taps_per_lane,
        construction.crossover_total_taps,
        construction.matrix_rows,
        construction.matrix_cols,
        construction.matrix_taps,
        construction.matrix_factor,
        construction.matrix_active_cells,
        construction.budget.coefficients,
        construction.budget.convolution_taps,
        construction.budget.state_samples,
        construction.budget.latency_samples,
        construction.budget.plan_bytes,
        construction.budget.memory_bytes
    )
}

pub(crate) fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_config() -> BenchConfig {
        BenchConfig {
            block_frames: 64,
            sample_rate: 48_000,
            lanes: 2,
            crossover_taps: 64,
            matrix_taps: 2,
            matrix_factor: 8,
            matrix_active_lanes: 2,
            warmup_blocks: 2,
            measure_blocks: 8,
            repeats: 1,
            kernel_mode: KernelMode::Auto,
        }
    }

    #[test]
    fn scenario_plans_compile_and_process() {
        let config = tiny_config();
        let limits = AdmissionLimits::default();
        let scenarios = scenarios(&config);
        assert_eq!(
            scenarios.len(),
            9,
            "档位数固定为 9（含 T56 的 B0-crossover96）"
        );
        let samples = config.samples_per_block();
        for scenario in &scenarios {
            let mut graph = scenario
                .compile(&limits)
                .unwrap_or_else(|error| panic!("档位 {} 编译失败: {error}", scenario.name));
            let mut input = vec![0.0f32; samples];
            fill_signal(&mut input, 0x1234_5678);
            let mut output = vec![0.0f32; samples];
            for _ in 0..3 {
                graph.process(&input, &mut output);
            }
            assert!(
                output.iter().all(|value| value.is_finite()),
                "档位 {} 输出含非有限值",
                scenario.name
            );
            assert!(
                output.iter().any(|value| *value != 0.0),
                "档位 {} 输出全 0",
                scenario.name
            );
            assert_eq!(scenario.construction.lanes, config.lanes);
            assert_eq!(scenario.construction.block_frames, config.block_frames);
            assert_eq!(scenario.construction.sample_rate, config.sample_rate);
            assert!(
                scenario.construction.matrix_active_cells > 0,
                "档位 {} 的矩阵活跃格数应为正",
                scenario.name
            );
        }
    }

    #[test]
    fn identity_tier_is_bit_exact() {
        let config = tiny_config();
        let scenario = scenarios(&config)
            .into_iter()
            .next()
            .expect("identity 档位");
        assert_eq!(scenario.name, "A-identity");
        let mut graph = scenario
            .compile(&AdmissionLimits::default())
            .expect("identity 图编译失败");
        let mut input = vec![0.0f32; config.samples_per_block()];
        fill_signal(&mut input, 0x0bad_f00d);
        let mut output = vec![0.0f32; config.samples_per_block()];
        for _ in 0..3 {
            graph.process(&input, &mut output);
        }
        for (index, (actual, expected)) in output.iter().zip(input.iter()).enumerate() {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "identity 档位样本 {index} 不逐位相等"
            );
        }
    }

    #[test]
    fn stats_percentiles_are_ordered_and_budget_math_is_consistent() {
        let samples: Vec<f64> = (1..=1000).map(|value| value as f64).collect();
        let budget_us = 2_666.666_666_666_667;
        let stats = stats_from_samples(samples, budget_us);
        assert!(stats.mean_us >= stats.min_us);
        assert!(stats.mean_us <= stats.max_us);
        assert!(stats.p50_us <= stats.p99_us);
        assert!(stats.p99_us <= stats.p999_us);
        assert!((stats.p50_us - 500.0).abs() <= 1.0, "p50={}", stats.p50_us);
        assert!((stats.p99_us - 990.0).abs() <= 1.0, "p99={}", stats.p99_us);
        assert!(
            (stats.p999_us - 999.0).abs() <= 1.0,
            "p999={}",
            stats.p999_us
        );
        assert!((stats.budget_pct_mean - stats.mean_us / budget_us * 100.0).abs() <= 1.0e-9);
        assert!((stats.budget_pct_p99 - stats.p99_us / budget_us * 100.0).abs() <= 1.0e-9);

        let aggregate = aggregate(&[stats, stats, stats], budget_us);
        assert!((aggregate.mean_us - stats.mean_us).abs() <= 1.0e-9);
        assert!((aggregate.p99_us - stats.p99_us).abs() <= 1.0e-9);
        assert_eq!(aggregate.blocks, 1000);
        assert!(spread_pct(&[stats, stats]).abs() <= 1.0e-12);
    }

    #[test]
    fn lowpass_fir_is_finite_and_unity_dc() {
        for taps in [1usize, 8, 64, 2048] {
            let coeffs = lowpass_fir(taps, CROSSOVER_CUTOFF);
            assert_eq!(coeffs.len(), taps);
            assert!(coeffs.iter().all(|coeff| coeff.is_finite()));
            let sum: f64 = coeffs.iter().map(|coeff| f64::from(*coeff)).sum();
            assert!(
                (sum - 1.0).abs() <= 1.0e-5,
                "taps={taps} 的 DC 增益 {sum} 偏离 1"
            );
        }
    }

    #[test]
    fn matrix_pair_uses_same_taps_and_expected_factor() {
        let config = tiny_config();
        let scenarios = scenarios(&config);
        let three_khz = scenarios
            .iter()
            .find(|scenario| scenario.name == "C-matrix3k")
            .expect("C 档位");
        let full_rate = scenarios
            .iter()
            .find(|scenario| scenario.name == "E-matrix-full-rate")
            .expect("E 档位");
        let matrix_three_khz = three_khz.plan.matrix.as_ref().expect("C 矩阵段");
        let matrix_full_rate = full_rate.plan.matrix.as_ref().expect("E 矩阵段");
        assert_eq!(matrix_three_khz.taps, matrix_full_rate.taps);
        assert_eq!(matrix_three_khz.coeffs.len(), matrix_full_rate.coeffs.len());
        assert_eq!(matrix_three_khz.decimation, config.matrix_factor);
        assert_eq!(matrix_three_khz.interpolation, config.matrix_factor);
        assert_eq!(matrix_full_rate.decimation, 1);
        assert_eq!(matrix_full_rate.interpolation, 1);
    }

    #[test]
    fn default_baseline_scenarios_pass_admission_and_compile() {
        let config = BenchConfig::default();
        let limits = AdmissionLimits::default();
        for scenario in scenarios(&config) {
            let graph = scenario.compile(&limits).unwrap_or_else(|error| {
                panic!(
                    "默认基线档位 {} 编译 / 准入失败: {error}；构造: {}",
                    scenario.name,
                    scenario.construction.describe()
                )
            });
            assert!(
                graph.estimated_latency_samples() <= 8192,
                "档位 {} 的 matrix 估算延迟超出预算",
                scenario.name
            );
        }
    }

    #[test]
    fn report_json_and_table_are_wellformed() {
        let config = BenchConfig {
            warmup_blocks: 1,
            measure_blocks: 4,
            repeats: 1,
            ..tiny_config()
        };
        let report = run(&config).expect("小配置 bench 运行失败");
        assert_eq!(report.tiers.len(), 9);
        assert_eq!(
            report.graph_auto_path,
            KernelChoice::Auto.resolve_path().name(),
            "graph 档位的 auto 路径必须是 host graph 的真实解析结果"
        );
        for tier in &report.tiers {
            // Auto 模式：graph 档位两列都要有（scalar = 标量参考内核，optimized = 核心 SIMD 入口）。
            let scalar = tier.scalar.as_ref().expect("Auto 模式缺标量列");
            let optimized = tier.optimized.as_ref().expect("Auto 模式缺优化列");
            assert_eq!(scalar.path, "scalar");
            assert_eq!(optimized.path, "optimized");
            for column in [scalar, optimized] {
                assert!(column.aggregate.p50_us <= column.aggregate.p99_us);
                assert!(column.aggregate.p99_us <= column.aggregate.p999_us);
                assert!(column.aggregate.budget_pct_p99 > 0.0);
                assert_eq!(column.repeats.len(), 1);
            }
            assert!(
                tier.speedup_p50.is_some()
                    && tier.speedup_p99.is_some()
                    && tier.speedup_min.is_some(),
                "两列都测时必须给出加速比: {}",
                tier.name
            );
            assert!(
                tier.parity_max_abs.is_finite() && tier.parity_max_abs <= 1.0e-2,
                "graph 两列 parity 过大: {} -> {}",
                tier.name,
                tier.parity_max_abs
            );
            assert!(!tier.backend.is_empty());
        }
        // identity 档两列走同一套直通/折叠实现：parity 必须逐位为 0。
        let identity = report
            .tiers
            .iter()
            .find(|tier| tier.name == "A-identity")
            .expect("identity 档位");
        assert_eq!(identity.parity_max_abs, 0.0, "identity 档两列应逐位相同");
        // kernel 级对照（T39a）：默认 auto 模式必须给出两列与加速比 + parity。
        assert!(
            !report.kernel_tiers.is_empty(),
            "kernel 级对照档缺失（默认至少有 K-direct-crossover 与 K-part-3khz）"
        );
        for tier in &report.kernel_tiers {
            assert!(
                tier.scalar.is_some(),
                "Auto 模式下标量列缺失: {}",
                tier.name
            );
            assert!(
                tier.optimized.is_some(),
                "Auto 模式下优化列缺失: {}",
                tier.name
            );
            assert!(
                tier.speedup_p50.is_some() && tier.speedup_min.is_some(),
                "两列都测时必须给出加速比: {}",
                tier.name
            );
            assert!(
                tier.parity_max_abs.is_finite() && tier.parity_max_abs <= 1.0e-2,
                "两路径 parity 过大: {} -> {}",
                tier.name,
                tier.parity_max_abs
            );
        }
        assert!(!report.kernel_backend.is_empty());
        let json = report.to_json();
        assert!(json.starts_with('{'));
        assert!(json.trim_end().ends_with('}'));
        assert!(json.contains("\"schema\": \"khaen-bench/v2\""));
        assert!(json.contains("\"p50_us\""));
        assert!(json.contains("\"aggregate\""));
        assert!(json.contains("\"construction\""));
        assert!(json.contains("\"kernel_backend\""));
        assert!(json.contains("\"kernel_tiers\""));
        assert!(json.contains("\"K-direct-crossover\""));
        assert!(json.contains("\"parity_max_abs\""));
        // T56：graph 档位两列 + graph_kernel 段（真实后端 / auto 路径）。
        assert!(json.contains("\"graph_kernel\""));
        assert!(json.contains("\"auto_path\""));
        assert!(json.contains("\"scalar\": {\"path\": \"scalar\""));
        assert!(json.contains("\"optimized\": {\"path\": \"optimized\""));
        assert!(json.contains("\"speedup_p50\""));
        assert!(json.contains("\"B0-crossover96\""));
        let table = report.human_report();
        assert!(table.contains("A-identity"));
        assert!(table.contains("sc p50"));
        assert!(table.contains("op p50"));
        assert!(table.contains("spd p50"));
        assert!(table.contains("graph 档位"));
        assert!(table.contains("kernel 级对照"));
        for tier in &report.kernel_tiers {
            assert!(
                table.contains(tier.name.as_str()),
                "人类可读表缺少 kernel 档位 {}",
                tier.name
            );
        }
        for tier in &report.tiers {
            assert!(
                table.contains(tier.name.as_str()),
                "人类可读表缺少档位 {}",
                tier.name
            );
            assert!(
                table.contains(&tier.construction.describe()),
                "人类可读表缺少档位 {} 的构造信息",
                tier.name
            );
        }
    }

    #[test]
    fn kernel_mode_scalar_and_optimized_only_measure_one_column() {
        let mut config = BenchConfig {
            warmup_blocks: 1,
            measure_blocks: 4,
            repeats: 1,
            ..tiny_config()
        };
        config.kernel_mode = KernelMode::Scalar;
        let report = run(&config).expect("scalar-only bench");
        for tier in &report.tiers {
            assert!(
                tier.scalar.is_some(),
                "scalar 模式缺 graph 标量列: {}",
                tier.name
            );
            assert!(
                tier.optimized.is_none(),
                "scalar 模式不应测 graph 优化列: {}",
                tier.name
            );
            assert!(tier.speedup_p50.is_none(), "只测一列时不应有 graph 加速比");
        }
        for tier in &report.kernel_tiers {
            assert!(tier.scalar.is_some(), "scalar 模式缺标量列: {}", tier.name);
            assert!(
                tier.optimized.is_none(),
                "scalar 模式不应测优化列: {}",
                tier.name
            );
            assert!(tier.speedup_p50.is_none(), "只测一列时不应有加速比");
        }
        config.kernel_mode = KernelMode::Optimized;
        let report = run(&config).expect("optimized-only bench");
        for tier in &report.tiers {
            assert!(
                tier.optimized.is_some(),
                "optimized 模式缺 graph 优化列: {}",
                tier.name
            );
            assert!(
                tier.scalar.is_none(),
                "optimized 模式不应测 graph 标量列: {}",
                tier.name
            );
        }
        for tier in &report.kernel_tiers {
            assert!(
                tier.optimized.is_some(),
                "optimized 模式缺优化列: {}",
                tier.name
            );
            assert!(
                tier.scalar.is_none(),
                "optimized 模式不应测标量列: {}",
                tier.name
            );
        }
        assert!(report.to_json().contains("\"kernel_tiers\""));
        assert_eq!(KernelMode::parse("auto").expect("auto"), KernelMode::Auto);
        assert_eq!(
            KernelMode::parse("scalar").expect("scalar"),
            KernelMode::Scalar
        );
        assert_eq!(
            KernelMode::parse("optimized").expect("optimized"),
            KernelMode::Optimized
        );
        assert!(KernelMode::parse("fast").is_err());
    }

    #[test]
    fn graph_crossover_tiers_show_simd_parity_against_scalar() {
        // 小配置（2 lane、64 帧）：B0/B 两档都走 direct 路径；B2 = 4096 taps 走分区路径。
        let config = BenchConfig {
            warmup_blocks: 2,
            measure_blocks: 6,
            repeats: 1,
            ..tiny_config()
        };
        let report = run(&config).expect("小配置 bench 运行失败");
        for name in ["B0-crossover96", "B2-crossover4096"] {
            let tier = report
                .tiers
                .iter()
                .find(|tier| tier.name == name)
                .unwrap_or_else(|| panic!("缺少档位 {name}"));
            // 两列都是同一方案、同一输入：parity 只能来自内核浮点舍入，容差与 core 对拍同量级。
            assert!(
                tier.parity_max_abs.is_finite() && tier.parity_max_abs <= 1.0e-3,
                "{name}: 两列 parity {} 超过 1e-3",
                tier.parity_max_abs
            );
            let scalar = tier.scalar.as_ref().expect("标量列");
            let optimized = tier.optimized.as_ref().expect("优化列");
            assert!(scalar.aggregate.p50_us > 0.0, "{name}: 标量列耗时应为正");
            assert!(optimized.aggregate.p50_us > 0.0, "{name}: 优化列耗时应为正");
        }
        // 矩阵低速率域档（C-matrix3k）目前没有 SIMD 入口：两列必须逐位相同（parity = 0），
        // 否则说明 matrix 段被意外改了数值行为。
        let matrix_tier = report
            .tiers
            .iter()
            .find(|tier| tier.name == "C-matrix3k")
            .expect("C 档位");
        assert_eq!(
            matrix_tier.parity_max_abs, 0.0,
            "matrix3k 档两列应逐位相同（matrix 段尚未接 SIMD）"
        );
    }

    #[test]
    fn invalid_configs_are_rejected_before_running() {
        let mut config = tiny_config();
        config.sample_rate = 44_100;
        assert!(config.validate().is_err(), "44.1 kHz 应被拒绝");
        let mut config = tiny_config();
        config.matrix_factor = 24;
        assert!(config.validate().is_err(), "64 帧不能被 24 整除，应被拒绝");
        let mut config = tiny_config();
        config.crossover_taps = DIRECT_MAX_TAPS + 1;
        assert!(config.validate().is_err(), "超过 direct 上限应被拒绝");
    }
}
