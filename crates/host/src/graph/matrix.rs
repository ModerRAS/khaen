//! matrix 段：跨 lane 满矩阵（全速率直算 / 3 kHz 低速率域 + 有理数重采样，T22）。
//!
//! 两种执行形态，编译期由方案的 `decimation` / `interpolation` 决定：
//!
//! 1. **全速率**（`decimation == interpolation == 1`）：每个格子 `(row, col)` 是一条 `taps`
//!    抽头 FIR，行输出是各列 FIR 之和（列序固定、tap 升序累加、历史跨块保留）。identity 判据
//!    （对角 1、`taps == 1`）在编译期折叠成逐样本拷贝，保证 identity 下逐位相等。
//! 2. **低速率域**（`decimation == interpolation == F > 1`）：全速率输入先按列抽取到
//!    `fs / F`（每列一个 [`RationalResampler`]，比例 `F / 1`），在低速率域做同一套满
//!    矩阵 FIR 卷积（复用全速率的格子 / 历史代码，块长换成 `chunk_frames / F`），再按行
//!    插值回全速率（每行一个重采样器，比例 `1 / F`）。`F = 16`、`sample_rate = 48000`
//!    即设计文档说的 3 kHz 域。
//!
//! 固定速率图（matrix 前后各段都跑全采样率）要求 matrix 段输入 / 输出速率一致，因此这里只接受
//! `decimation == interpolation`；其它组合（变比 / ASRC）在编译期返回
//! [`super::CompileError::UnsupportedRates`]，0 因子另按非法区段拒绝。
//!
//! # 延迟（全采样率样本）
//!
//! 假设重采样滤波器与矩阵 FIR 都按线性相位设计：
//!
//! ~~~text
//! latency = (L_dec - 1) / 2          // 列抽取器：输入侧群延迟（全速率样本）
//!         + (taps - 1) / 2 * F       // 低速率域矩阵 FIR：换算回全速率
//!         + (L_int - 1) / 2          // 行插值器：输出侧群延迟（全速率样本）
//! L_dec = L_int = taps_per_phase * F    （F > 1 时；见 resample 模块）
//! ~~~
//!
//! 本模块只上报 matrix 段自身的估算延迟（[`MatrixStage::estimated_latency_samples`]）；
//! crossover / protection 的延迟不在内。
//!
//! # 块长边界
//!
//! 低速率域每块样本数在编译期算出：`stride_low = chunk_frames / F`，因此要求
//! `chunk_frames % F == 0`；低速率域的全部缓冲（每列 / 每行的重采样器状态、低速率平面）在
//! 编译期一次性分配。运行期每块帧数也必须是 `F` 的整数倍：不足一组的尾部按 fail-safe 写
//! +0.0（不 panic、不破坏跨块状态）；真实块由引擎 / 调用方保证是整倍块长。
//!
//! # SIMD 派发评估（T56，未接 optimized，附实测依据）
//!
//! T56 把 crossover 两条卷积路径接到 `khaen_core::kernels::dispatch` 的优化入口；本段**没有**
//! 同样接线，原因来自实测拆分而不是"漏了"（本机 release quick，22 lane / B=128 / 48 kHz，
//! 单块预算 2.667 ms，p50 见 `khaen-bench` 的 graph 档位）：
//!
//! ~~~text
//! 档位                          p50 µs   budget%   说明
//! C-matrix3k        (8-tap)      90.0     3.4%    3 kHz 域：262 活跃格
//! C1-matrix3k-1tap  (1-tap)      82.2     3.1%    同一条链只把矩阵格换成 1 tap
//! E-matrix-full-rate(8-tap)     136.3     5.1%    全速率：262 × 8 tap
//! E1-full-rate-1tap (1-tap)       6.5     0.2%    全速率静态混音
//! ~~~
//!
//! 1. **生产形态（3 kHz 低速率域）的成本几乎全在重采样器**：C1 与 C 只差 8 µs（8 个 tap 的
//!    矩阵 FIR 全部代价），其余 ~82 µs 是每列抽取器 + 每行插值器（[RationalResampler]，
//!    16 taps/phase）。重采样器在 `khaen_core::kernels::resample`，T39a/T56 的内核优化入口
//!    只覆盖 direct / partitioned 卷积，**没有** resample 的 optimized 入口；在 core 之外
//!    无法把这段接到 SIMD，属于后续 core 任务。矩阵 FIR 本身即使归零也只省 ~0.3% 预算。
//! 2. **全速率形态（factor = 1）的矩阵 FIR 才是本段唯一可 SIMD 化的热点**（E 档 ~130 µs 的
//!    矩阵 FIR，约 4.9% 预算），但它不在生产形态里：DESIGN 的矩阵目标域是 3 kHz，校准链
//!    （`calibration::plan_levels`）用的是 identity 矩阵（编译期折叠为直通）。要接线只能
//!    按格子调 1 源/1 角色/1 容器的 direct 优化入口、再用 scratch 累加（内核入口是写不是
//!    累加）：每块 262 次内核调用 + 2 次额外搬运，估算收益 ~2-3% 预算，代价是 factor = 1
//!    配置的默认舍入行为改变，且需要一套新的矩阵格子对拍测试。T56 的写范围只含
//!    `crates/host/src/graph/**` 与两个既有测试文件，不值得为这个非生产形态扩大数值面；
//!    结论记为后续任务（若 factor = 1 成为生产配置，再按上面设计接线并补对拍）。
//!
//! 因此本段在两条 `KernelPath` 下都是同一套标量实现（`KernelChoice` 只影响 crossover）；
//! `PlanGraph::kernel_dispatch` 上报的是**图实际接的内核**，不会因为本段没接 SIMD 而虚报。
//!
//! # T22 边界
//!
//! 未实现（留给后续）：系数在块边界的原子切换（方案切换由 host 的 commit 层做整图切换）、
//! 变比重采样（ASRC）、低速率域分区卷积、矩阵段的 SIMD/Core-optimized 格子卷积（见上节）。

