//! [RT] 有理数多相重采样内核（T22）。
//!
//! 单级有理数重采样器：输入速率 `fs_in`，输出速率 `fs_in * interpolation / decimation`。
//! 抽取 / 插值因子都为 1 时是恒等搬运；矩阵段的 3 kHz 低速率域用它做两件事：
//!
//! ~~~text
//! 全速率 --每列抽取(decimation=F, interpolation=1)--> fs/F 低速率域
//!        --每行插值(decimation=1, interpolation=F)--> 全速率
//! ~~~
//!
//! # 系数设计（只在构造期算一次，之后只读）
//!
//! 窗函数法：理想低通 × Hann 窗，f64 只用 `std` 的 `sin` / `cos`，因此同一构造参数在任何
//! 平台得到同一组系数的 f64 值（平台间 `sin` 末位差异用调用方测试容差处理，不做冻结摘要）。
//!
//! - 原型滤波器长度：`filter_len = ceil(taps_per_phase * max(decimation, interpolation) / interpolation) * interpolation`，
//!   即 `interpolation` 个相位、每相 `arm_taps = filter_len / interpolation` 个抽头。
//! - 截止角频率（在 `interpolation * fs_in` 的上采样率下）：`omega_c = cutoff_norm * PI / max(decimation, interpolation)`。
//!   等价于截止频率 = `cutoff_norm * min(fs_in, fs_out) / 2`，即低侧 Nyquist 的 `cutoff_norm` 倍。
//! - 抽头系数按**相位归一化**：每一相的抽头之和严格为 1（f64 计算后落 f32），因此常数输入
//!   （直流）经过任意 `(decimation, interpolation)` 组合后增益为 1。
//!
//! # 延迟定义（宿主换算用）
//!
//! 滤波器为线性相位，中心在 `(filter_len - 1) / 2`（上采样率样本）。换算：
//!
//! - [`RationalResampler::group_delay_input_samples`]：`(filter_len - 1) / (2 * interpolation)`，单位是**本器输入样本**；
//! - [`RationalResampler::group_delay_output_samples`]：`(filter_len - 1) / (2 * decimation)`，单位是**本器输出样本**。
//!
//! 矩阵段把这两个量换算成全采样率样本，公式见 host 侧 `graph::matrix` 模块。
//!
//! # 实时约束
//!
//! 构造期（NONRT）一次性分配系数与环形历史缓冲；[`RationalResampler::process`] 只读写调用方
//! 预分配的切片与自身缓冲：**零分配、无锁、无 IO、无 panic**。输出切片不足时写 0 个样本并
//! 返回 0（不推进状态），调用方据返回值判断。
//!
//! # T22 边界
//!
//! 只做**固定比例**的有理数重采样（构造后比例不变）。变比（ASRC）、块边界系数切换、分区卷积
//! 的重采样都留给后续任务。

use core::f64::consts::PI;

/// 每相最少抽头数：再少则窗函数/归一化没有意义。
pub const MIN_TAPS_PER_PHASE: usize = 4;
/// 每相最多抽头数（防止构造期分配失控；host 用的是更小的固定值）。
pub const MAX_TAPS_PER_PHASE: usize = 1024;
/// 抽取 / 插值因子上限（防止构造期长度计算溢出；host 另有更严格的矩阵上限）。
pub const MAX_RATE_FACTOR: u32 = 1024;
/// 内部每次最多搬入环形缓冲的输入样本数（固定值，保证 process 不分配）。
const INNER_CHUNK: usize = 64;

/// 重采样器构造错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerError {
    /// 抽取或插值因子为 0。
    ZeroRate,
    /// 抽取或插值因子超过 [`MAX_RATE_FACTOR`]。
    RateTooLarge,
    /// 每相抽头数不在 `[MIN_TAPS_PER_PHASE, MAX_TAPS_PER_PHASE]`。
    TapsOutOfRange,
    /// 截止比例不在 `(0, 1]` 或不是有限值。
    InvalidCutoff,
    /// 长度计算溢出。
    LengthOverflow,
}

impl core::fmt::Display for ResamplerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ResamplerError::ZeroRate => write!(f, "resampler rate factor must be positive"),
            ResamplerError::RateTooLarge => {
                write!(f, "resampler rate factor exceeds limit {MAX_RATE_FACTOR}")
            }
            ResamplerError::TapsOutOfRange => write!(
                f,
                "resampler taps_per_phase outside [{MIN_TAPS_PER_PHASE}, {MAX_TAPS_PER_PHASE}]"
            ),
            ResamplerError::InvalidCutoff => {
                write!(f, "resampler cutoff_norm must be in (0, 1]")
            }
            ResamplerError::LengthOverflow => write!(f, "resampler length overflow"),
        }
    }
}

impl std::error::Error for ResamplerError {}

