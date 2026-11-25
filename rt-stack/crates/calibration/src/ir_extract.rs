//! [NONRT] 采集 sweep -> 每输出通道反卷积 IR（goal 6：RT60/CSD 后处理的输入）。
//!
//! 测量阶段（sweep_capture / remeasure / holdout）的 capture raw 是“已知对数扫频”
//! 经房间/设备的响应；本模块用 Farina 式频谱除法把它变成每输出通道冲激响应：
//!
//! 1. 按 T61 时间轴重建该 slot 的精确播放波形（起点/终点频率、fade 包络、
//!    实际电平；与 SweepPlayer::sample_at 同一公式）；
//! 2. 采集段 = [sweep 起点 + seed, sweep 帧数 + tail_s]（seed = render frame 0
//!    对应的 capture 样本下标；T62 采集报告口径）；
//! 3. 频谱除法 H = Y*conj(X) / (|X|^2 + reg*max|X|^2)，ir = IFFT(H)，
//!    截到 sweep_frames + tail_s（与 tools/sweep-analysis/analyze_sweep.py 的
//!    deconvolve 完全同口径，reg=1e-5，tail_s=0.9 s）；
//! 4. 直达声峰（前 100 ms）对齐后截取 [peak-2 ms, peak+tail_s]；多 repeat 时
//!    对齐平均（提高窄带 EDC 稳定性）。
//!
//! 诚实边界：离线 fake 引擎没有房间衰减，反卷积得到的只是“直达脉冲 + 噪声”，
//! RT60 会被如实标 unmeasurable；真实房间 IR 的 RT60 由外部真实 IR 路径
//! （--rt60-ir）或白天窗口的真实采集产生 —— 本模块的 real-capture 分支
//! 在离线态属于 NOT_RUN（无设备/无声）。

use crate::sweep::SweepTimeline;

/// 反卷积默认尾部（s；与 Python 工具 --window-ms 900 ms 匹配）。
pub const DEFAULT_TAIL_S: f64 = 0.9;
/// 频谱除法正则化系数（相对 max|X|^2；analyze_sweep.py 同值）。
pub const DEFAULT_REG: f64 = 1e-5;
/// 直达声搜索窗（s）。
pub const DIRECT_SOUND_SEARCH_S: f64 = 0.1;
/// IR 前置余量（s；保留峰值前少量样本，便于与 Python 工具同口径找 onset）。
pub const IR_PRE_MARGIN_S: f64 = 0.002;

/// 反卷积配置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IrExtractConfig {
    /// 尾部长度（s）。
    pub tail_s: f64,
    /// 频谱除法正则化（相对）。
    pub reg: f64,
}

impl Default for IrExtractConfig {
    fn default() -> Self {
        IrExtractConfig {
            tail_s: DEFAULT_TAIL_S,
            reg: DEFAULT_REG,
        }
    }
}

impl IrExtractConfig {
    /// 校验。
    pub fn validate(&self) -> Result<(), String> {
        if !self.tail_s.is_finite() || !(0.05..=5.0).contains(&self.tail_s) {
            return Err("ir_extract: tail_s must be finite and in 0.05..=5.0".to_string());
        }
        if !self.reg.is_finite() || !(0.0..=1.0).contains(&self.reg) {
            return Err("ir_extract: reg must be finite and in 0..=1".to_string());
        }
        Ok(())
    }
}

/// 一个输出通道的反卷积 IR。
#[derive(Clone, Debug, PartialEq)]
pub struct DeconvolvedIr {
    /// 标签（speaker 名；如 FL）。
    pub label: String,
    /// layout 容器槽位。
    pub slot: u32,
    /// 播放设备通道号。
    pub playback_channel: u32,
    /// IR 样本（从直达声峰前 ~2 ms 起，已峰值归一化到 1.0）。
    pub ir: Vec<f64>,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 峰值下标（ir 内）。
    pub peak_index: usize,
    /// 平均的重复遍数。
    pub repeats_averaged: u32,
    /// 反卷积尾部（s）。
    pub tail_s: f64,
    /// sweep 起始频率（Hz）。
    pub sweep_start_hz: f64,
    /// sweep 结束频率（Hz）。
    pub sweep_end_hz: f64,
    /// sweep 单遍时长（s）。
    pub sweep_seconds: f64,
}

