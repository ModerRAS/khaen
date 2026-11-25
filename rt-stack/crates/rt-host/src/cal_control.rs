//! 校准控制面。
//!
//! Unix socket 在非实时线程上接受 `status`、`enter`、`measure`、`solve`、
//! `remeasure`、`leave`。音频线程只在校准模式改写输入缓冲，播放模式立刻返回。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use tspi_cal_dsp::deconvolve;
use tspi_cal_dsp::peq_fit::fit_target_fir_only;
use tspi_dsp_core::mimo12k::Mimo12kRuntime;
use tspi_mimo_design::emperor::{
    decim_lowpass, decimate_48_to_12, design_channel, low_extension_hz, DecayMetric, EmperorParams,
};

const MODE_PLAYBACK: u8 = 0;
const MODE_CAL: u8 = 1;
const SWEEP_OFF: u8 = 0;
const SWEEP_LOW: u8 = 1;
const SWEEP_FULL: u8 = 2;
const PASS_IDLE: u8 = 0;
const PASS_LOW_IDENTITY: u8 = 1;
const PASS_LOW_SIDE: u8 = 2;
const PASS_FULL_IDENTITY: u8 = 3;
const PASS_FULL_FIR: u8 = 4;
const SWEEP_RATE_HZ: f64 = 48_000.0;
const SWEEP_SECONDS: f64 = 5.0;
/// −12 dBFS。10^(−12/20)。
const SWEEP_LEVEL: f32 = 0.251_188_64;
const MAX_REC: usize = 48_000 * 5;
const RESAMP_TAPS: usize = 95;
const IR_TAIL_FRAMES: usize = 48_000;

static HUB: OnceLock<CalControl> = OnceLock::new();
static SERVER_STARTED: AtomicBool = AtomicBool::new(false);

const CHANNEL_NAMES: [&str; 7] = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];

#[derive(Default)]
struct Takes {
    rec_play: Vec<f64>,
    rec_mic: Vec<f64>,
    low_play: Vec<f64>,
    low_mic: Vec<f64>,
    full_play: Vec<f64>,
    full_mic: Vec<f64>,
    low_ir: Vec<f64>,
    full_ir: Vec<f64>,
}

struct Play {
    g: Vec<f32>,
    fir: Vec<f32>,
    g_hist: Vec<f32>,
    fir_hist: Vec<f32>,
    g_pos: usize,
    fir_pos: usize,
    armed_pass: u8,
    /// 低段复测时乘在全部输出声道上的线性增益。1 表示没有正增益。
    pre_gain: f32,
    /// 12 kHz 预延迟。直通要延迟 `4·D12` 再加两段重采样群延迟，修正本身不含这段。
    predelay_low: usize,
    side: Option<SideBranch>,
}

struct SideBranch {
    runtime: Mimo12kRuntime,
    corr: Vec<f32>,
    dry_hist: Vec<f32>,
    dry_pos: usize,
}

impl Play {
    fn empty() -> Self {
        Self {
            g: Vec::new(),
            fir: Vec::new(),
            g_hist: Vec::new(),
            fir_hist: Vec::new(),
            g_pos: 0,
            fir_pos: 0,
            armed_pass: PASS_IDLE,
            pre_gain: 1.0,
            predelay_low: 0,
            side: None,
        }
    }
}

fn build_side(g: &[f32], predelay_low: usize, block: usize) -> Option<SideBranch> {
    if g.is_empty() || predelay_low >= g.len() || block < 4 || block % 4 != 0 {
        return None;
    }
    let decim = decim_lowpass(RESAMP_TAPS).ok()?;
    let decim: Vec<f32> = decim.into_iter().map(|value| value as f32).collect();
    let interp: Vec<f32> = decim.iter().map(|value| value * 4.0).collect();
    let runtime = Mimo12kRuntime::build_from_design(
        1,
        g.len(),
        block,
        RESAMP_TAPS,
        predelay_low,
        g,
        &decim,
        &interp,
    )
    .ok()?;
    let delay = runtime.delay48() as usize;
    Some(SideBranch {
        runtime,
        corr: vec![0.0; block],
        dry_hist: vec![0.0; delay],
        dry_pos: 0,
    })
}

fn delay_block(hist: &mut [f32], pos: &mut usize, block: &mut [f32]) {
    if hist.is_empty() {
        return;
    }
    for sample in block {
        let delayed = hist[*pos];
        hist[*pos] = *sample;
        *sample = delayed;
        *pos += 1;
        if *pos == hist.len() {
            *pos = 0;
        }
    }
}

struct Solved {
    g_pp: Vec<f64>,
    target_fir: Vec<f64>,
    target: String,
    f_hi: f64,
    f_lo: f64,
    supports: Vec<String>,
    metric: String,
    engine_playable: bool,
    pre_gain_db: f64,
}

/// 一次 host 进程里的校准门。初始状态是播放。
pub struct CalControl {
    mode: AtomicU8,
    umik_open: AtomicBool,
    sweep: AtomicU8,
    pass: AtomicU8,
    samples_left: AtomicU64,
    sweep_pos: AtomicU64,
    sweep_total: AtomicU64,
    phase_bits: AtomicU64,
    low_identity_blocks: AtomicU64,
    low_side_blocks: AtomicU64,
    full_identity_blocks: AtomicU64,
    full_fir_blocks: AtomicU64,
    primary_ir: Mutex<Vec<f64>>,
    solved: Mutex<Option<Solved>>,
    takes: Mutex<Takes>,
    play: Mutex<Play>,
}

impl CalControl {
    fn new() -> Self {
        Self {
            mode: AtomicU8::new(MODE_PLAYBACK),
            umik_open: AtomicBool::new(false),
            sweep: AtomicU8::new(SWEEP_OFF),
            pass: AtomicU8::new(PASS_IDLE),
            samples_left: AtomicU64::new(0),
            sweep_pos: AtomicU64::new(0),
            sweep_total: AtomicU64::new(0),
            phase_bits: AtomicU64::new(0),
            low_identity_blocks: AtomicU64::new(0),
            low_side_blocks: AtomicU64::new(0),
            full_identity_blocks: AtomicU64::new(0),
            full_fir_blocks: AtomicU64::new(0),
            primary_ir: Mutex::new(Vec::new()),
            solved: Mutex::new(None),
            takes: Mutex::new(Takes::default()),
            play: Mutex::new(Play::empty()),
        }
    }

    pub fn is_cal(&self) -> bool {
        self.mode.load(Ordering::Acquire) == MODE_CAL
    }

    /// 单测注入 12 kHz 主喇叭脉冲响应。没有这份数据时 `solve` 拒绝下结论。
    pub fn set_primary_ir(&self, ir: Vec<f64>) {
        if let Ok(mut slot) = self.primary_ir.lock() {
            *slot = ir;
        }
    }

    pub fn handle(&self, line: &str) -> String {
        let op = json_string(line, "op").unwrap_or_default();
        let audible = json_bool(line, "audible").unwrap_or(false);
        match op.as_str() {
            "status" => self.status_json(true, ""),
            "enter" => self.enter(audible),
            "leave" => self.leave(),
            "measure" => self.measure(audible),
            "remeasure" => self.remeasure(audible),
            "solve" => self.solve(line),
            _ => self.status_json(false, "unknown_op"),
        }
    }