/// 有理数多相重采样器（固定抽取 / 插值比例，零分配处理）。
///
/// 语义：输出样本 `m` 为
///
/// ~~~text
/// y[m] = sum_{p=0}^{arm_taps-1} coeffs[phase * arm_taps + p] * x[floor(m * D / I) - p]
/// phase = (m * D) mod I
/// ~~~
///
/// 其中 `D = decimation`、`I = interpolation`，`x[n < 0] = 0`（因果初态全零）。系数按相位
/// 优先存放（phase-major），每相之和为 1（直流增益 1）。
#[derive(Debug)]
pub struct RationalResampler {
    /// 抽取因子 D。
    decimation: u32,
    /// 插值因子 I。
    interpolation: u32,
    /// 构造时请求的每相抽头数。
    taps_per_phase: usize,
    /// 原型滤波器长度（上采样率下的抽头数，I 的整数倍）。
    filter_len: usize,
    /// 每相抽头数 = `filter_len / I`。
    arm_taps: usize,
    /// 相位优先系数：`coeffs[phase * arm_taps + p]`。
    coeffs: Vec<f32>,
    /// 输入历史环形缓冲：长度 `arm_taps + INNER_CHUNK`，位置 `k % ring_len` 存样本 `k`。
    ring: Vec<f32>,
    /// 环形缓冲长度。
    ring_len: usize,
    /// 环形缓冲写指针（= 已消费样本数 % ring_len）。
    ring_write: usize,
    /// 已消费的输入样本总数。
    consumed: u64,
    /// 已产出的输出样本总数。
    produced: u64,
}

impl RationalResampler {
    /// 构造重采样器：`decimation` / `interpolation` 为速率因子，`taps_per_phase` 为每相抽头数，
    /// `cutoff_norm` 为截止频率占低侧 Nyquist 的比例（典型 0.85~0.95）。
    ///
    /// 全部系数与状态缓冲在这里分配并算好；之后 [`Self::process`] 不再分配。
    pub fn new(
        decimation: u32,
        interpolation: u32,
        taps_per_phase: usize,
        cutoff_norm: f64,
    ) -> Result<Self, ResamplerError> {
        if decimation == 0 || interpolation == 0 {
            return Err(ResamplerError::ZeroRate);
        }
        if decimation > MAX_RATE_FACTOR || interpolation > MAX_RATE_FACTOR {
            return Err(ResamplerError::RateTooLarge);
        }
        if !(MIN_TAPS_PER_PHASE..=MAX_TAPS_PER_PHASE).contains(&taps_per_phase) {
            return Err(ResamplerError::TapsOutOfRange);
        }
        if !cutoff_norm.is_finite() || cutoff_norm <= 0.0 || cutoff_norm > 1.0 {
            return Err(ResamplerError::InvalidCutoff);
        }
        let span = core::cmp::max(decimation, interpolation) as usize;
        let prototype_taps = taps_per_phase
            .checked_mul(span)
            .ok_or(ResamplerError::LengthOverflow)?;
        let phases = interpolation as usize;
        // 原型长度必须是相位数的整数倍（每相抽头数相同）。
        let filter_len = prototype_taps.div_ceil(phases) * phases;
        let arm_taps = filter_len / phases;
        let ring_len = arm_taps
            .checked_add(INNER_CHUNK)
            .ok_or(ResamplerError::LengthOverflow)?;
        let coeffs = design_coefficients(decimation, interpolation, filter_len, cutoff_norm);
        Ok(Self {
            decimation,
            interpolation,
            taps_per_phase,
            filter_len,
            arm_taps,
            coeffs,
            ring: vec![0.0f32; ring_len],
            ring_len,
            ring_write: 0,
            consumed: 0,
            produced: 0,
        })
    }

    /// 抽取因子。
    pub fn decimation(&self) -> u32 {
        self.decimation
    }

    /// 插值因子。
    pub fn interpolation(&self) -> u32 {
        self.interpolation
    }

    /// 构造时请求的每相抽头数。
    pub fn taps_per_phase(&self) -> usize {
        self.taps_per_phase
    }

    /// 原型滤波器长度（上采样率下的抽头数）。
    pub fn filter_len(&self) -> usize {
        self.filter_len
    }

    /// 每个相位的抽头数。
    pub fn arm_taps(&self) -> usize {
        self.arm_taps
    }

    /// 相位交错系数（测试与离线核对用）。
    pub fn coefficients(&self) -> &[f32] {
        &self.coeffs
    }

    /// 本器输入侧的群延迟（单位：本器输入样本）。
    pub fn group_delay_input_samples(&self) -> f64 {
        (self.filter_len - 1) as f64 / (2.0 * self.interpolation as f64)
    }

    /// 本器输出侧的群延迟（单位：本器输出样本）。
    pub fn group_delay_output_samples(&self) -> f64 {
        (self.filter_len - 1) as f64 / (2.0 * self.decimation as f64)
    }

    /// 处理一段输入前，预先算出 [`Self::process`] 会写出的输出样本数。
    ///
    /// 该值与调用历史有关（有理数比例下每块输出数可能相差 1），所以必须在调用前询问。
    pub fn output_len_for(&self, input_len: usize) -> usize {
        let before = outputs_after(self.consumed, self.interpolation, self.decimation);
        let after = outputs_after(
            self.consumed.saturating_add(input_len as u64),
            self.interpolation,
            self.decimation,
        );
        (after - before) as usize
    }

