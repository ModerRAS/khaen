//! 合成时钟偏差测试台（仅供本 crate 的集成测试使用）。
//!
//! 模型：本地时钟按 `block_frames` 帧/块固定前进；发送端时钟速率 = 本地 × (1 + d)，
//! 媒体帧按 `packet_frames` 一包突发到达，与音频块边界不对齐（产生 fill 锯齿）。
//! 这样能在 Windows 本机、不碰真实 socket/PTP 的前提下复现"长流漂移"现场。
#![allow(dead_code)]

use std::collections::VecDeque;

use khaen_asrc::{Asrc, AsrcConfig, AsrcStatus, Interpolation};

/// 本地/媒体名义采样率（测试固定 48 kHz）。
pub const SAMPLE_RATE: u32 = 48_000;
/// 块帧数（= 引擎 DEFAULT_BLOCK_FRAMES）。
pub const BLOCK_FRAMES: usize = 128;
/// 测试信号频率（Hz）。
pub const TONE_HZ: f64 = 1_000.0;
/// 测试信号幅度（正弦峰值）。
pub const TONE_AMPLITUDE: f32 = 0.5;

/// 测试台参数。
#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    /// 声道数。
    pub channels: usize,
    /// 块帧数。
    pub block_frames: usize,
    /// 采样率。
    pub sample_rate: u32,
    /// 发送端一包多少媒体帧（典型 AES67 1 ms = 48 帧）。
    pub packet_frames: usize,
    /// 总仿真时长（秒）。
    pub run_seconds: f64,
    /// 统计窗口开始前的预热（秒）：排除启动/收敛瞬态。
    pub warmup_seconds: f64,
    /// 末尾收集输出音频的时长（秒，0 = 不收集）。
    pub collect_seconds: f64,
    /// 预填额外多写的帧数（帧，模拟启动时水位高于目标；0 = 正好目标）。
    pub prefill_overfill: usize,
    /// 信号频率。
    pub tone_hz: f64,
    /// 信号幅度。
    pub amplitude: f32,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            channels: 2,
            block_frames: BLOCK_FRAMES,
            sample_rate: SAMPLE_RATE,
            packet_frames: 48,
            run_seconds: 30.0,
            warmup_seconds: 15.0,
            collect_seconds: 0.0,
            prefill_overfill: 0,
            tone_hz: TONE_HZ,
            amplitude: TONE_AMPLITUDE,
        }
    }
}

/// 发送端时钟：按 (1 + drift_ppm·1e-6) 的速率产生 1 kHz 正弦媒体帧，
/// 并以整包突发的方式推入抖动环。
pub struct SenderClock {
    drift_ppm: f64,
    local_frames: u64,
    media_total: u64,
    emitted: u64,
    omega: f64,
    amplitude: f32,
}

impl SenderClock {
    /// 新发送端时钟（从媒体帧 0 开始）。
    pub fn new(drift_ppm: f64, sim: &SimConfig) -> Self {
        Self {
            drift_ppm,
            local_frames: 0,
            media_total: 0,
            emitted: 0,
            omega: std::f64::consts::TAU * sim.tone_hz / f64::from(sim.sample_rate),
            amplitude: sim.amplitude,
        }
    }

    /// 本地时钟前进 `local_frames` 帧，把这段时间内到达的整包媒体帧推入环。
    /// 返回仍未凑满一包的媒体帧数（< packet_frames）。
    pub fn advance(&mut self, sim: &SimConfig, ring: &mut VecDeque<f32>) -> usize {
        self.local_frames += sim.block_frames as u64;
        let target = (self.local_frames as f64 * (1.0 + self.drift_ppm * 1e-6)).floor() as u64;
        self.media_total = target;
        let packet = sim.packet_frames as u64;
        while self.emitted + packet <= self.media_total {
            for _ in 0..packet {
                let value = (self.omega * self.emitted as f64).sin() as f32 * self.amplitude;
                for _ in 0..sim.channels {
                    ring.push_back(value);
                }
                self.emitted += 1;
            }
        }
        (self.media_total - self.emitted) as usize
    }
}