use khaen_core::kernels::resample::RationalResampler;
use khaen_core::plan::MatrixSection;

use super::CompileError;

/// 低速率域重采样器的每相抽头数（构造期固定的实现参数，不属于方案文件字段）。
pub(crate) const RESAMPLE_TAPS_PER_PHASE: usize = 16;
/// 低速率域重采样器截止频率占低侧 Nyquist 的比例。
pub(crate) const RESAMPLE_CUTOFF_NORM: f64 = 0.9;
/// 矩阵段支持的抽取 / 插值因子上限（超过此值的比例在编译期拒绝）。
pub(crate) const MAX_MATRIX_RATE_FACTOR: usize = 64;

/// 单个矩阵格子的 FIR 状态。
#[derive(Debug)]
struct MatrixCell {
    /// 该格子的 taps 抽头系数。
    coeffs: Vec<f32>,
    /// 卷积历史：`hist[t] = x[block_start - 1 - t]`，长度 `taps - 1`。
    hist: Vec<f32>,
    /// 系数全为 0：该格子跳过。
    silent: bool,
}

/// 低速率域状态（仅 `decimation == interpolation == F > 1` 时存在）。
#[derive(Debug)]
struct LowRateState {
    /// 抽取 / 插值因子 F（= decimation = interpolation）。
    factor: usize,
    /// 低速率域每块最大样本数（= chunk_frames / F）。
    stride_low: usize,
    /// 每列一个抽取器（F/1）。
    decimators: Vec<RationalResampler>,
    /// 每行一个插值器（1/F）。
    interpolators: Vec<RationalResampler>,
    /// 抽取后的列信号平面（`cols * stride_low`）。
    low_in: Vec<f32>,
    /// 低速率域矩阵输出平面（`rows * stride_low`）。
    low_out: Vec<f32>,
}

/// 矩阵段的执行形态。
#[derive(Debug)]
enum MatrixMode {
    /// identity 判据成立：逐样本拷贝。
    Identity,
    /// 全速率满矩阵直接卷积（decimation == interpolation == 1）。
    FullRate,
    /// 低速率域满矩阵卷积 + 抽取 / 插值重采样。
    LowRate(LowRateState),
}