    /// 处理一段输入，写出由此可确定的全部输出样本，返回写出的样本数。
    ///
    /// - `output.len()` 必须 ≥ [`Self::output_len_for`]`(input.len())`；不足时**不写、不推进
    ///   状态**并返回 0（调用方负责预分配足够输出）。
    /// - 零分配、无锁、无 IO、无 panic。
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> usize {
        let expected = self.output_len_for(input.len());
        if output.len() < expected {
            return 0;
        }
        let mut written = 0usize;
        let mut position = 0usize;
        while position < input.len() {
            let take = core::cmp::min(INNER_CHUNK, input.len() - position);
            self.push_input(&input[position..position + take]);
            self.consumed += take as u64;
            let target = outputs_after(self.consumed, self.interpolation, self.decimation);
            while self.produced < target {
                output[written] = self.next_output();
                written += 1;
                self.produced += 1;
            }
            position += take;
        }
        written
    }

    /// 清空历史与计数（不分配；用于测试与重新开始一个 epoch）。
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.ring_write = 0;
        self.consumed = 0;
        self.produced = 0;
    }

    /// 把一段输入按环形顺序写入历史缓冲。
    fn push_input(&mut self, samples: &[f32]) {
        let mut write = self.ring_write;
        let ring_len = self.ring_len;
        for &sample in samples {
            self.ring[write] = sample;
            write += 1;
            if write == ring_len {
                write = 0;
            }
        }
        self.ring_write = write;
    }

    /// 计算下一个输出样本（调用前保证所需输入已全部写入历史缓冲）。
    fn next_output(&mut self) -> f32 {
        let index = self.produced as u128;
        let interpolation = self.interpolation as u128;
        let base = (index * self.decimation as u128 / interpolation) as u64;
        let phase = (index % interpolation) as usize;
        let arm_taps = self.arm_taps;
        let ring_len = self.ring_len;
        let arm = &self.coeffs[phase * arm_taps..phase * arm_taps + arm_taps];
        let mut accumulator = 0.0f32;
        let mut ring_index = (base % ring_len as u64) as usize;
        for (tap, &coefficient) in arm.iter().enumerate() {
            if base >= tap as u64 {
                accumulator += coefficient * self.ring[ring_index];
            }
            // 历史缓冲逆序读：位置 (base - tap) % ring_len。
            ring_index = ring_index.wrapping_sub(1);
            if ring_index == usize::MAX {
                ring_index = ring_len - 1;
            }
        }
        accumulator
    }
}

/// 消费 `consumed` 个输入样本后可产出的输出样本总数：`ceil(consumed * I / D)`。
fn outputs_after(consumed: u64, interpolation: u32, decimation: u32) -> u64 {
    let numerator = (consumed as u128) * (interpolation as u128);
    let denominator = decimation as u128;
    numerator.div_ceil(denominator) as u64
}

/// 窗函数法设计相位交错系数（构造期一次算完；f64 计算，f32 存储）。
///
/// 理想低通 `h_ideal(t) = sin(omega_c * t) / (PI * t)`（`t = 0` 处取 `omega_c / PI`），乘 Hann
/// 窗后按相位归一化到每相之和 = 1。
fn design_coefficients(
    decimation: u32,
    interpolation: u32,
    filter_len: usize,
    cutoff_norm: f64,
) -> Vec<f32> {
    let phases = interpolation as usize;
    let arm_taps = filter_len / phases;
    let span = core::cmp::max(decimation, interpolation) as f64;
    let cutoff = cutoff_norm * PI / span;
    let center = (filter_len - 1) as f64 / 2.0;
    let window_denominator = (filter_len - 1) as f64;
    let mut prototype = vec![0.0f64; filter_len];
    for (index, slot) in prototype.iter_mut().enumerate() {
        let offset = index as f64 - center;
        let ideal = if offset == 0.0 {
            cutoff / PI
        } else {
            (cutoff * offset).sin() / (PI * offset)
        };
        let angle = 2.0 * PI * index as f64 / window_denominator;
        let window = if filter_len > 1 {
            0.5 - 0.5 * angle.cos()
        } else {
            1.0
        };
        *slot = ideal * window;
    }
    // 相位交错 -> 相位优先（phase-major）；每相单独归一化到直流增益 1。
    let mut coeffs = vec![0.0f32; filter_len];
    for phase in 0..phases {
        let arm_sum: f64 = prototype[phase..].iter().step_by(phases).sum();
        let scale = if arm_sum == 0.0 { 0.0 } else { 1.0 / arm_sum };
        for (tap, value) in prototype[phase..].iter().step_by(phases).enumerate() {
            coeffs[phase * arm_taps + tap] = (*value * scale) as f32;
        }
    }
    coeffs
}