/// 抖动环当前帧数（= 每声道样本数）。
pub fn ring_frames(ring: &VecDeque<f32>, channels: usize) -> usize {
    ring.len() / channels
}

/// ASRC 仿真结果。
#[derive(Clone, Debug)]
pub struct AsrcOutcome {
    /// 总块数。
    pub blocks: usize,
    /// 统计窗口内的欠载块数（输入帧不足）。
    pub underruns: usize,
    /// 窗口内 fill 最小值（帧）。
    pub fill_min: usize,
    /// 窗口内 fill 最大值（帧）。
    pub fill_max: usize,
    /// 窗口内 fill 均值（帧）。
    pub fill_mean: f64,
    /// 结束时的状态快照。
    pub status: AsrcStatus,
    /// 结束时的重采样器群延迟（输出帧）。
    pub output_delay: usize,
    /// 末尾收集的输出音频（帧交错；collect_seconds = 0 时为空）。
    pub output: Vec<f32>,
}

/// 用 ASRC 跑一次带发送端时钟偏差的仿真。
pub fn run_with_asrc(sim: &SimConfig, config: &AsrcConfig, drift_ppm: f64) -> AsrcOutcome {
    assert_eq!(config.channels, sim.channels);
    assert_eq!(config.block_frames, sim.block_frames);
    let mut asrc = Asrc::new(*config).expect("construct asrc");
    let mut ring: VecDeque<f32> = VecDeque::new();
    let mut clock = SenderClock::new(drift_ppm, sim);
    // 预填到目标水位（与 host 的 prefill gate 语义一致）。
    // 预填按包粒度推进（真实 host 也是收到包就写环，凑够水位即开始消费），
    // 避免一次推进一整块造成几百帧的初始过冲。
    let mut prefill_sim = *sim;
    prefill_sim.block_frames = sim.packet_frames;
    let prefill_target = config.target_fill_frames + sim.prefill_overfill;
    while ring_frames(&ring, sim.channels) < prefill_target {
        let _ = clock.advance(&prefill_sim, &mut ring);
    }
    let total_blocks =
        ((sim.run_seconds * f64::from(sim.sample_rate)) / sim.block_frames as f64).round() as usize;
    let warmup_blocks = ((sim.warmup_seconds * f64::from(sim.sample_rate))
        / sim.block_frames as f64)
        .round() as usize;
    let collect_blocks = ((sim.collect_seconds * f64::from(sim.sample_rate))
        / sim.block_frames as f64)
        .round() as usize;
    let collect_from = total_blocks.saturating_sub(collect_blocks);
    let mut underruns = 0usize;
    let mut fill_min = usize::MAX;
    let mut fill_max = 0usize;
    let mut fill_sum = 0f64;
    let mut fill_count = 0usize;
    let mut output = Vec::new();
    for block in 0..total_blocks {
        let _ = clock.advance(sim, &mut ring);
        let available = ring_frames(&ring, sim.channels);
        let need = asrc.input_frames_next();
        let take = available.min(need);
        {
            let input = asrc.input_mut();
            for frame in 0..take {
                for channel in 0..sim.channels {
                    let value = ring.pop_front().expect("ring has frame");
                    input.channel_mut(channel)[frame] = value;
                }
            }
            for frame in take..need {
                for channel in 0..sim.channels {
                    input.channel_mut(channel)[frame] = 0.0;
                }
            }
        }
        if take < need {
            underruns += 1;
        }
        asrc.process().expect("asrc process");
        let fill = ring_frames(&ring, sim.channels);
        asrc.update_fill(fill).expect("controller update");
        if block >= warmup_blocks {
            fill_min = fill_min.min(fill);
            fill_max = fill_max.max(fill);
            fill_sum += fill as f64;
            fill_count += 1;
        }
        if block >= collect_from && collect_blocks > 0 {
            let planar = asrc.output();
            for frame in 0..config.block_frames {
                for channel in 0..sim.channels {
                    output.push(planar.channel(channel)[frame]);
                }
            }
        }
    }
    let fill = ring_frames(&ring, sim.channels);
    AsrcOutcome {
        blocks: total_blocks,
        underruns,
        fill_min,
        fill_max,
        fill_mean: if fill_count == 0 {
            0.0
        } else {
            fill_sum / fill_count as f64
        },
        status: asrc.status(fill),
        output_delay: asrc.output_delay(),
        output,
    }
}