/// matrix 段状态。
#[derive(Debug)]
pub(crate) struct MatrixStage {
    /// 输出 lane 数。
    rows: usize,
    /// 输入 lane 数。
    cols: usize,
    /// 每格抽头数（低速率形态下是低速率域的抽头数）。
    taps: usize,
    /// 执行形态。
    mode: MatrixMode,
    /// 行优先排列的格子（`rows * cols`），Identity 模式下为空。
    cells: Vec<MatrixCell>,
}

impl MatrixStage {
    /// 编译 matrix 段。
    ///
    /// `chunk_frames` 是宿主每块处理的最大帧数（全采样率），用于算出低速率域每块样本数并
    /// 一次性分配低速率缓冲。
    pub(crate) fn compile(
        section: &MatrixSection,
        cols: usize,
        rows: usize,
        chunk_frames: usize,
    ) -> Result<Self, CompileError> {
        if section.cols as usize != cols || section.rows as usize != rows {
            return Err(CompileError::LaneMismatch {
                stage: "matrix",
                expected: cols,
                actual: section.cols as usize,
            });
        }
        if rows == 0 || cols == 0 {
            return Err(CompileError::InvalidSection(
                "matrix rows and cols must be positive",
            ));
        }
        let taps = section.taps as usize;
        if taps == 0 {
            return Err(CompileError::InvalidSection("matrix taps must be positive"));
        }
        let expected = rows
            .checked_mul(cols)
            .and_then(|cells| cells.checked_mul(taps))
            .ok_or(CompileError::InvalidSection(
                "matrix coefficient count overflow",
            ))?;
        if section.coeffs.len() != expected {
            return Err(CompileError::InvalidSection(
                "matrix coefficient count mismatch",
            ));
        }
        for coeff in &section.coeffs {
            if !coeff.is_finite() {
                return Err(CompileError::NonFiniteCoefficient("matrix"));
            }
        }
        let decimation = usize::from(section.decimation);
        let interpolation = usize::from(section.interpolation);
        if decimation == 0 || interpolation == 0 {
            return Err(CompileError::InvalidSection(
                "matrix decimation and interpolation must be positive",
            ));
        }
        if decimation > MAX_MATRIX_RATE_FACTOR || interpolation > MAX_MATRIX_RATE_FACTOR {
            return Err(CompileError::UnsupportedRates {
                decimation: section.decimation,
                interpolation: section.interpolation,
            });
        }
        if decimation != interpolation {
            // 固定速率图要求 matrix 段输入 / 输出速率一致；变比（ASRC）留给后续任务。
            return Err(CompileError::UnsupportedRates {
                decimation: section.decimation,
                interpolation: section.interpolation,
            });
        }
        if decimation > 1 && !is_multiple(chunk_frames, decimation) {
            return Err(CompileError::InvalidSection(
                "matrix decimation must divide the processing chunk size",
            ));
        }
        // identity 判据：rows == cols、taps == 1、对角 1.0 其余 0（只在全速率形态成立）。
        let identity = decimation == 1
            && rows == cols
            && taps == 1
            && section.coeffs.iter().enumerate().all(|(index, coeff)| {
                if index % (cols + 1) == 0 {
                    *coeff == 1.0
                } else {
                    *coeff == 0.0
                }
            });
        if identity {
            return Ok(Self {
                rows,
                cols,
                taps,
                mode: MatrixMode::Identity,
                cells: Vec::new(),
            });
        }
        let mut cells = Vec::with_capacity(rows * cols);
        for row in 0..rows {
            for col in 0..cols {
                let base = (row * cols + col) * taps;
                let coeffs = section.coeffs[base..base + taps].to_vec();
                let silent = coeffs.iter().all(|coeff| *coeff == 0.0);
                let hist = if silent {
                    Vec::new()
                } else {
                    vec![0.0f32; taps - 1]
                };
                cells.push(MatrixCell {
                    coeffs,
                    hist,
                    silent,
                });
            }
        }
        let mode = if decimation == 1 {
            MatrixMode::FullRate
        } else {
            MatrixMode::LowRate(Self::compile_low_rate(
                decimation,
                cols,
                rows,
                chunk_frames,
            )?)
        };
        Ok(Self {
            rows,
            cols,
            taps,
            mode,
            cells,
        })
    }