/// 重建一个 slot 的精确播放波形（与 SweepPlayer::sample_at 同一公式）。
/// 所有 slot 的激励相同，调用方可复用。
pub fn render_sweep(timeline: &SweepTimeline) -> Result<Vec<f64>, String> {
    let frames = timeline.actual_stimulus.sweep_frames as usize;
    if frames == 0 {
        return Err("ir_extract: zero sweep frames".to_string());
    }
    let sample_rate = timeline.sample_rate as f64;
    if sample_rate <= 0.0 {
        return Err("ir_extract: zero sample rate".to_string());
    }
    let f0 = timeline.stimulus.start_hz;
    let f1 = timeline.stimulus.end_hz;
    if !(f0 > 0.0) || !(f1 > f0) {
        return Err(format!("ir_extract: invalid sweep range {f0}..{f1}"));
    }
    let fade = timeline.actual_stimulus.fade_frames as usize;
    let duration = frames as f64 / sample_rate;
    let log_ratio = (f1 / f0).ln();
    let peak = timeline.level.peak_linear;
    let mut out = Vec::with_capacity(frames);
    for t in 0..frames {
        let time = t as f64 / sample_rate;
        let phase = 2.0
            * std::f64::consts::PI
            * f0
            * duration
            / log_ratio
            * ((time / duration * log_ratio).exp() - 1.0);
        let envelope = segment_envelope(t, frames, fade);
        out.push(peak * envelope * phase.sin());
    }
    Ok(out)
}

/// fade 包络（与 sweep.rs segment_envelope 一致）。
fn segment_envelope(t: usize, frames: usize, fade: usize) -> f64 {
    if fade == 0 || frames == 0 {
        return 1.0;
    }
    if t < fade {
        let denom = fade as f64 - 1.0;
        if denom <= 0.0 {
            return 1.0;
        }
        return 0.5 * (1.0 - (std::f64::consts::PI * t as f64 / denom).cos());
    }
    if t + fade >= frames {
        let local = t.saturating_sub(frames.saturating_sub(fade));
        let denom = fade as f64 - 1.0;
        if denom <= 0.0 {
            return 0.0;
        }
        return 0.5 * (1.0 + (std::f64::consts::PI * local as f64 / denom).cos());
    }
    1.0
}

/// Farina 式反卷积（与 analyze_sweep.py deconvolve 同口径）。
/// 实现在 `khaen_cal_dsp::deconvolve`，host 求解走同一函数。
pub fn deconvolve(seg: &[f64], sweep: &[f64], tail_frames: usize, reg: f64) -> Vec<f64> {
    khaen_cal_dsp::deconvolve(seg, sweep, tail_frames, reg)
}

fn argmax_abs(values: &[f64]) -> usize {
    let mut best = 0usize;
    let mut best_val = f64::NEG_INFINITY;
    for (i, v) in values.iter().enumerate() {
        if v.abs() > best_val {
            best_val = v.abs();
            best = i;
        }
    }
    best
}