/// 关闭 ASRC 的对照仿真结果。
#[derive(Clone, Copy, Debug)]
pub struct LegacyOutcome {
    /// 统计窗口内欠载块数（fill < 一块）。
    pub underruns: usize,
    /// 统计窗口内抖动环溢出丢弃的帧数（模拟真实 feed_packet 的 overflow 丢包）。
    pub overruns: usize,
    /// 窗口内 fill 最小值（帧）。
    pub fill_min: usize,
    /// 窗口内 fill 最大值（帧）。
    pub fill_max: usize,
    /// 结束时 fill（帧）。
    pub fill_end: usize,
    /// 结束时相对初始水位的漂移（帧，正 = 涨）。
    pub fill_drift: i64,
}

/// 关闭 ASRC 的对照：抖动环 + 本地时钟硬消费（每块消费 block_frames 帧）。
pub fn run_without_asrc(
    sim: &SimConfig,
    drift_ppm: f64,
    target_fill_frames: usize,
    capacity_frames: usize,
) -> LegacyOutcome {
    let mut ring: VecDeque<f32> = VecDeque::new();
    let mut clock = SenderClock::new(drift_ppm, sim);
    let mut prefill_sim = *sim;
    prefill_sim.block_frames = sim.packet_frames;
    let prefill_target = target_fill_frames + sim.prefill_overfill;
    while ring_frames(&ring, sim.channels) < prefill_target {
        let _ = clock.advance(&prefill_sim, &mut ring);
    }
    let total_blocks =
        ((sim.run_seconds * f64::from(sim.sample_rate)) / sim.block_frames as f64).round() as usize;
    let warmup_blocks = ((sim.warmup_seconds * f64::from(sim.sample_rate))
        / sim.block_frames as f64)
        .round() as usize;
    let mut underruns = 0usize;
    let mut overruns = 0usize;
    let mut fill_min = usize::MAX;
    let mut fill_max = 0usize;
    let start_fill = ring_frames(&ring, sim.channels) as i64;
    for block in 0..total_blocks {
        let _ = clock.advance(sim, &mut ring);
        // 抖动环溢出：真实 feed_packet 在帧号超出窗口时整包丢弃；这里按帧裁掉最新到达的。
        let mut fill = ring_frames(&ring, sim.channels);
        while fill > capacity_frames {
            for _ in 0..sim.channels {
                let _ = ring.pop_back();
            }
            overruns += 1;
            fill -= 1;
        }
        // 本地时钟硬消费一块。
        if fill >= sim.block_frames {
            for _ in 0..sim.block_frames * sim.channels {
                let _ = ring.pop_front();
            }
        } else {
            if block >= warmup_blocks {
                underruns += 1;
            }
            ring.clear();
        }
        let fill = ring_frames(&ring, sim.channels);
        if block >= warmup_blocks {
            fill_min = fill_min.min(fill);
            fill_max = fill_max.max(fill);
        }
    }
    let fill_end = ring_frames(&ring, sim.channels);
    LegacyOutcome {
        underruns,
        overruns,
        fill_min,
        fill_max,
        fill_end,
        fill_drift: fill_end as i64 - start_fill,
    }
}

/// 按仿真参数构造 poly/sinc 配置。
pub fn asrc_config(
    sim: &SimConfig,
    interp: Interpolation,
    target_fill_frames: usize,
) -> AsrcConfig {
    let mut config = AsrcConfig::new(sim.channels, sim.block_frames, sim.sample_rate);
    config.interp = interp;
    config.target_fill_frames = target_fill_frames;
    config
}