    fn enter(&self, audible: bool) -> String {
        if !audible {
            return self.status_json(false, "audible_authorization_required");
        }
        if self.mode.load(Ordering::Acquire) != MODE_CAL {
            if let Err(error) = self.open_umik() {
                return self.fail(&error);
            }
            self.sweep.store(SWEEP_OFF, Ordering::Release);
            self.pass.store(PASS_IDLE, Ordering::Release);
            self.samples_left.store(0, Ordering::Release);
            self.clear_takes();
            self.mode.store(MODE_CAL, Ordering::Release);
        }
        self.status_json(true, "")
    }

    fn leave(&self) -> String {
        self.sweep.store(SWEEP_OFF, Ordering::Release);
        self.pass.store(PASS_IDLE, Ordering::Release);
        self.samples_left.store(0, Ordering::Release);
        self.close_umik();
        self.mode.store(MODE_PLAYBACK, Ordering::Release);
        self.status_json(true, "")
    }

    fn measure(&self, audible: bool) -> String {
        if self.mode.load(Ordering::Acquire) != MODE_CAL {
            return self.status_json(false, "not_in_calibration");
        }
        if !audible {
            return self.status_json(false, "audible_authorization_required");
        }
        if !self.umik_open.load(Ordering::Acquire) {
            return self.status_json(false, "umik_closed");
        }
        self.pass.store(PASS_IDLE, Ordering::Release);
        self.arm_sweep(SWEEP_LOW);
        self.status_json(true, "")
    }