/// 从一次 job 采集反卷积出每输出通道 IR。
///
/// - timeline：T61 时间轴（测量阶段同一条）；
/// - capture：单通道 dense 采集样本（f32；T62 CapturedStream::samples）；
/// - seed_render_frame：render frame 0 对应的 capture 样本下标（T62 采集报告）；
/// - 每个 slot 的 repeats 对齐平均（峰值对齐到 [peak-2ms]）。
pub fn extract_irs(
    timeline: &SweepTimeline,
    capture: &[f32],
    seed_render_frame: f64,
    config: &IrExtractConfig,
) -> Result<Vec<DeconvolvedIr>, String> {
    config.validate()?;
    if timeline.slots.is_empty() {
        return Err("ir_extract: timeline has no slots".to_string());
    }
    if capture.is_empty() {
        return Err("ir_extract: empty capture".to_string());
    }
    let sample_rate = timeline.sample_rate as f64;
    let sweep = render_sweep(timeline)?;
    let sweep_frames = sweep.len();
    let tail_frames = (config.tail_s * sample_rate) as usize;
    let search = (DIRECT_SOUND_SEARCH_S * sample_rate) as usize;
    let pre_margin = (IR_PRE_MARGIN_S * sample_rate) as usize;
    let mut out = Vec::with_capacity(timeline.slots.len());
    for slot in &timeline.slots {
        let mut aligned: Vec<Vec<f64>> = Vec::new();
        for &start_frame in &slot.repeat_render_frames {
            let capture_start = start_frame as f64 + seed_render_frame;
            if capture_start < 0.0 {
                return Err(format!(
                    "ir_extract: slot {} starts before capture (frame {start_frame}, seed {seed_render_frame})",
                    slot.speaker
                ));
            }
            let start = capture_start.round() as usize;
            let available = capture.len().saturating_sub(start);
            if available < sweep_frames {
                return Err(format!(
                    "ir_extract: capture too short for slot {} (need {} frames from {}, have {available})",
                    slot.speaker, sweep_frames, start
                ));
            }
            let seg_len = sweep_frames + tail_frames;
            let mut seg = vec![0.0f64; seg_len];
            let copy_len = available.min(seg_len);
            for i in 0..copy_len {
                seg[i] = capture[start + i] as f64;
            }
            let ir = deconvolve(&seg, &sweep, tail_frames, config.reg);
            let peak = argmax_abs(&ir[..ir.len().min(search).max(1)]);
            let trim_start = peak.saturating_sub(pre_margin);
            let trim_end = (peak + tail_frames).min(ir.len());
            if trim_end <= trim_start + 8 {
                return Err(format!(
                    "ir_extract: degenerate IR for slot {} (peak {peak})",
                    slot.speaker
                ));
            }
            aligned.push(ir[trim_start..trim_end].to_vec());
        }
        if aligned.is_empty() {
            return Err(format!("ir_extract: slot {} has no repeats", slot.speaker));
        }
        let length = aligned.iter().map(|ir| ir.len()).min().unwrap_or(0);
        if length < 16 {
            return Err(format!("ir_extract: slot {} IR too short", slot.speaker));
        }
        let repeats = aligned.len() as u32;
        let mut averaged = vec![0.0f64; length];
        for ir in &aligned {
            for i in 0..length {
                averaged[i] += ir[i];
            }
        }
        for value in averaged.iter_mut() {
            *value /= repeats as f64;
        }
        // 归一化基准 = 直达声峰（前 100 ms），与 Python analyze_ir 的 onset 口径一致；
        // 不用全段最大值，避免尾部交叉项/混叠旁瓣（幅度可能与直达声相当）被当成峰值。
        let peak = argmax_abs(&averaged[..averaged.len().min(search).max(1)]);
        let peak_abs = averaged[peak].abs();
        if !(peak_abs > 0.0) {
            return Err(format!("ir_extract: slot {} all-zero IR", slot.speaker));
        }
        for value in averaged.iter_mut() {
            *value /= peak_abs;
        }
        out.push(DeconvolvedIr {
            label: slot.speaker.clone(),
            slot: slot.slot,
            playback_channel: slot.channel,
            ir: averaged,
            sample_rate_hz: timeline.sample_rate,
            peak_index: peak,
            repeats_averaged: repeats,
            tail_s: config.tail_s,
            sweep_start_hz: timeline.stimulus.start_hz,
            sweep_end_hz: timeline.stimulus.end_hz,
            sweep_seconds: timeline.actual_stimulus.seconds,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microfft::{irfft, next_pow2_at_least, rfft, C64};
    use crate::rt60::{analyze_ir, Rt60Config};
    use crate::sweep::{SweepConfig, SweepOutput, SweepPlayer, SweepSessionKind, SweepTimeline};

    struct TestNoise {
        state: u64,
    }

    impl TestNoise {
        fn new(seed: u64) -> Self {
            TestNoise { state: seed | 1 }
        }
        fn next_u64(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.state
        }
        fn uniform(&mut self) -> f64 {
            ((self.next_u64() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64)
        }
        fn normal(&mut self) -> f64 {
            let u1 = self.uniform();
            let u2 = self.uniform();
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }

    /// 8 次独立实现平均的统计房间 IR（RT60=rt_true；直达声 + 指数衰减扩散尾）。
    fn synthetic_room_ir(rate: u32, rt_true_s: f64, seed: u64) -> Vec<f64> {
        let n = (1.0 * rate as f64) as usize;
        let mut rng = TestNoise::new(seed);
        let mut acc = vec![0.0f64; n];
        for _ in 0..8 {
            let mut ir = vec![0.0f64; n];
            for (i, value) in ir.iter_mut().enumerate() {
                let t = i as f64 / rate as f64;
                *value = 0.20 * 10.0f64.powf(-3.0 * t / rt_true_s) * rng.normal();
            }
            ir[0] += 1.0;
            let peak = ir.iter().fold(0.0f64, |a, v| a.max(v.abs()));
            for value in ir.iter_mut() {
                *value /= peak;
            }
            for (i, value) in acc.iter_mut().enumerate() {
                *value += ir[i];
            }
        }
        for value in acc.iter_mut() {
            *value /= 8.0;
        }
        acc
    }

    /// 线性卷积（FFT，测试辅助）。
    fn convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
        let nfft = next_pow2_at_least(a.len() + b.len());
        let fa = rfft(a, nfft);
        let fb = rfft(b, nfft);
        let prod: Vec<C64> = fa
            .iter()
            .zip(fb.iter())
            .map(|(x, y)| {
                C64::new(
                    x.re * y.re - x.im * y.im,
                    x.re * y.im + x.im * y.re,
                )
            })
            .collect();
        irfft(&prod, nfft)
    }

    fn make_config(seconds: f64) -> SweepConfig {
        let mut config = SweepConfig::acoustic(
            "ir-extract-test",
            vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)],
        );
        config.stimulus.seconds = seconds;
        config.stimulus.repeats = 1;
        config.stimulus.repeat_gap_seconds = 0.5;
        config.pre_seconds = 0.2;
        config.post_seconds = 0.2;
        config.playback_channels = 2;
        config.session_kind = SweepSessionKind::Acoustic;
        config
    }

    fn timeline_with(seconds: f64) -> SweepTimeline {
        let player = SweepPlayer::new(make_config(seconds)).expect("timeline");
        player.timeline().clone()
    }

    #[test]
    fn render_sweep_matches_player_render() {
        let timeline = timeline_with(0.2);
        let sweep = render_sweep(&timeline).expect("render");
        let player = SweepPlayer::new(make_config(0.2)).expect("player");
        let segment = timeline
            .segments
            .iter()
            .find(|s| s.kind.is_sweep())
            .expect("sweep segment")
            .clone();
        let block = timeline.block_frames as usize;
        let start_block = segment.start_frame / timeline.block_frames as u64;
        let blocks = segment.frames / timeline.block_frames as u64;
        let mut planar = vec![0.0f32; timeline.playback_channels as usize * block];
        let mut checked = 0usize;
        for seq in start_block..start_block + blocks {
            let info = player.render_block(seq, &mut planar).expect("render block");
            let channel = info
                .output_index
                .map(|i| player.timeline().output_channel(i as usize).unwrap())
                .unwrap_or(0) as usize;
            let base = channel * block;
            for frame in 0..block {
                let global = (seq - start_block) as usize * block + frame;
                if global >= sweep.len() {
                    break;
                }
                let want = sweep[global];
                let got = planar[base + frame] as f64;
                assert!((got - want).abs() < 1e-5, "frame {global}: {got} vs {want}");
                checked += 1;
            }
        }
        assert_eq!(checked, sweep.len());
    }

    #[test]
    fn deconvolve_recovers_impulse() {
        let rate = 48_000u32;
        let mut timeline = timeline_with(0.1);
        timeline.sample_rate = rate;
        let sweep = render_sweep(&timeline).expect("render");
        let tail = (0.2 * rate as f64) as usize;
        let mut seg: Vec<f64> = sweep.clone();
        seg.extend(vec![0.0f64; tail]);
        let ir = deconvolve(&seg, &sweep, tail, DEFAULT_REG);
        let peak = argmax_abs(&ir);
        assert!(peak < (0.01 * rate as f64) as usize, "peak index {peak}");
        let peak_abs = ir[peak].abs();
        assert!(peak_abs > 0.0);
        let total: f64 = ir.iter().map(|v| v * v).sum();
        assert!(
            peak_abs * peak_abs / total > 0.05,
            "IR not impulse-like: {:.4}",
            peak_abs * peak_abs / total
        );
    }

    #[test]
    fn extract_irs_from_synthetic_capture_produces_per_slot_labels() {
        let rate = 48_000u32;
        let mut timeline = timeline_with(0.1);
        timeline.sample_rate = rate;
        let player = SweepPlayer::new(make_config(0.1)).expect("player");
        let block = timeline.block_frames as usize;
        let mut planar = vec![0.0f32; timeline.playback_channels as usize * block];
        let mut render: Vec<f32> = Vec::with_capacity(timeline.total_frames as usize);
        for seq in 0..timeline.total_blocks {
            player.render_block(seq, &mut planar).expect("render");
            for frame in 0..block {
                let mut sum = 0.0f32;
                for channel in 0..timeline.playback_channels as usize {
                    sum += planar[channel * block + frame];
                }
                render.push(sum);
            }
        }
        // 房间：三个回波（1 ms 间隔），采集段前加 2400 样本 preroll
        let d = (0.001 * rate as f64) as usize;
        let mut capture: Vec<f32> = vec![0.0; render.len() + d * 3 + 2400];
        for (i, v) in render.iter().enumerate() {
            capture[i + 2400] += *v;
            capture[i + 2400 + d] += 0.5 * *v;
            capture[i + 2400 + 2 * d] += 0.2 * *v;
        }
        let irs =
            extract_irs(&timeline, &capture, 2400.0, &IrExtractConfig::default()).expect("extract");
        assert_eq!(irs.len(), 2);
        assert_eq!(irs[0].label, "FL");
        assert_eq!(irs[1].label, "FR");
        for ir in &irs {
            assert!(
                ir.peak_index < ir.ir.len() / 10,
                "peak={} len={} ir0={} maxabs={}",
                ir.peak_index,
                ir.ir.len(),
                ir.ir[0],
                ir.ir.iter().fold(0.0f64, |a, v| a.max(v.abs()))
            );
            assert!((ir.ir[ir.peak_index].abs() - 1.0).abs() < 1e-9);
            assert!(ir.ir.len() > rate as usize / 2);
        }
    }

    #[test]
    fn extract_irs_rejects_short_capture() {
        let timeline = timeline_with(0.1);
        let irs = extract_irs(&timeline, &[0.0f32; 100], 0.0, &IrExtractConfig::default());
        assert!(irs.is_err());
    }

    #[test]
    fn deconvolved_capture_recovers_synthetic_rt60() {
        // 端到端（离线）：已知 sweep 播放波形 x 房间 IR -> 采集 capture -> Rust 反卷积 ->
        // RT60 分析；验证 auto 路径（job 采集反卷积）能恢复已知 0.50 s 的 RT60。
        let rate = 48_000u32;
        // 0.5 s 的 gap（>= 0.9 s 尾窗？这里反而要用更长 gap 避免下一 slot 扫频落进本 slot 尾窗）。
        let mut sweep_config = make_config(0.2);
        sweep_config.stimulus.repeat_gap_seconds = 1.2;
        let player = SweepPlayer::new(sweep_config).expect("player");
        let timeline = player.timeline().clone();
        assert_eq!(timeline.sample_rate, rate);
        let block = timeline.block_frames as usize;
        let mut planar = vec![0.0f32; timeline.playback_channels as usize * block];
        let mut render: Vec<f64> = Vec::with_capacity(timeline.total_frames as usize);
        for seq in 0..timeline.total_blocks {
            player.render_block(seq, &mut planar).expect("render");
            for frame in 0..block {
                let mut sum = 0.0f64;
                for channel in 0..timeline.playback_channels as usize {
                    sum += planar[channel * block + frame] as f64;
                }
                render.push(sum);
            }
        }
        let rt_true = 0.5f64;
        let room = synthetic_room_ir(rate, rt_true, 0x5EED_00A5);
        let captured = convolve(&render, &room);
        let capture: Vec<f32> = captured.iter().map(|v| *v as f32).collect();
        let irs = extract_irs(&timeline, &capture, 0.0, &IrExtractConfig::default())
            .expect("extract IRs");
        assert_eq!(irs.len(), 2);
        let config = Rt60Config {
            bands_hz: vec![1000.0, 2000.0, 4000.0],
            ..Rt60Config::default()
        };
        let mut failures = Vec::new();
        for ir in &irs {
            let analysis = analyze_ir(&ir.ir, rate, &ir.label, &config).expect("rt60 analysis");
            for band in &analysis.bands {
                match band.rt60_s {
                    Some(got) if (got - rt_true).abs() <= 0.08 => {}
                    other => failures.push(format!(
                        "{} fc={:.0} got={other:?} valid={} flags={:?}",
                        ir.label, band.center_hz, band.valid, band.flags
                    )),
                }
            }
        }
        assert!(
            failures.is_empty(),
            "deconvolution + RT60 end-to-end failures: {failures:#?}"
        );
    }
}