/// 稳态 THD+N（dB，相对基波功率）：分窗最小二乘拟合的残差/基波功率比。
///
/// 每 10 ms（@1 kHz = 10.0 个周期，故意不是整数周期）窗口对
/// `[1, sin(ωt), cos(ωt)]` 做**精确最小二乘**（解 3×3 正规方程），
/// 残差功率 / 基波功率即 THD+N（含噪声与杂散，不含慢漂移：慢漂移由 ppm 估计测试覆盖）。
/// 不用 `2/N·Σ` 近似公式：非整数周期下近似式本身有 ~1e-4 的偏置，会淹没真实失真。
pub fn tone_thd_n_db(output: &[f32], channels: usize, sample_rate: u32, tone_hz: f64) -> f64 {
    let frames = output.len() / channels;
    assert!(frames >= 480, "need at least 10 ms of output");
    let window = (sample_rate / 100) as usize;
    assert!(
        window > 0 && frames >= window,
        "output shorter than one window"
    );
    let omega = std::f64::consts::TAU * tone_hz / f64::from(sample_rate);
    let samples: Vec<f64> = output
        .iter()
        .step_by(channels)
        .map(|&v| f64::from(v))
        .collect();
    let mut signal_total = 0.0f64;
    let mut residual_total = 0.0f64;
    let mut start = 0usize;
    while start + window <= frames {
        let slice = &samples[start..start + window];
        // 正规方程 AᵀA·x = Aᵀy（基函数 1, sin, cos）。
        let (mut s1, mut c1, mut s2, mut c2, mut sc) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let (mut y, mut ys, mut yc) = (0.0f64, 0.0f64, 0.0f64);
        for (index, &value) in slice.iter().enumerate() {
            let phase = omega * index as f64;
            let (sin_phase, cos_phase) = phase.sin_cos();
            s1 += sin_phase;
            c1 += cos_phase;
            s2 += sin_phase * sin_phase;
            c2 += cos_phase * cos_phase;
            sc += sin_phase * cos_phase;
            y += value;
            ys += value * sin_phase;
            yc += value * cos_phase;
        }
        let n = window as f64;
        let matrix = [[n, s1, c1], [s1, s2, sc], [c1, sc, c2]];
        let rhs = [y, ys, yc];
        let solution = solve3(matrix, rhs);
        let [offset, sin_amp, cos_amp] = solution;
        let signal_power = (sin_amp * sin_amp + cos_amp * cos_amp) / 2.0;
        let mut residual_power = 0.0f64;
        for (index, &value) in slice.iter().enumerate() {
            let phase = omega * index as f64;
            let fit = offset + sin_amp * phase.sin() + cos_amp * phase.cos();
            let residual = value - fit;
            residual_power += residual * residual;
        }
        residual_power /= n;
        signal_total += signal_power;
        residual_total += residual_power;
        start += window;
    }
    10.0 * (residual_total / signal_total).log10()
}

/// 3×3 线性方程组（列主元高斯消元；奇异时返回 0 解，避免 panic）。
fn solve3(mut matrix: [[f64; 3]; 3], mut rhs: [f64; 3]) -> [f64; 3] {
    for column in 0..3 {
        let mut pivot = column;
        for row in column + 1..3 {
            if matrix[row][column].abs() > matrix[pivot][column].abs() {
                pivot = row;
            }
        }
        if matrix[pivot][column].abs() < 1e-12 {
            return [0.0; 3];
        }
        matrix.swap(column, pivot);
        rhs.swap(column, pivot);
        let pivot_row = matrix[column];
        let pivot_value = pivot_row[column];
        for row in column + 1..3 {
            let factor = matrix[row][column] / pivot_value;
            for (inner, &value) in pivot_row.iter().enumerate().skip(column) {
                matrix[row][inner] -= factor * value;
            }
            rhs[row] -= factor * rhs[column];
        }
    }
    let mut solution = [0.0f64; 3];
    for row in (0..3).rev() {
        let mut sum = rhs[row];
        for column in row + 1..3 {
            sum -= matrix[row][column] * solution[column];
        }
        solution[row] = sum / matrix[row][row];
    }
    solution
}
