//! protection 段：每 lane 的保护（延迟 / 极性 / 电平 / PEQ）。
//!
//! 处理顺序固定：延迟 -> 电平 -> 极性 -> PEQ（PEQ 为 Direct Form I biquad 级联）。全部在
//! planar lane 平面上原地完成，延迟线状态与 PEQ 状态在编译期分配。`delay == 0`、
//! `gain_db == 0`、`polarity == +1`、无 PEQ（或每段 PEQ 为 `b0 = 1`，其余 0 的恒等段）的 lane
//! 在编译期折叠为直通（identity 判据）。

use khaen_core::plan::{PeqSection, ProtectionSection};

use super::CompileError;

/// 归一化 biquad 段（`a0 = 1`）。
#[derive(Debug)]
struct Biquad {
    /// 前馈系数 b0。
    b0: f32,
    /// 前馈系数 b1。
    b1: f32,
    /// 前馈系数 b2。
    b2: f32,
    /// 反馈系数 a1。
    a1: f32,
    /// 反馈系数 a2。
    a2: f32,
}

/// 单 lane 的保护状态。
#[derive(Debug)]
struct ProtectionLane {
    /// 延迟样本数。
    delay: usize,
    /// 延迟线（长度 = delay）。
    delay_buf: Vec<f32>,
    /// 延迟线写指针。
    delay_index: usize,
    /// dB -> 线性增益后的电平系数。
    gain: f32,
    /// 极性系数（+1.0 / -1.0）。
    polarity: f32,
    /// PEQ 段级联（恒等段已在编译期移除）。
    peq: Vec<Biquad>,
    /// 每段 4 个状态：x1, x2, y1, y2。
    peq_state: Vec<f32>,
    /// 编译期折叠后的直通 lane。
    bypass: bool,
}

impl ProtectionLane {
    /// 恒等保护 lane。
    fn identity() -> Self {
        Self {
            delay: 0,
            delay_buf: Vec::new(),
            delay_index: 0,
            gain: 1.0,
            polarity: 1.0,
            peq: Vec::new(),
            peq_state: Vec::new(),
            bypass: true,
        }
    }
}

/// protection 段状态：每 lane 一条保护链。
#[derive(Debug)]
pub(crate) struct ProtectionStage {
    /// 每 lane 的保护状态。
    lanes: Vec<ProtectionLane>,
}

impl ProtectionStage {
    /// 编译 protection 段；缺少区段时整段直通。
    pub(crate) fn compile(
        section: Option<&ProtectionSection>,
        lanes: usize,
    ) -> Result<Self, CompileError> {
        let mut states: Vec<ProtectionLane> =
            (0..lanes).map(|_| ProtectionLane::identity()).collect();
        if let Some(section) = section {
            let mut seen = vec![false; lanes];
            for entry in &section.entries {
                let lane = entry.lane as usize;
                if lane >= lanes {
                    return Err(CompileError::InvalidSection(
                        "protection entry lane out of range",
                    ));
                }
                if seen[lane] {
                    return Err(CompileError::InvalidSection("duplicate protection lane"));
                }
                seen[lane] = true;
                if entry.polarity != 1 && entry.polarity != -1 {
                    return Err(CompileError::InvalidSection(
                        "protection polarity must be +1 or -1",
                    ));
                }
                if !entry.gain_db.is_finite() {
                    return Err(CompileError::NonFiniteCoefficient("protection gain_db"));
                }
                let gain = if entry.gain_db == 0.0 {
                    1.0
                } else {
                    10f32.powf(entry.gain_db / 20.0)
                };
                if !gain.is_finite() {
                    return Err(CompileError::NonFiniteCoefficient("protection gain_db"));
                }
                let mut peq = Vec::new();
                for peq_section in &entry.peq {
                    for coeff in [
                        peq_section.b0,
                        peq_section.b1,
                        peq_section.b2,
                        peq_section.a1,
                        peq_section.a2,
                    ] {
                        if !coeff.is_finite() {
                            return Err(CompileError::NonFiniteCoefficient("protection peq"));
                        }
                    }
                    // identity 判据：b0 == 1 其余 0 的恒等段在编译期移除。
                    if is_identity_peq(peq_section) {
                        continue;
                    }
                    peq.push(Biquad {
                        b0: peq_section.b0,
                        b1: peq_section.b1,
                        b2: peq_section.b2,
                        a1: peq_section.a1,
                        a2: peq_section.a2,
                    });
                }
                let delay = entry.delay as usize;
                let bypass = delay == 0 && gain == 1.0 && entry.polarity == 1 && peq.is_empty();
                states[lane] = ProtectionLane {
                    delay,
                    delay_buf: vec![0.0f32; delay],
                    delay_index: 0,
                    gain,
                    polarity: entry.polarity as f32,
                    peq_state: vec![0.0f32; peq.len() * 4],
                    peq,
                    bypass,
                };
            }
        }
        Ok(Self { lanes: states })
    }

    /// 处理一个子块：在矩阵输出平面上原地完成。
    pub(crate) fn process(&mut self, inout: &mut [f32], stride: usize, frames: usize) {
        for (lane, state) in self.lanes.iter_mut().enumerate() {
            if state.bypass {
                continue;
            }
            let plane = &mut inout[lane * stride..lane * stride + frames];
            if state.delay > 0 {
                let delay = state.delay;
                let mut frame = 0;
                while frame < frames {
                    let delayed = state.delay_buf[state.delay_index];
                    state.delay_buf[state.delay_index] = plane[frame];
                    state.delay_index += 1;
                    if state.delay_index == delay {
                        state.delay_index = 0;
                    }
                    plane[frame] = delayed;
                    frame += 1;
                }
            }
            let mut frame = 0;
            while frame < frames {
                let mut sample = plane[frame] * state.gain * state.polarity;
                let mut section = 0;
                while section < state.peq.len() {
                    let start = section * 4;
                    let output = biquad(
                        &state.peq[section],
                        &mut state.peq_state[start..start + 4],
                        sample,
                    );
                    sample = output;
                    section += 1;
                }
                plane[frame] = sample;
                frame += 1;
            }
        }
    }
}

/// 恒等 PEQ 段判定：`b0 = 1`，其余系数为 0。
fn is_identity_peq(section: &PeqSection) -> bool {
    section.b0 == 1.0
        && section.b1 == 0.0
        && section.b2 == 0.0
        && section.a1 == 0.0
        && section.a2 == 0.0
}

/// Direct Form I biquad：一次处理一个样本并推进状态（`state = [x1, x2, y1, y2]`）。
fn biquad(section: &Biquad, state: &mut [f32], sample: f32) -> f32 {
    let output = section.b0 * sample + section.b1 * state[0] + section.b2 * state[1]
        - section.a1 * state[2]
        - section.a2 * state[3];
    state[1] = state[0];
    state[0] = sample;
    state[3] = state[2];
    state[2] = output;
    output
}