    /// 编译低速率域：每列一个抽取器、每行一个插值器，并一次性分配低速率平面。
    fn compile_low_rate(
        factor: usize,
        cols: usize,
        rows: usize,
        chunk_frames: usize,
    ) -> Result<LowRateState, CompileError> {
        let stride_low = chunk_frames / factor;
        // 每个 lane 一个独立状态的重采样器（系数相同；构造在 NONRT，允许重复计算）。
        let mut decimators = Vec::with_capacity(cols);
        for _ in 0..cols {
            decimators.push(
                RationalResampler::new(
                    factor as u32,
                    1,
                    RESAMPLE_TAPS_PER_PHASE,
                    RESAMPLE_CUTOFF_NORM,
                )
                .map_err(|_| {
                    CompileError::InvalidSection("matrix decimation resampler rejected")
                })?,
            );
        }
        let mut interpolators = Vec::with_capacity(rows);
        for _ in 0..rows {
            interpolators.push(
                RationalResampler::new(
                    1,
                    factor as u32,
                    RESAMPLE_TAPS_PER_PHASE,
                    RESAMPLE_CUTOFF_NORM,
                )
                .map_err(|_| {
                    CompileError::InvalidSection("matrix interpolation resampler rejected")
                })?,
            );
        }
        let low_in_len = cols
            .checked_mul(stride_low)
            .ok_or(CompileError::InvalidSection(
                "matrix low-rate buffer size overflow",
            ))?;
        let low_out_len = rows
            .checked_mul(stride_low)
            .ok_or(CompileError::InvalidSection(
                "matrix low-rate buffer size overflow",
            ))?;
        Ok(LowRateState {
            factor,
            stride_low,
            decimators,
            interpolators,
            low_in: vec![0.0f32; low_in_len],
            low_out: vec![0.0f32; low_out_len],
        })
    }