    fn remeasure(&self, audible: bool) -> String {
        if self.mode.load(Ordering::Acquire) != MODE_CAL {
            return self.status_json(false, "not_in_calibration");
        }
        if !audible {
            return self.status_json(false, "audible_authorization_required");
        }
        let ready = self
            .solved
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|_| ()))
            .is_some();
        if !ready {
            return self.status_json(false, "no_solution");
        }
        self.low_identity_blocks.store(0, Ordering::Release);
        self.low_side_blocks.store(0, Ordering::Release);
        self.full_identity_blocks.store(0, Ordering::Release);
        self.full_fir_blocks.store(0, Ordering::Release);
        self.pass.store(PASS_LOW_IDENTITY, Ordering::Release);
        self.arm_sweep(SWEEP_LOW);
        self.status_json(true, "")
    }

    fn solve(&self, line: &str) -> String {
        let f_hi = json_f64(line, "f_hi").unwrap_or(150.0);
        let target = json_string(line, "target").unwrap_or_else(|| "flat".to_string());
        let supports = json_string_array(line, "supports");
        if let Some(name) = supports
            .iter()
            .find(|name| !CHANNEL_NAMES.contains(&name.as_str()))
        {
            return self.fail(&format!("unknown_support:{name}"));
        }
        self.realize_recordings();
        let (low_ir, full_ir) = self
            .takes
            .lock()
            .map(|slot| (slot.low_ir.clone(), slot.full_ir.clone()))
            .unwrap_or_default();
        let primary = if !low_ir.is_empty() {
            match decimate_48_to_12(&low_ir, RESAMP_TAPS) {
                Ok(decimated) => window_from_peak(&decimated, 24_000),
                Err(error) => return self.fail(&error),
            }
        } else {
            self.primary_ir
                .lock()
                .map(|slot| slot.clone())
                .unwrap_or_default()
        };
        if primary.is_empty() {
            return self.status_json(false, "no_measurement");
        }
        if !supports.is_empty() {
            return self.status_json(false, "support_ir_missing");
        }
        let mut params = EmperorParams::default();
        params.f_hi = f_hi;
        if let Some(hz) = low_extension_hz(&primary, params.fs) {
            if hz < params.f_hi {
                params.f_lo = hz;
            }
        }
        let designed = match design_channel(&primary, &[], &params) {
            Ok(designed) => designed,
            Err(error) => return self.fail(&error),
        };
        let fitted = match fit_target_fir_only(&full_ir, &target) {
            Ok(fitted) => fitted,
            Err(error) => return self.fail(&error),
        };
        let fir = fitted.coeffs;
        self.install_correction(&designed.g_primary, &fir, designed.pre_gain_db, params.d12);
        let metric = match designed.metric {
            DecayMetric::T30 => "T30",
            DecayMetric::T20 => "T20",
        };
        let solved = Solved {
            g_pp: designed.g_primary,
            target_fir: fir,
            target,
            f_hi,
            f_lo: params.f_lo,
            supports,
            metric: metric.to_string(),
            engine_playable: designed.engine_playable,
            pre_gain_db: designed.pre_gain_db,
        };
        let body = self.solved_json(&solved, low_ir.is_empty());
        if let Ok(mut slot) = self.solved.lock() {
            *slot = Some(solved);
        }
        body
    }

    /// 校准模式时清掉节目样本。扫频已武装时，只往第一路写发生器。
    pub fn overlay(&self, input: &mut [f32], stride: usize, block: usize) -> bool {
        if self.mode.load(Ordering::Acquire) != MODE_CAL {
            return false;
        }
        if block == 0 || stride < block || input.is_empty() {
            return true;
        }
        let channels = input.len() / stride;
        for channel in 0..channels {
            let start = channel * stride;
            input[start..start + block].fill(0.0);
        }
        self.render_sweep(input, stride, block, channels);
        true
    }

    /// 图跑完之后记一块。只有复测档位会增加计数。
    pub fn note_chain(&self) {
        match self.pass.load(Ordering::Acquire) {
            PASS_LOW_IDENTITY => {
                self.low_identity_blocks.fetch_add(1, Ordering::Release);
            }
            PASS_LOW_SIDE => {
                self.low_side_blocks.fetch_add(1, Ordering::Release);
            }
            PASS_FULL_IDENTITY => {
                self.full_identity_blocks.fetch_add(1, Ordering::Release);
            }
            PASS_FULL_FIR => {
                self.full_fir_blocks.fetch_add(1, Ordering::Release);
            }
            _ => {}
        }
    }

    /// 当前扫频走完后进入下一次复测。侧路打开时目标 FIR 仍是恒等档。
    pub fn advance_remeasure_pass(&self) {
        let next = match self.pass.load(Ordering::Acquire) {
            PASS_LOW_IDENTITY => PASS_LOW_SIDE,
            PASS_LOW_SIDE => PASS_FULL_IDENTITY,
            PASS_FULL_IDENTITY => PASS_FULL_FIR,
            _ => PASS_IDLE,
        };
        self.pass.store(next, Ordering::Release);
        if next == PASS_LOW_SIDE || next == PASS_LOW_IDENTITY {
            self.arm_sweep(SWEEP_LOW);
        } else if next == PASS_FULL_IDENTITY || next == PASS_FULL_FIR {
            self.arm_sweep(SWEEP_FULL);
        } else {
            self.sweep.store(SWEEP_OFF, Ordering::Release);
        }
    }

    pub fn side_path_open(&self) -> bool {
        self.pass.load(Ordering::Acquire) == PASS_LOW_SIDE
    }

    pub fn post_is_identity(&self) -> bool {
        self.pass.load(Ordering::Acquire) != PASS_FULL_FIR
    }

    fn arm_sweep(&self, kind: u8) {
        let total = (SWEEP_RATE_HZ * SWEEP_SECONDS) as u64;
        self.phase_bits.store(0.4f64.to_bits(), Ordering::Release);
        self.sweep_pos.store(0, Ordering::Release);
        self.sweep_total.store(total, Ordering::Release);
        self.samples_left.store(total, Ordering::Release);
        self.sweep.store(kind, Ordering::Release);
    }

    fn render_sweep(&self, input: &mut [f32], _stride: usize, block: usize, _channels: usize) {
        let kind = self.sweep.load(Ordering::Acquire);
        if kind == SWEEP_OFF {
            return;
        }
        let left = self.samples_left.load(Ordering::Acquire);
        if left == 0 {
            self.sweep.store(SWEEP_OFF, Ordering::Release);
            return;
        }
        let take = (block as u64).min(left) as usize;
        let (f0, f1) = if kind == SWEEP_LOW {
            (20.0_f64, 500.0_f64)
        } else {
            (20.0_f64, 20_000.0_f64)
        };
        let total = self.sweep_total.load(Ordering::Acquire).max(1);
        let start = self.sweep_pos.load(Ordering::Acquire);
        let mut phase = f64::from_bits(self.phase_bits.load(Ordering::Acquire));
        let ln_ratio = (f1 / f0).ln();
        let dest = &mut input[..take];
        for (index, sample) in dest.iter_mut().enumerate() {
            let frac = (start + index as u64) as f64 / total as f64;
            let freq = f0 * (frac * ln_ratio).exp();
            *sample = (phase.sin() as f32) * SWEEP_LEVEL;
            phase += std::f64::consts::TAU * freq / SWEEP_RATE_HZ;
            if phase >= std::f64::consts::TAU {
                phase -= std::f64::consts::TAU;
            }
        }
        let written = take as u64;
        self.phase_bits.store(phase.to_bits(), Ordering::Release);
        self.sweep_pos.store(start + written, Ordering::Release);
        let remain = left - written;
        self.samples_left.store(remain, Ordering::Release);
        self.record_play(&input[..take], kind);
        if remain == 0 {
            let pass = self.pass.load(Ordering::Acquire);
            if pass == PASS_IDLE {
                self.freeze_take(kind);
                if kind == SWEEP_LOW {
                    self.arm_sweep(SWEEP_FULL);
                } else {
                    self.sweep.store(SWEEP_OFF, Ordering::Release);
                }
            } else {
                self.advance_remeasure_pass();
            }
        }
    }

    /// 把一次扫频的播放参考和麦信号交给求解。低频和全频分开放。
    pub fn submit_sweep_capture(&self, full_band: bool, play: Vec<f64>, mic: Vec<f64>) {
        if let Ok(mut takes) = self.takes.lock() {
            if full_band {
                takes.full_play = play;
                takes.full_mic = mic;
                takes.full_ir.clear();
            } else {
                takes.low_play = play;
                takes.low_mic = mic;
                takes.low_ir.clear();
            }
        }
    }

    pub fn push_mic(&self, samples: &[f64]) {
        if self.mode.load(Ordering::Acquire) != MODE_CAL
            || self.pass.load(Ordering::Acquire) != PASS_IDLE
            || self.sweep.load(Ordering::Acquire) == SWEEP_OFF
        {
            return;
        }
        let Ok(mut takes) = self.takes.lock() else {
            return;
        };
        let room = MAX_REC.saturating_sub(takes.rec_mic.len());
        let count = samples.len().min(room);
        takes.rec_mic.extend_from_slice(&samples[..count]);
    }

    fn record_play(&self, samples: &[f32], kind: u8) {
        if self.pass.load(Ordering::Acquire) != PASS_IDLE || kind == SWEEP_OFF {
            return;
        }
        let Ok(mut takes) = self.takes.lock() else {
            return;
        };
        let room = MAX_REC.saturating_sub(takes.rec_play.len());
        let count = samples.len().min(room);
        takes
            .rec_play
            .extend(samples[..count].iter().map(|sample| f64::from(*sample)));
    }

    fn freeze_take(&self, kind: u8) {
        let Ok(mut takes) = self.takes.lock() else {
            return;
        };
        let play = std::mem::take(&mut takes.rec_play);
        let mic = std::mem::take(&mut takes.rec_mic);
        if kind == SWEEP_LOW {
            takes.low_play = play;
            takes.low_mic = mic;
            takes.low_ir.clear();
        } else if kind == SWEEP_FULL {
            takes.full_play = play;
            takes.full_mic = mic;
            takes.full_ir.clear();
        }
    }

    fn clear_takes(&self) {
        if let Ok(mut takes) = self.takes.lock() {
            *takes = Takes::default();
        }
    }

    fn realize_recordings(&self) {
        let (low_pair, full_pair) = {
            let Ok(takes) = self.takes.lock() else {
                return;
            };
            let low_pair = if takes.low_ir.is_empty() {
                Some((takes.low_play.clone(), takes.low_mic.clone()))
            } else {
                None
            };
            let full_pair = if takes.full_ir.is_empty() {
                Some((takes.full_play.clone(), takes.full_mic.clone()))
            } else {
                None
            };
            (low_pair, full_pair)
        };
        let low_ir = low_pair
            .as_ref()
            .map(|(play, mic)| ir_from_pair(play, mic))
            .unwrap_or_default();
        let full_ir = full_pair
            .as_ref()
            .map(|(play, mic)| ir_from_pair(play, mic))
            .unwrap_or_default();
        let Ok(mut takes) = self.takes.lock() else {
            return;
        };
        if low_pair.is_some() && !low_ir.is_empty() {
            takes.low_ir = low_ir;
        }
        if full_pair.is_some() && !full_ir.is_empty() {
            takes.full_ir = full_ir;
        }
    }

    fn install_correction(&self, g: &[f64], fir: &[f64], pre_gain_db: f64, predelay_low: usize) {
        let Ok(mut play) = self.play.lock() else {
            return;
        };
        play.g = g.iter().map(|value| *value as f32).collect();
        play.fir = fir.iter().map(|value| *value as f32).collect();
        play.g_hist = vec![0.0; play.g.len()];
        play.fir_hist = vec![0.0; play.fir.len()];
        play.g_pos = 0;
        play.fir_pos = 0;
        play.armed_pass = PASS_IDLE;
        play.pre_gain = 10f32.powf(pre_gain_db as f32 / 20.0);
        play.predelay_low = predelay_low;
        play.side = None;
    }

    /// 复测时改播放缓冲。侧路和目标 FIR 不同时加上。
    pub fn apply_output(
        &self,
        input: &[f32],
        in_stride: usize,
        output: &mut [f32],
        out_stride: usize,
        block: usize,
    ) {
        let pass = self.pass.load(Ordering::Acquire);
        if pass != PASS_LOW_SIDE && pass != PASS_FULL_FIR {
            return;
        }
        if block == 0 || in_stride < block || out_stride < block {
            return;
        }
        if input.len() < block || output.len() < block {
            return;
        }
        let Ok(mut play) = self.play.lock() else {
            return;
        };
        if play.armed_pass != pass {
            play.g_hist.fill(0.0);
            play.fir_hist.fill(0.0);
            play.g_pos = 0;
            play.fir_pos = 0;
            play.armed_pass = pass;
        }
        if pass == PASS_LOW_SIDE {
            let scale = play.pre_gain;
            if play.side.as_ref().map(|side| side.corr.len()) != Some(block) {
                play.side = build_side(&play.g, play.predelay_low, block);
            }
            if let Some(mut side) = play.side.take() {
                delay_block(&mut side.dry_hist, &mut side.dry_pos, &mut output[..block]);
                let mut corr = std::mem::take(&mut side.corr);
                let status = side.runtime.process_block_f32(
                    &input[..block],
                    block,
                    &mut corr,
                    block,
                    block,
                );
                if status.is_ok() {
                    for (sample, extra) in output[..block].iter_mut().zip(corr.iter()) {
                        *sample += *extra;
                    }
                }
                side.corr = corr;
                play.side = Some(side);
            }
            // 全部音箱乘同一个数。各路预衰减不同会改变相对声压。
            let mut start = 0;
            while start + block <= output.len() {
                for sample in &mut output[start..start + block] {
                    *sample *= scale;
                }
                start += out_stride;
            }
        } else {
            let coeffs = std::mem::take(&mut play.fir);
            let mut hist = std::mem::take(&mut play.fir_hist);
            let mut pos = play.fir_pos;
            fir_into(
                &mut hist,
                &mut pos,
                &coeffs,
                &input[..block],
                &mut output[..block],
                false,
            );
            play.fir = coeffs;
            play.fir_hist = hist;
            play.fir_pos = pos;
        }
    }

    fn open_umik(&self) -> Result<(), String> {
        #[cfg(test)]
        {
            self.umik_open.store(true, Ordering::Release);
            return Ok(());
        }
        #[cfg(all(not(test), target_os = "linux"))]
        {
            umik::open_session()?;
            self.umik_open.store(true, Ordering::Release);
            return Ok(());
        }
        #[cfg(all(not(test), not(target_os = "linux")))]
        {
            Err("UMIK 只在 Linux host 上打开".to_string())
        }
    }

    fn close_umik(&self) {
        #[cfg(all(not(test), target_os = "linux"))]
        umik::close_session();
        self.umik_open.store(false, Ordering::Release);
    }

    fn fail(&self, error: &str) -> String {
        let body = self.status_json(false, "refused");
        let escaped = error.replace('\\', "\\\\").replace('"', "\\\"");
        body.replacen("\"error\":\"refused\"", &format!("\"error\":\"{escaped}\""), 1)
    }

    fn status_json(&self, ok: bool, error: &str) -> String {
        let cal = self.mode.load(Ordering::Acquire) == MODE_CAL;
        let mode = if cal { "calibration" } else { "playback" };
        let source = if cal { "cal" } else { "program" };
        let sweep = match self.sweep.load(Ordering::Acquire) {
            SWEEP_LOW => "low",
            SWEEP_FULL => "full",
            _ => "off",
        };
        let pass = match self.pass.load(Ordering::Acquire) {
            PASS_LOW_IDENTITY => "low_identity",
            PASS_LOW_SIDE => "low_side",
            PASS_FULL_IDENTITY => "full_identity",
            PASS_FULL_FIR => "full_fir",
            _ => "idle",
        };
        let changed = cal && error.is_empty();
        let body = format!(
            "{{\"ok\":{ok},\"error\":\"{error}\",\"mode\":\"{mode}\",\"capture_source\":\"{source}\",\"capture_source_changed\":{changed},\"program_input_reaches_dsp\":{reaches},\"umik_open\":{umik},\"playback_restored\":{restored},\"sweep\":\"{sweep}\",\"pass\":\"{pass}\",\"samples_left\":{left},\"layout\":\"4.1.2\",\"channels\":[0,1,2,3,4,5,7],\"level_dbfs\":-12,\"iir_sections\":0,\"synthetic\":true,\"verdict\":\"PREDICTED_ONLY\",\"low_identity_blocks\":{low_i},\"low_side_blocks\":{low_s},\"full_identity_blocks\":{full_i},\"full_fir_blocks\":{full_f},\"side_path_open\":{side},\"post_identity\":{post}}}",
            ok = if ok { "true" } else { "false" },
            changed = if changed { "true" } else { "false" },
            reaches = if cal { "false" } else { "true" },
            umik = if self.umik_open.load(Ordering::Acquire) { "true" } else { "false" },
            restored = if cal { "false" } else { "true" },
            left = self.samples_left.load(Ordering::Acquire),
            low_i = self.low_identity_blocks.load(Ordering::Acquire),
            low_s = self.low_side_blocks.load(Ordering::Acquire),
            full_i = self.full_identity_blocks.load(Ordering::Acquire),
            full_f = self.full_fir_blocks.load(Ordering::Acquire),
            side = if self.side_path_open() { "true" } else { "false" },
            post = if self.post_is_identity() { "true" } else { "false" },
        );
        write_job_file("status.json", &body);
        body
    }

    fn solved_json(&self, solved: &Solved, synthetic: bool) -> String {
        let mut g = String::from("[");
        for (index, value) in solved.g_pp.iter().enumerate() {
            if index > 0 {
                g.push(',');
            }
            g.push_str(&format!("{value:.8}"));
        }
        g.push(']');
        let supports = if solved.supports.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                solved
                    .supports
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let body = format!(
            "{{\"ok\":true,\"error\":\"\",\"verdict\":\"PREDICTED_ONLY\",\"synthetic\":{synthetic},\"f_hi\":{f_hi},\"f_lo\":{f_lo:.1},\"supports\":{supports},\"metric\":\"{metric}\",\"engine_playable\":{playable},\"target\":\"{target}\",\"target_fir_taps\":{taps},\"iir_sections\":0,\"pre_gain_db\":{pre:.3},\"g_pp\":{g}}}",
            pre = solved.pre_gain_db,
            f_lo = solved.f_lo,
            synthetic = if synthetic { "true" } else { "false" },
            f_hi = solved.f_hi,
            metric = solved.metric,
            playable = if solved.engine_playable { "true" } else { "false" },
            target = solved.target,
            taps = solved.target_fir.len(),
        );
        write_job_file("summary.json", &body);
        body
    }
}