    /// 处理一个子块：`input` 为 `cols` 个平面，`output` 为 `rows` 个平面。
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        let Self {
            rows,
            cols,
            taps,
            mode,
            cells,
        } = self;
        let rows = *rows;
        let cols = *cols;
        let taps = *taps;
        match mode {
            MatrixMode::Identity => {
                let mut lane = 0;
                while lane < rows {
                    let inp = &input[lane * stride..lane * stride + frames];
                    output[lane * stride..lane * stride + frames].copy_from_slice(inp);
                    lane += 1;
                }
            }
            MatrixMode::FullRate => {
                Self::full_rate_matrix(cells, cols, rows, taps, input, output, stride, frames);
            }
            MatrixMode::LowRate(low) => {
                Self::low_rate_matrix(cells, low, cols, rows, taps, input, output, stride, frames);
            }
        }
    }

    /// 全速率满矩阵直接卷积。
    #[allow(clippy::too_many_arguments)]
    fn full_rate_matrix(
        cells: &mut [MatrixCell],
        cols: usize,
        rows: usize,
        taps: usize,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        let mut row = 0;
        while row < rows {
            output[row * stride..row * stride + frames].fill(0.0);
            let mut col = 0;
            while col < cols {
                let cell = &mut cells[row * cols + col];
                if !cell.silent {
                    let x = &input[col * stride..col * stride + frames];
                    let out = &mut output[row * stride..row * stride + frames];
                    Self::accumulate_cell(cell, x, out, taps, frames);
                }
                col += 1;
            }
            row += 1;
        }
    }

    /// 低速率域：每列抽取 -> 低速率满矩阵 FIR -> 每行插值。
    #[allow(clippy::too_many_arguments)]
    fn low_rate_matrix(
        cells: &mut [MatrixCell],
        low: &mut LowRateState,
        cols: usize,
        rows: usize,
        taps: usize,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        let factor = low.factor;
        let stride_low = low.stride_low;
        // 运行期块长必须是 F 的整数倍；不足一组的尾部后面按 fail-safe 静音。
        let aligned = frames - frames % factor;
        let low_frames = aligned / factor;
        // 1) 每列抽取到低速率域（列间状态互相独立）。
        let mut col = 0;
        while col < cols {
            let source = &input[col * stride..col * stride + aligned];
            let target = &mut low.low_in[col * stride_low..col * stride_low + low_frames];
            let written = low.decimators[col].process(source, target);
            if written < low_frames {
                target[written..].fill(0.0);
            }
            col += 1;
        }
        // 2) 低速率域满矩阵 FIR：复用全速率的格子卷积（只是块长换成低速率块长）。
        let mut row = 0;
        while row < rows {
            low.low_out[row * stride_low..row * stride_low + low_frames].fill(0.0);
            let mut col = 0;
            while col < cols {
                let cell = &mut cells[row * cols + col];
                if !cell.silent {
                    let source = &low.low_in[col * stride_low..col * stride_low + low_frames];
                    let target = &mut low.low_out[row * stride_low..row * stride_low + low_frames];
                    Self::accumulate_cell(cell, source, target, taps, low_frames);
                }
                col += 1;
            }
            row += 1;
        }
        // 3) 每行插值回全速率。
        let mut row = 0;
        while row < rows {
            let source = &low.low_out[row * stride_low..row * stride_low + low_frames];
            let target = &mut output[row * stride..row * stride + aligned];
            let written = low.interpolators[row].process(source, target);
            if written < aligned {
                target[written..].fill(0.0);
            }
            row += 1;
        }
        // 4) 尾部不足一组：fail-safe 静音（不 panic、不破坏跨块状态）。
        if aligned < frames {
            let mut row = 0;
            while row < rows {
                output[row * stride + aligned..row * stride + frames].fill(0.0);
                row += 1;
            }
        }
    }

    /// 把一个格子的 FIR 累加进输出平面（固定 tap 升序，历史跨块保留）。
    fn accumulate_cell(
        cell: &mut MatrixCell,
        x: &[f32],
        out: &mut [f32],
        taps: usize,
        frames: usize,
    ) {
        let mut frame = 0;
        while frame < frames {
            let mut acc = cell.coeffs[0] * x[frame];
            let mut tap = 1;
            while tap < taps {
                let value = if tap <= frame {
                    x[frame - tap]
                } else {
                    cell.hist[tap - 1 - frame]
                };
                acc += cell.coeffs[tap] * value;
                tap += 1;
            }
            out[frame] += acc;
            frame += 1;
        }
        // 历史更新：hist[t] = x[frames - 1 - t]，不足部分沿用旧历史（从后往前写）。
        let mut index = taps - 1;
        while index > 0 {
            index -= 1;
            cell.hist[index] = if frames > index {
                x[frames - 1 - index]
            } else {
                cell.hist[index - frames]
            };
        }
    }

    /// 估算 matrix 段的延迟（全采样率样本）：重采样滤波器群延迟 + 矩阵 FIR 群延迟。
    pub(crate) fn estimated_latency_samples(&self) -> u64 {
        let matrix_low_rate_samples = (self.taps.saturating_sub(1)) as f64 / 2.0;
        match &self.mode {
            MatrixMode::Identity => 0,
            MatrixMode::FullRate => round_samples(matrix_low_rate_samples),
            MatrixMode::LowRate(low) => {
                let factor = low.factor as f64;
                let decimator_delay = low
                    .decimators
                    .first()
                    .map_or(0.0, RationalResampler::group_delay_input_samples);
                let interpolator_delay = low
                    .interpolators
                    .first()
                    .map_or(0.0, RationalResampler::group_delay_output_samples);
                round_samples(
                    decimator_delay + matrix_low_rate_samples * factor + interpolator_delay,
                )
            }
        }
    }
}

/// `chunk_frames % factor == 0`。
///
/// workspace MSRV 1.80：`usize::is_multiple_of` 自 1.87 才稳定，这里保持取模写法并
/// 关掉对应的 clippy 建议（与 `crates/aoip-aes67` 的处理一致）。
#[allow(clippy::manual_is_multiple_of)]
fn is_multiple(chunk_frames: usize, factor: usize) -> bool {
    chunk_frames % factor == 0
}

/// 四舍五入到样本数（估算值可能带 0.5 样本；返回最近整数）。
fn round_samples(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value.round() as u64
    }
}