fn write_job_file(name: &str, body: &str) {
    let dir = std::env::var("TSPI_CAL_DIR").unwrap_or_else(|_| {
        if cfg!(unix) {
            "/run/tspi-rt-stack".to_string()
        } else {
            return String::new();
        }
    });
    if dir.is_empty() {
        return;
    }
    let path = std::path::Path::new(&dir).join(name);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(path, body);
    }
}

fn ir_from_pair(play: &[f64], mic: &[f64]) -> Vec<f64> {
    if play.len() < 8 || mic.len() < 8 {
        return Vec::new();
    }
    let count = play.len().min(mic.len());
    let ir = deconvolve(&mic[..count], &play[..count], IR_TAIL_FRAMES, 1e-5);
    window_from_peak(&ir, 48_000)
}

fn window_from_peak(ir: &[f64], keep: usize) -> Vec<f64> {
    if ir.is_empty() || keep == 0 {
        return Vec::new();
    }
    let peak = ir
        .iter()
        .enumerate()
        .max_by(|left, right| {
            left.1
                .abs()
                .partial_cmp(&right.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    let end = (peak + keep).min(ir.len());
    ir[peak..end].to_vec()
}

fn fir_into(
    hist: &mut [f32],
    pos: &mut usize,
    coeffs: &[f32],
    input: &[f32],
    output: &mut [f32],
    add: bool,
) {
    let taps = coeffs.len();
    if taps == 0 || hist.len() != taps {
        return;
    }
    for (index, &sample) in input.iter().enumerate() {
        hist[*pos] = sample;
        let mut acc = 0.0f32;
        let mut tap = *pos;
        for &coeff in coeffs {
            acc += coeff * hist[tap];
            tap = if tap == 0 { taps - 1 } else { tap - 1 };
        }
        *pos += 1;
        if *pos == taps {
            *pos = 0;
        }
        if add {
            output[index] += acc;
        } else {
            output[index] = acc;
        }
    }
}

/// 进程内唯一校准门。
pub fn shared() -> &'static CalControl {
    HUB.get_or_init(CalControl::new)
}

/// 音频线程调用。门还没装上时什么都不改。
pub fn overlay_live(input: &mut [f32], stride: usize, block: usize) -> bool {
    match HUB.get() {
        Some(control) => control.overlay(input, stride, block),
        None => false,
    }
}

/// 图跑完一块之后，把复测系数加到播放缓冲。播放模式和恒等档直接返回。
pub fn apply_live(
    input: &[f32],
    in_stride: usize,
    output: &mut [f32],
    out_stride: usize,
    block: usize,
) {
    if let Some(control) = HUB.get() {
        control.apply_output(input, in_stride, output, out_stride, block);
    }
}

/// 图跑完一块之后调用。不在复测档位时什么都不记。
pub fn note_chain_block() {
    if let Some(control) = HUB.get() {
        control.note_chain();
    }
}

/// 默认 `/run/tspi-rt-stack/host.sock`，可用 `TSPI_HOST_SOCK` 换路径。
pub fn socket_path() -> String {
    std::env::var("TSPI_HOST_SOCK").unwrap_or_else(|_| "/run/tspi-rt-stack/host.sock".to_string())
}

/// 在调用线程上绑定 socket，再把 accept 丢到后台。绑定失败不改变播放模式。
pub fn spawn_server() -> Result<(), String> {
    let control = shared();
    if SERVER_STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let listener = bind_listener(&socket_path())?;
        std::thread::Builder::new()
            .name("tspi-cal-sock".to_string())
            .spawn(move || accept_loop(listener, control))
            .map_err(|error| format!("校准 socket 线程启动失败：{error}"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = control;
        Ok(())
    }
}

/// 连上 host，写一行请求，读一行响应。
pub fn exchange(path: &str, request: &str) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        use std::time::Duration;
        let mut stream =
            UnixStream::connect(path).map_err(|error| format!("连接 {path} 失败：{error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(|error| format!("写入校准请求失败：{error}"))?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|error| format!("读取校准响应失败：{error}"))?;
        if line.is_empty() {
            return Err("校准响应为空".to_string());
        }
        Ok(line.trim().to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, request);
        Err("unix socket 只在 host 上可用".to_string())
    }
}

#[cfg(unix)]
fn bind_listener(path: &str) -> Result<std::os::unix::net::UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;
    let file = std::path::Path::new(path);
    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 socket 目录失败：{error}"))?;
        }
    }
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)
        .map_err(|error| format!("绑定 {path} 失败：{error}"))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660));
    Ok(listener)
}

#[cfg(unix)]
fn accept_loop(listener: std::os::unix::net::UnixListener, control: &'static CalControl) {
    use std::io::{Read, Write};
    for incoming in listener.incoming() {
        let mut stream = match incoming {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
        let mut buf = [0u8; 2048];
        let size = stream.read(&mut buf).unwrap_or(0);
        let line = std::str::from_utf8(&buf[..size]).unwrap_or("");
        let response = control.handle(line.trim());
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}

#[cfg(all(not(test), target_os = "linux"))]
#[path = "cal_umik.rs"]
mod umik;

fn json_string(line: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\":\"");
    let start = line.find(&pattern)? + pattern.len();
    let rest = line.get(start..)?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn json_bool(line: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{key}\":");
    let start = line.find(&pattern)? + pattern.len();
    let rest = line.get(start..)?.trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_f64(line: &str, key: &str) -> Option<f64> {
    let pattern = format!("\"{key}\":");
    let start = line.find(&pattern)? + pattern.len();
    let rest = line.get(start..)?.trim_start();
    let end = rest
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn json_string_array(line: &str, key: &str) -> Vec<String> {
    let pattern = format!("\"{key}\":[");
    let Some(start) = line.find(&pattern) else {
        return Vec::new();
    };
    let rest = &line[start + pattern.len()..];
    let Some(end) = rest.find(']') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .filter_map(|part| {
            let part = part.trim().trim_matches('"');
            if part.is_empty() {
                None
            } else {
                Some(part.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{window_from_peak, RESAMP_TAPS};
    use tspi_mimo_design::emperor::{
        decim_lowpass, decimate_48_to_12, design_channel, low_extension_hz, EmperorParams,
    };

    fn modal_ir() -> Vec<f64> {
        let fs = 12_000.0;
        let seconds = 0.4;
        let n = (fs * seconds) as usize;
        let mut y = vec![0.0; n];
        for (i, sample) in y.iter_mut().enumerate() {
            let t = i as f64 / fs;
            *sample = (-t / 0.04).exp() * (std::f64::consts::TAU * 63.0 * t).sin();
        }
        let mut state = 0x1234_5678u32;
        for (i, sample) in y.iter_mut().enumerate() {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (state >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
            if i as f64 > fs * (seconds - 0.25) {
                *sample = 1e-6 * noise;
            } else {
                *sample += 1e-9 * noise;
            }
        }
        y
    }

    #[test]
    fn enter_without_audible_keeps_playback_and_leaves_input() {
        let control = CalControl::new();
        let response = control.handle(r#"{"op":"enter","audible":false}"#);
        assert!(response.contains("\"ok\":false"), "{response}");
        assert!(
            response.contains("audible_authorization_required"),
            "{response}"
        );
        assert!(response.contains("\"mode\":\"playback\""), "{response}");
        assert!(
            response.contains("\"capture_source_changed\":false"),
            "{response}"
        );
        assert!(response.contains("\"umik_open\":false"), "{response}");
        assert!(!control.is_cal());
        let mut input = vec![0.25f32; 8];
        assert!(!control.overlay(&mut input, 4, 4));
        assert!(input.iter().all(|sample| *sample == 0.25));
    }

    #[test]
    fn enter_replaces_program_samples_and_leave_restores() {
        let control = CalControl::new();
        let response = control.handle(r#"{"op":"enter","audible":true}"#);
        assert!(response.contains("\"ok\":true"), "{response}");
        assert!(response.contains("\"capture_source\":\"cal\""), "{response}");
        assert!(
            response.contains("\"program_input_reaches_dsp\":false"),
            "{response}"
        );
        assert!(response.contains("\"umik_open\":true"), "{response}");
        assert!(
            response.contains("\"channels\":[0,1,2,3,4,5,7]"),
            "{response}"
        );
        let mut input = vec![0.5f32; 8];
        assert!(control.overlay(&mut input, 4, 4));
        assert!(input.iter().all(|sample| *sample == 0.0));
        let left = control.handle(r#"{"op":"leave"}"#);
        assert!(left.contains("\"mode\":\"playback\""), "{left}");
        assert!(left.contains("\"umik_open\":false"), "{left}");
        assert!(left.contains("\"playback_restored\":true"), "{left}");
        input.fill(0.5);
        assert!(!control.overlay(&mut input, 4, 4));
        assert!(input.iter().all(|sample| *sample == 0.5));
    }

    #[test]
    fn measure_without_audible_does_not_arm_a_sweep() {
        let control = CalControl::new();
        control.handle(r#"{"op":"enter","audible":true}"#);
        let held = control.handle(r#"{"op":"measure","audible":false}"#);
        assert!(held.contains("audible_authorization_required"), "{held}");
        assert!(held.contains("\"sweep\":\"off\""), "{held}");
    }

    #[test]
    fn low_sweep_is_written_on_the_virtual_input() {
        let control = CalControl::new();
        control.handle(r#"{"op":"enter","audible":true}"#);
        let armed = control.handle(r#"{"op":"measure","audible":true}"#);
        assert!(armed.contains("\"sweep\":\"low\""), "{armed}");
        let mut input = vec![1.0f32; 8];
        assert!(control.overlay(&mut input, 4, 4));
        assert!(input[0].abs() > 0.0 && input[0].abs() <= SWEEP_LEVEL + 1e-6);
        assert!(input[4..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn solve_without_a_recording_is_not_a_verdict() {
        let control = CalControl::new();
        let response = control.handle(r#"{"op":"solve"}"#);
        assert!(response.contains("\"ok\":false"), "{response}");
        assert!(response.contains("no_measurement"), "{response}");
        assert!(response.contains("PREDICTED_ONLY"), "{response}");
        assert!(response.contains("\"synthetic\":true"), "{response}");
        assert!(!response.contains("FIELD_SHORTER"), "{response}");
    }

    #[test]
    fn solve_exports_g_pp_and_target_fir_without_iir() {
        let control = CalControl::new();
        control.set_primary_ir(modal_ir());
        let response =
            control.handle(r#"{"op":"solve","f_hi":150,"supports":[],"target":"flat"}"#);
        assert!(response.contains("\"ok\":true"), "{response}");
        assert!(response.contains("\"g_pp\":["), "{response}");
        assert!(response.contains("\"target_fir_taps\":65"), "{response}");
        assert!(response.contains("\"iir_sections\":0"), "{response}");
        assert!(response.contains("\"verdict\":\"PREDICTED_ONLY\""), "{response}");
        assert!(response.contains("\"synthetic\":true"), "{response}");
        assert!(!response.contains("biquad"), "{response}");
        assert!(response.contains("\"supports\":[]"), "{response}");
        let above = control.handle(r#"{"op":"solve","f_hi":151}"#);
        assert!(above.contains("\"ok\":false"), "{above}");
        assert!(above.contains("150"), "{above}");
    }

    #[test]
    fn remeasure_counts_separate_passes() {
        let control = CalControl::new();
        control.handle(r#"{"op":"enter","audible":true}"#);
        control.set_primary_ir(modal_ir());
        control.handle(r#"{"op":"solve","target":"tilt10"}"#);
        let started = control.handle(r#"{"op":"remeasure","audible":true}"#);
        assert!(started.contains("\"pass\":\"low_identity\""), "{started}");
        assert!(started.contains("\"side_path_open\":false"), "{started}");
        control.note_chain();
        assert_eq!(control.low_identity_blocks.load(Ordering::Acquire), 1);
        assert_eq!(control.low_side_blocks.load(Ordering::Acquire), 0);
        control.advance_remeasure_pass();
        assert!(control.side_path_open());
        assert!(control.post_is_identity());
        control.note_chain();
        assert_eq!(control.low_side_blocks.load(Ordering::Acquire), 1);
        control.advance_remeasure_pass();
        assert!(!control.side_path_open());
        assert!(control.post_is_identity());
        control.note_chain();
        assert_eq!(control.full_identity_blocks.load(Ordering::Acquire), 1);
        assert_eq!(control.low_side_blocks.load(Ordering::Acquire), 1);
        control.advance_remeasure_pass();
        assert!(!control.post_is_identity());
        assert!(!control.side_path_open());
        control.note_chain();
        assert_eq!(control.full_fir_blocks.load(Ordering::Acquire), 1);
        let refused = control.handle(r#"{"op":"remeasure","audible":false}"#);
        assert!(
            refused.contains("audible_authorization_required"),
            "{refused}"
        );
    }

    #[test]
    fn low_and_full_sweeps_stay_in_separate_buffers() {
        let control = CalControl::new();
        control.handle(r#"{"op":"enter","audible":true}"#);
        control.handle(r#"{"op":"measure","audible":true}"#);
        control.push_mic(&[0.2, 0.0, 0.0, 0.0]);
        control.samples_left.store(4, Ordering::Release);
        control.sweep_total.store(4, Ordering::Release);
        let mut input = vec![0.0f32; 4];
        assert!(control.overlay(&mut input, 4, 4));
        assert_eq!(control.sweep.load(Ordering::Acquire), SWEEP_FULL);
        {
            let takes = control.takes.lock().expect("takes");
            assert_eq!(takes.low_play.len(), 4);
            assert_eq!(takes.low_mic.len(), 4);
            assert!(takes.full_play.is_empty());
            assert!(takes.rec_play.is_empty());
        }
        control.samples_left.store(4, Ordering::Release);
        control.sweep_total.store(4, Ordering::Release);
        assert!(control.overlay(&mut input, 4, 4));
        assert_eq!(control.sweep.load(Ordering::Acquire), SWEEP_OFF);
        let takes = control.takes.lock().expect("takes");
        assert_eq!(takes.low_play.len(), 4);
        assert_eq!(takes.full_play.len(), 4);
        assert_ne!(takes.low_play, takes.full_play);
        assert!(takes.full_mic.is_empty());
    }

    #[test]
    fn deconvolve_keeps_the_two_irs_apart() {
        let control = CalControl::new();
        let play = noise(512);
        let low_mic = delay_copy(&play, 8);
        let mut full_mic = delay_copy(&play[..128], 20);
        full_mic.iter_mut().for_each(|sample| *sample *= 0.25);
        control.submit_sweep_capture(false, play.clone(), low_mic);
        control.submit_sweep_capture(true, play[..128].to_vec(), full_mic);
        control.realize_recordings();
        let takes = control.takes.lock().expect("takes");
        assert_eq!(takes.low_play.len(), 512);
        assert_eq!(takes.full_play.len(), 128);
        assert!(!takes.low_ir.is_empty());
        assert!(!takes.full_ir.is_empty());
        assert_ne!(takes.low_ir, takes.full_ir);
        assert!(
            takes.low_ir[0].abs() > takes.full_ir[0].abs() * 2.0,
            "low {} full {}",
            takes.low_ir[0],
            takes.full_ir[0]
        );
    }

    #[test]
    fn side_path_and_target_fir_are_not_on_together() {
        let control = CalControl::new();
        control.install_correction(&[0.5], &[0.0, 1.0], 0.0, 0);
        let input = [1.0f32, 0.0, 0.0, 0.0];
        let mut output = [0.25f32; 4];
        control.pass.store(PASS_LOW_IDENTITY, Ordering::Release);
        control.apply_output(&input, 4, &mut output, 4, 4);
        assert!((output[0] - 0.25).abs() < 1e-5);

        output = [0.25; 4];
        control.pass.store(PASS_LOW_SIDE, Ordering::Release);
        control.apply_output(&input, 4, &mut output, 4, 4);
        assert!(
            (output[0] - 0.75).abs() > 1e-3,
            "12 kHz 侧路不能把系数按 48 kHz 逐点加上，得到 {}",
            output[0]
        );
        assert!(control.side_path_open());
        assert!(control.post_is_identity());

        output = [0.25; 4];
        control.pass.store(PASS_FULL_FIR, Ordering::Release);
        control.apply_output(&input, 4, &mut output, 4, 4);
        assert!(
            output[0].abs() < 1e-5,
            "target FIR replaces the block, got {}",
            output[0]
        );
        assert!((output[1] - 1.0).abs() < 1e-4, "{}", output[1]);
        assert!(!control.side_path_open());
        assert!(!control.post_is_identity());
    }

    #[test]
    fn low_side_pre_gain_scales_every_channel() {
        let control = CalControl::new();
        let block = 64;
        control.install_correction(&[2.0], &[0.0], -6.0, 0);
        let input = vec![0.0f32; block];
        let mut output = vec![0.25f32; block];
        output.extend(vec![0.40f32; block]);
        control.pass.store(PASS_LOW_SIDE, Ordering::Release);
        control.apply_output(&input, block, &mut output, block, block);
        let scale = 10f32.powf(-6.0 / 20.0);
        assert!(output[..block].iter().all(|sample| sample.abs() < 1e-4), "直通要先对齐延迟");
        assert!(
            output[block..].iter().all(|sample| (*sample - 0.40 * scale).abs() < 1e-4),
            "其余音箱乘同一个预衰减"
        );
        let stored = control.play.lock().expect("play");
        assert!((stored.g[0] - 2.0).abs() < 1e-6, "系数仍是原值，不是被削过的");
    }

    #[test]
    fn low_side_filters_at_12khz() {
        let block = 64;
        let control = CalControl::new();
        control.install_correction(&[1.0], &[0.0], 0.0, 0);
        control.pass.store(PASS_LOW_SIDE, Ordering::Release);
        let mut dc_tail = vec![0.0f32; block];
        for _ in 0..8 {
            let input = vec![1.0f32; block];
            let mut output = vec![0.0f32; block];
            control.apply_output(&input, block, &mut output, block, block);
            dc_tail = output;
        }
        let dc_mean = dc_tail.iter().sum::<f32>() / block as f32;
        assert!((dc_mean - 1.0).abs() < 0.05, "直流增益应是 1，得到 {dc_mean}");

        control.install_correction(&[1.0], &[0.0], 0.0, 0);
        let mut hf_tail = vec![0.0f32; block];
        for n in 0..8 {
            let input: Vec<f32> = (0..block)
                .map(|i| if (n * block + i) % 2 == 0 { 1.0 } else { -1.0 })
                .collect();
            let mut output = vec![0.0f32; block];
            control.apply_output(&input, block, &mut output, block, block);
            hf_tail = output;
        }
        let hf_mean = hf_tail.iter().map(|sample| sample.abs()).sum::<f32>() / block as f32;
        assert!(hf_mean < 0.05, "24 kHz 不应从 12 kHz 侧路漏出来，得到 {hf_mean}");
    }

    /// 全场每条测量都走真正的复测侧路。控制频段尾巴要比直通短。文件不在时跳过。
    #[test]
    fn played_rew_side_path_shortens_the_control_band_tail() {
        let names = [
            "L-full",
            "R-full",
            "SL-full",
            "SR-full",
            "HFL-full-24",
            "HFR-full-24",
            "LFE-full",
            "L-b",
            "R-b",
            "SL-b",
            "SR-b",
            "HFL-b-24",
            "HFR-b-24",
            "LFE-b",
        ];
        let mut saw = 0;
        let mut failures = Vec::new();
        for name in names {
            let path = format!(r"C:\WorkSpace\Claw\scratch\lead\rew-room-20261123\{name}.f32");
            let bytes = std::fs::read(&path).unwrap_or_default();
            if bytes.len() < 64 {
                continue;
            }
            saw += 1;
            match played_tail_db(&bytes) {
                Ok((played_db, predicted_db, taps)) => {
                    println!("{name} played {played_db:.2} dB predicted {predicted_db:.2} dB taps={taps}");
                    if played_db >= -2.0 {
                        failures.push(format!(
                            "{name} 复测侧路只短了 {played_db:.2} dB，预测 {predicted_db:.2} dB"
                        ));
                    }
                }
                Err(err) => failures.push(format!("{name}: {err}")),
            }
        }
        if saw == 0 {
            return;
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    fn played_tail_db(bytes: &[u8]) -> Result<(f64, f64, usize), String> {
        let recorded: Vec<f64> = bytes
            .chunks_exact(4)
            .map(|chunk| f64::from(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])))
            .collect();
        let ir = window_from_peak(&recorded, recorded.len());
        let mut used = EmperorParams::default();
        if let Some(hz) = low_extension_hz(&ir, 48_000.0) {
            if hz < used.f_hi {
                used.f_lo = hz;
            }
        }
        let decimated = decimate_48_to_12(&ir, RESAMP_TAPS)?;
        let primary = window_from_peak(&decimated, 24_000);
        let designed = design_channel(&primary, &[], &used)?;
        let block = 128usize;
        let control = CalControl::new();
        control.install_correction(&designed.g_primary, &[0.0], 0.0, used.d12);
        control.pass.store(PASS_LOW_SIDE, Ordering::Release);
        let mut rendered = Vec::with_capacity(ir.len());
        let mut offset = 0;
        while offset + block <= ir.len() {
            let input: Vec<f32> = ir[offset..offset + block].iter().map(|value| *value as f32).collect();
            let mut output = input.clone();
            control.apply_output(&input, block, &mut output, block, block);
            rendered.extend_from_slice(&output);
            offset += block;
        }
        let stored = control.play.lock().map_err(|_| "播放状态锁失败".to_string())?;
        if stored.side.is_none() {
            return Err(format!("侧路没有装上，抽头 {}", designed.taps));
        }
        drop(stored);
        let delay = (RESAMP_TAPS - 1) + 4 * used.d12;
        let mut dry = vec![0.0f32; delay];
        dry.extend(ir[..rendered.len().saturating_sub(delay)].iter().map(|value| *value as f32));
        dry.truncate(rendered.len());
        let window_s = designed.tail_s.min(0.5);
        let (dry_peak, dry_energy) = control_band_tail_energy(&dry, used.f_lo, window_s, None);
        let (_, played_energy) = control_band_tail_energy(&rendered, used.f_lo, window_s, Some(dry_peak));
        let ratio = played_energy / dry_energy.max(1e-30);
        let played_db = 10.0 * ratio.max(1e-20).log10();
        let predicted_db = 10.0 * designed.predicted_tail_ratio.max(1e-20).log10();
        Ok((played_db, predicted_db, designed.taps))
    }

    #[test]
    fn r_branch_keeps_the_12k_tail_cut() {
        let bytes = std::fs::read(r"C:\WorkSpace\Claw\scratch\lead\rew-room-20261123\R-full.f32")
            .unwrap_or_default();
        if bytes.len() < 64 {
            return;
        }
        let ir: Vec<f64> = bytes
            .chunks_exact(4)
            .map(|chunk| f64::from(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])))
            .collect();
        let mut used = EmperorParams::default();
        if let Some(hz) = low_extension_hz(&ir, 48_000.0) {
            if hz < used.f_hi {
                used.f_lo = hz;
            }
        }
        let decimated = decimate_48_to_12(&ir, RESAMP_TAPS).unwrap();
        let primary = window_from_peak(&decimated, 24_000);
        let designed = design_channel(&primary, &[], &used).unwrap();
        let signal = upsample_12_to_48(&primary);
        let block = 128usize;
        let control = CalControl::new();
        control.install_correction(&designed.g_primary, &[0.0], 0.0, used.d12);
        control.pass.store(PASS_LOW_SIDE, Ordering::Release);
        let mut rendered = Vec::new();
        let mut offset = 0;
        while offset + block <= signal.len() {
            let input = signal[offset..offset + block].to_vec();
            let mut output = input.clone();
            control.apply_output(&input, block, &mut output, block, block);
            rendered.extend_from_slice(&output);
            offset += block;
        }
        let delay = (RESAMP_TAPS - 1) + 4 * used.d12;
        let mut dry = vec![0.0f32; delay];
        dry.extend(signal[..rendered.len().saturating_sub(delay)].iter().copied());
        dry.truncate(rendered.len());
        let (peak, dry_energy) = control_band_tail_energy(&dry, used.f_lo, 0.5, None);
        let (_, played_energy) = control_band_tail_energy(&rendered, used.f_lo, 0.5, Some(peak));
        let db = 10.0 * (played_energy / dry_energy.max(1e-30)).max(1e-20).log10();
        println!("R upsampled played {db:.2} dB taps={}", designed.taps);
        assert!(db < -6.0, "右声道插回 48 kHz 后控制频段只剩 {db:.2} dB");
    }

    fn upsample_12_to_48(samples: &[f64]) -> Vec<f32> {
        let kernel = decim_lowpass(RESAMP_TAPS).expect("decimator");
        let mut stuffed = vec![0.0f64; samples.len() * 4];
        for (index, value) in samples.iter().enumerate() {
            stuffed[index * 4] = *value;
        }
        let mut up = vec![0.0f32; stuffed.len()];
        for n in 0..stuffed.len() {
            let mut acc = 0.0;
            for (k, coeff) in kernel.iter().enumerate() {
                if n >= k {
                    acc += stuffed[n - k] * coeff * 4.0;
                }
            }
            up[n] = acc as f32;
        }
        up
    }

    fn bass_peak(y: &[f64]) -> usize {
        let taps = 63usize;
        let cutoff = 200.0 / 12_000.0;
        let mid = (taps - 1) as f64 / 2.0;
        let mut kernel = vec![0.0; taps];
        let mut sum = 0.0;
        for (k, slot) in kernel.iter_mut().enumerate() {
            let t = k as f64 - mid;
            let x = std::f64::consts::TAU * cutoff * t;
            let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
            let angle = std::f64::consts::TAU * k as f64 / (taps - 1) as f64;
            *slot = sinc * (0.42 - 0.5 * angle.cos() + 0.08 * (2.0 * angle).cos());
            sum += *slot;
        }
        for value in &mut kernel {
            *value /= sum;
        }
        let delay = (taps - 1) / 2;
        let mut best = 0usize;
        let mut best_abs = 0.0;
        for n in delay..y.len() {
            let mut acc = 0.0;
            for (k, coeff) in kernel.iter().enumerate() {
                if n >= k {
                    acc += y[n - k] * coeff;
                }
            }
            let abs = acc.abs();
            if abs > best_abs {
                best_abs = abs;
                best = n - delay;
            }
        }
        best
    }

    fn control_band_tail_energy(
        x: &[f32],
        f_lo: f64,
        window_s: f64,
        peak_at: Option<usize>,
    ) -> (usize, f64) {
        let samples: Vec<f64> = x.iter().map(|value| f64::from(*value)).collect();
        let y = decimate_48_to_12(&samples, RESAMP_TAPS).unwrap_or_default();
        if y.len() < 512 {
            return (0, 0.0);
        }
        let peak = peak_at.unwrap_or_else(|| bass_peak(&y));
        let start = peak + 240;
        let end = (start + (window_s * 12_000.0) as usize).min(y.len());
        if start + 64 >= end {
            return (peak, 0.0);
        }
        let tail = &y[start..end];
        let n = tail.len();
        let k_lo = ((f_lo.max(0.0) * n as f64) / 12_000.0).ceil() as usize;
        let k_hi = ((150.0 * n as f64) / 12_000.0).floor() as usize;
        let mut energy = 0.0;
        for k in k_lo..=k_hi.min(n / 2) {
            let mut re = 0.0;
            let mut im = 0.0;
            for (index, value) in tail.iter().enumerate() {
                let angle = std::f64::consts::TAU * (k * index) as f64 / n as f64;
                re += value * angle.cos();
                im -= value * angle.sin();
            }
            energy += re * re + im * im;
        }
        (peak, energy)
    }

    fn delay_copy(play: &[f64], delay: usize) -> Vec<f64> {
        let mut mic = vec![0.0; play.len()];
        if delay < play.len() {
            mic[delay..].copy_from_slice(&play[..play.len() - delay]);
        }
        mic
    }

    fn noise(count: usize) -> Vec<f64> {
        let mut state = 0x1234_5678u32;
        (0..count)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                f64::from(state >> 8) / f64::from(1u32 << 24) - 0.5
            })
            .collect()
    }
}
