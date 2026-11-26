//! 通道识别测试音（SPEAKER-MAPPING P3.5 / API v1.2 §12.2）：控制面 test_tone / test_tone_stop。
//!
//! 复用 host 已有的校准播放 seam（Runtime::begin_calibration_playback / run_calibration_block /
//! CalibrationPlayback / CalibrationProgress），**不新增数据面路径、不改 crates/host**：
//! 控制线程把整段测试音预渲染成交错 f32，经 ControlCommand::CalPlaybackBegin 交给块线程；
//! 块线程在块边界登记后由 run_calibration_block 逐块写出（只有目标 DAC 非零，其余通道静音）。
//! 播完（progress.finished）看门狗自动发 CalPlaybackEnd，恢复节目输出；stop 幂等。
//!
//! 约束（契约 §12.2）：
//! - audible:true + confirm:true 必须显式给出（测试音会出声，不默认）；ms 500..=10000；
//!   db -60..=-12（默认 -30）；kind noise|sine（sine 可带 freq_hz；noise 的 freq_hz 忽略）；
//! - 只有目标 DAC 有信号；20 ms 线性淡入淡出；硬上限 10 s；
//! - 忙（校准模式/热重配在飞/已有测试音）-> 稳定码 busy（daemon 映射 409）。
//!
//! NOT_RUN：真机 ALSA 出声（本机 Windows 只编译桩，实际声音由 N150 上的任务验证）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use khaen_host::control::{error, ControlError};
use khaen_host::engine::EngineConfig;
use khaen_host::runtime::{CalibrationPlayback, CalibrationProgress};
use serde_json::{json, Map, Value};

use super::command::ControlCommand;

/// 测试音的两个 op（status.wired 上报）。
pub const TEST_TONE_OPS: &[&str] = &["test_tone", "test_tone_stop"];
/// 忙（daemon 映射 HTTP 409）。
pub const BUSY: &str = "busy";
/// 测试音最短时长（毫秒）。
pub const MIN_MS: u64 = 500;
/// 测试音最长时长（毫秒；硬上限 10 s，契约与出声纪律一致）。
pub const MAX_MS: u64 = 10_000;
/// 默认电平（dBFS）。
pub const DEFAULT_DB: f32 = -30.0;
/// 最轻电平（dBFS）。
pub const MIN_DB: f32 = -60.0;
/// 最响电平（dBFS）。
pub const MAX_DB: f32 = -12.0;
/// 淡入/淡出长度（毫秒）。
pub const FADE_MS: u32 = 20;
/// 正弦测试音的最低频率（Hz）。
pub const MIN_SINE_HZ: f64 = 100.0;
/// 正弦测试音的最高频率（Hz）。
pub const MAX_SINE_HZ: f64 = 10_000.0;
/// 正弦测试音默认频率（Hz）。
pub const DEFAULT_SINE_HZ: f64 = 1_000.0;
/// 噪声测试音的带通下限（Hz）。
pub const NOISE_BAND_LOW_HZ: f64 = 500.0;
/// 噪声测试音的带通上限（Hz）。
pub const NOISE_BAND_HIGH_HZ: f64 = 4_000.0;
/// 看门狗兜底时限在测试音时长之外多给的余量（毫秒）。
const WATCHDOG_MARGIN_MS: u64 = 1_000;

/// 测试音种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestToneKind {
    /// 500 Hz - 4 kHz 带限噪声（覆盖多数通道识别场景，不偏袒某个单频）。
    Noise,
    /// 单频正弦（默认 1 kHz；可带 freq_hz）。
    Sine,
}

impl TestToneKind {
    /// 稳定字符串（响应/状态里用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Noise => "noise",
            Self::Sine => "sine",
        }
    }
}

/// 一条已校验的测试音请求。
#[derive(Debug, Clone, PartialEq)]
pub struct TestToneRequest {
    /// 目标 DAC 序号（0-based；必须小于当前数据面通道数）。
    pub dac: usize,
    /// 时长（毫秒）。
    pub ms: u64,
    /// 种类。
    pub kind: TestToneKind,
    /// 正弦频率（只有 kind=sine 才有）。
    pub freq_hz: Option<f64>,
    /// 电平（dBFS）。
    pub level_db: f32,
}

fn bad_request(message: impl Into<String>) -> ControlError {
    ControlError::new(error::BAD_REQUEST, message)
}

/// audible / confirm 必须显式 true：测试音会出声，不允许默认同意。
fn require_true(object: &Map<String, Value>, name: &str) -> Result<(), ControlError> {
    match object.get(name) {
        Some(Value::Bool(true)) => Ok(()),
        Some(Value::Bool(false)) => Err(bad_request(format!(
            "field {name} must be true: a speaker test tone is audible, so the request must acknowledge it explicitly"
        ))),
        Some(_) => Err(bad_request(format!("field {name} must be a boolean"))),
        None => Err(bad_request(format!(
            "missing field: {name} (a speaker test tone requires {name}:true)"
        ))),
    }
}

fn integer_field(object: &Map<String, Value>, name: &str) -> Result<u64, ControlError> {
    match object.get(name) {
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| bad_request(format!("field {name} must be a non-negative integer"))),
        Some(_) => Err(bad_request(format!(
            "field {name} must be a non-negative integer"
        ))),
        None => Err(bad_request(format!("missing field: {name}"))),
    }
}

fn number_field(object: &Map<String, Value>, name: &str) -> Result<Option<f64>, ControlError> {
    match object.get(name) {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Some)
            .ok_or_else(|| bad_request(format!("field {name} must be a finite number"))),
        Some(_) => Err(bad_request(format!("field {name} must be a number"))),
        None => Ok(None),
    }
}

impl TestToneRequest {
    /// 解析并校验请求（严格：未知字段不看，但已知字段类型/范围不对就 bad_request）。
    pub fn parse(object: &Map<String, Value>, format: &EngineConfig) -> Result<Self, ControlError> {
        require_true(object, "audible")?;
        require_true(object, "confirm")?;
        let channels = format.channels.max(1) as u64;
        let dac = integer_field(object, "dac")?;
        if dac >= channels {
            return Err(bad_request(format!(
                "field dac must be in 0..={} (the running data-plane format has {channels} channels)",
                channels - 1
            )));
        }
        let ms = integer_field(object, "ms")?;
        if !(MIN_MS..=MAX_MS).contains(&ms) {
            return Err(bad_request(format!(
                "field ms must be in {MIN_MS}..={MAX_MS} (speaker test limits: at most 10 s of continuous sound)"
            )));
        }
        let kind = match object.get("kind") {
            Some(Value::String(name)) => match name.as_str() {
                "noise" => TestToneKind::Noise,
                "sine" => TestToneKind::Sine,
                other => {
                    return Err(bad_request(format!(
                        "field kind must be noise or sine (got {other})"
                    )))
                }
            },
            Some(_) => return Err(bad_request("field kind must be a string")),
            None => return Err(bad_request("missing field: kind")),
        };
        let level_db = match number_field(object, "db")? {
            Some(value) => value as f32,
            None => DEFAULT_DB,
        };
        if !(MIN_DB..=MAX_DB).contains(&level_db) {
            return Err(bad_request(format!(
                "field db must be in {MIN_DB}..={MAX_DB} (dBFS)"
            )));
        }
        let freq_hz = number_field(object, "freq_hz")?;
        let freq_hz = match kind {
            // noise 是固定的 500 Hz - 4 kHz 带限噪声：freq_hz 对它无意义，宽容忽略
            // （daemon/UI 可能统一带默认 freq_hz，不能因此 400）。
            TestToneKind::Noise => None,
            TestToneKind::Sine => match freq_hz {
                Some(value) => {
                    if !(MIN_SINE_HZ..=MAX_SINE_HZ).contains(&value) {
                        return Err(bad_request(format!(
                            "field freq_hz must be in {MIN_SINE_HZ}..={MAX_SINE_HZ} Hz"
                        )));
                    }
                    Some(value)
                }
                None => Some(DEFAULT_SINE_HZ),
            },
        };
        Ok(Self {
            dac: dac as usize,
            ms,
            kind,
            freq_hz,
            level_db,
        })
    }
}

/// 预渲染好的采样缓冲（纯函数，便于离线校验淡入淡出/只有目标 DAC 有信号）。
pub struct RenderedToneBuffer {
    /// 帧交错 f32（长度 = total_blocks x block_frames x channels）。
    pub data: Vec<f32>,
    /// 总块数。
    pub total_blocks: u64,
    /// 有效采样帧数（其后的整块填充是静音）。
    /// 仅测试读取：bin 目标不读它，`--all-targets` 的 clippy 会对 bin 目标报 dead_code。
    #[allow(dead_code)]
    pub total_samples: u64,
}

/// 一段可直接交给 host 播放 seam 的测试音。
pub struct RenderedTone {
    /// 播放数据（交错 f32 + 进度句柄）。
    pub playback: CalibrationPlayback,
    /// 进度共享句柄（总块数/剩余进度从 progress 读）。
    pub progress: Arc<CalibrationProgress>,
}

/// 双二阶滤波器系数（RBJ 低通/高通，Q=1/sqrt(2) 即 Butterworth 响应）。
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// 双二阶滤波器状态（直接 I 型；控制线程一次性渲染，无需 SIMD 优化）。
#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn lowpass(sample_rate: f64, cutoff_hz: f64, q: f64) -> Self {
        let w0 = std::f64::consts::TAU * cutoff_hz / sample_rate;
        let cos = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cos) / 2.0) / a0,
            b1: (1.0 - cos) / a0,
            b2: ((1.0 - cos) / 2.0) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn highpass(sample_rate: f64, cutoff_hz: f64, q: f64) -> Self {
        let w0 = std::f64::consts::TAU * cutoff_hz / sample_rate;
        let cos = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos) / 2.0) / a0,
            b1: -(1.0 + cos) / a0,
            b2: ((1.0 + cos) / 2.0) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn process(&self, state: &mut BiquadState, input: f64) -> f64 {
        let output = self.b0 * input + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;
        state.x2 = state.x1;
        state.x1 = input;
        state.y2 = state.y1;
        state.y1 = output;
        output
    }
}

/// 小幅线性同余白噪声（确定性，便于测试与复现；非加密用途）。
struct WhiteNoise {
    state: u32,
}

impl WhiteNoise {
    fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        (f64::from(self.state) / f64::from(u32::MAX)) * 2.0 - 1.0
    }
}

/// 把请求渲染成交错 f32（只有目标 DAC 非零；20 ms 线性淡入淡出；块尾静音填充）。
pub fn render_buffer(
    format: &EngineConfig,
    request: &TestToneRequest,
) -> Result<RenderedToneBuffer, ControlError> {
    let sample_rate = u64::from(format.sample_rate);
    let block_frames = format.block_frames as u64;
    let channels = format.channels as u64;
    if sample_rate == 0 || block_frames == 0 || channels == 0 {
        return Err(ControlError::new(
            error::NOT_APPLIED,
            "the running data-plane format has a zero sample_rate/block_frames/channels; a speaker test tone cannot be rendered",
        ));
    }
    let total_samples = request.ms * sample_rate / 1000;
    if total_samples == 0 {
        return Err(bad_request(
            "the requested test tone is shorter than one sample",
        ));
    }
    let total_blocks = total_samples.div_ceil(block_frames);
    let padded_samples = total_blocks
        .checked_mul(block_frames)
        .ok_or_else(|| bad_request("the requested test tone buffer is too large"))?;
    let total_len = usize::try_from(
        padded_samples
            .checked_mul(channels)
            .ok_or_else(|| bad_request("the requested test tone buffer is too large"))?,
    )
    .map_err(|_| bad_request("the requested test tone buffer is too large"))?;
    let fade_samples = (sample_rate * u64::from(FADE_MS) / 1000).max(1);
    let fade_samples = fade_samples.min(total_samples);
    let amplitude = 10.0_f64.powf(f64::from(request.level_db) / 20.0);
    let mut data = vec![0.0_f32; total_len];
    let dac = request.dac as u64;
    if dac >= channels {
        return Err(bad_request(format!(
            "field dac {dac} is outside the running data-plane format ({channels} channels)"
        )));
    }
    let mut noise = WhiteNoise::new(0x1234_5678);
    // 4 阶 Butterworth 带通（高通 500 Hz + 低通 4 kHz，各 12 dB/oct 裙边）：
    // 一阶/低 Q 二阶滤波的阻带衰减不足，近 Nyquist 的残余会主导过零率（听起来是"嘶"）。
    let highpass = Biquad::highpass(
        sample_rate as f64,
        NOISE_BAND_LOW_HZ,
        std::f64::consts::FRAC_1_SQRT_2,
    );
    let lowpass = Biquad::lowpass(
        sample_rate as f64,
        NOISE_BAND_HIGH_HZ,
        std::f64::consts::FRAC_1_SQRT_2,
    );
    let mut highpass_state = BiquadState::default();
    let mut lowpass_state = BiquadState::default();
    let sine_hz = request.freq_hz.unwrap_or(DEFAULT_SINE_HZ);
    for frame in 0..total_samples {
        let envelope = if frame < fade_samples {
            frame as f64 / fade_samples as f64
        } else if frame >= total_samples - fade_samples {
            (total_samples - frame) as f64 / fade_samples as f64
        } else {
            1.0
        };
        let signal = match request.kind {
            TestToneKind::Sine => {
                (std::f64::consts::TAU * sine_hz * frame as f64 / sample_rate as f64).sin()
            }
            TestToneKind::Noise => {
                let high = highpass.process(&mut highpass_state, noise.next());
                let band = lowpass.process(&mut lowpass_state, high);
                // 带通输出是窄带白噪声（RMS 约 0.16）：按 3 倍补偿后由下面的 clamp 兜底。
                (band * 3.0).clamp(-1.0, 1.0)
            }
        };
        let value = (signal * amplitude * envelope).clamp(-1.0, 1.0);
        let index = usize::try_from(frame * channels + dac)
            .map_err(|_| bad_request("the requested test tone buffer is too large"))?;
        data[index] = value as f32;
    }
    Ok(RenderedToneBuffer {
        data,
        total_blocks,
        total_samples,
    })
}

/// 渲染一段可直接交给 host 播放 seam 的测试音（进度句柄与数据一起构造）。
pub fn render(
    format: &EngineConfig,
    request: &TestToneRequest,
) -> Result<RenderedTone, ControlError> {
    let buffer = render_buffer(format, request)?;
    let progress = Arc::new(CalibrationProgress::new(buffer.total_blocks));
    let playback = CalibrationPlayback::new(
        buffer.data,
        format.block_frames,
        format.channels,
        Arc::clone(&progress),
    )
    .map_err(|err| {
        ControlError::new(
            error::NOT_APPLIED,
            format!("speaker test tone buffer was rejected by the host playback seam: {err}"),
        )
    })?;
    Ok(RenderedTone { playback, progress })
}

/// 正在播放的测试音（控制面会话状态；块线程只拥有 playback/progress）。
#[derive(Debug)]
struct ActiveTestTone {
    /// 会话序号（看门狗只清理自己那次会话，避免误清新会话）。
    id: u64,
    dac: usize,
    kind: TestToneKind,
    total_blocks: u64,
    block_frames: u64,
    sample_rate: u32,
    progress: Arc<CalibrationProgress>,
    cancelled: Arc<AtomicBool>,
}

/// 测试音会话槽（ControlCore 持有；socket status 读它，不需要块线程参与）。
#[derive(Debug, Default)]
pub struct TestToneSlot {
    inner: Mutex<Option<ActiveTestTone>>,
    next_id: AtomicU64,
}

impl TestToneSlot {
    /// 新建共享槽。
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Option<ActiveTestTone>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 是否有测试音在播（或刚注册、块线程尚未写出）。
    pub fn active(&self) -> bool {
        self.lock().is_some()
    }

    /// socket status 的 speaker_test 字段（加法字段；没有测试音时给 false + null）。
    pub fn status_json(&self) -> Value {
        match &*self.lock() {
            None => json!({
                "active": false,
                "dac": Value::Null,
                "remaining_ms": 0,
                "kind": Value::Null,
            }),
            Some(tone) => {
                let done = tone.progress.blocks_done().min(tone.total_blocks);
                let finished = tone.progress.finished();
                let remaining_ms = if finished || done >= tone.total_blocks {
                    0
                } else {
                    (tone.total_blocks - done)
                        .saturating_mul(tone.block_frames)
                        .saturating_mul(1000)
                        / u64::from(tone.sample_rate.max(1))
                };
                json!({
                    "active": !finished,
                    "dac": tone.dac,
                    "remaining_ms": remaining_ms,
                    "kind": tone.kind.as_str(),
                })
            }
        }
    }

    /// 当前测试音的进度句柄（测试用：离线校验 remaining_ms 计算）。
    #[cfg(test)]
    pub(crate) fn progress_handle(&self) -> Option<Arc<CalibrationProgress>> {
        self.lock().as_ref().map(|tone| Arc::clone(&tone.progress))
    }

    pub(crate) fn install(
        &self,
        request: &TestToneRequest,
        progress: Arc<CalibrationProgress>,
        total_blocks: u64,
        block_frames: u64,
        sample_rate: u32,
    ) -> (u64, Arc<CalibrationProgress>, Arc<AtomicBool>) {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel) + 1;
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut guard = self.lock();
        *guard = Some(ActiveTestTone {
            id,
            dac: request.dac,
            kind: request.kind,
            total_blocks,
            block_frames,
            sample_rate,
            progress: Arc::clone(&progress),
            cancelled: Arc::clone(&cancelled),
        });
        (id, progress, cancelled)
    }

    /// 结束当前测试音（stop 或看门狗）：标记取消并清槽，返回此前是否有测试音。
    pub fn cancel_active(&self) -> bool {
        let tone = self.lock().take();
        match tone {
            Some(tone) => {
                tone.cancelled.store(true, Ordering::Release);
                true
            }
            None => false,
        }
    }

    /// 只清理指定会话（看门狗用；不影响之后新建的会话）。
    fn finish(&self, id: u64) {
        let mut guard = self.lock();
        if guard.as_ref().is_some_and(|tone| tone.id == id) {
            *guard = None;
        }
    }
}

/// 看门狗：测试音播完（progress.finished）或兜底时限到，自动发 CalPlaybackEnd 恢复节目输出。
///
/// 必须在控制线程调用（不是 RT 路径）：这里会分配/睡眠/等 ack；块线程只负责逐块写出。
pub fn spawn_auto_stop(
    slot: Arc<TestToneSlot>,
    id: u64,
    progress: Arc<CalibrationProgress>,
    cancelled: Arc<AtomicBool>,
    commands: Sender<ControlCommand>,
    timeout: Duration,
) {
    let spawned = std::thread::Builder::new()
        .name("khaen-test-tone".to_string())
        .spawn(move || {
            let deadline = Instant::now() + timeout;
            while !cancelled.load(Ordering::Acquire) {
                if progress.finished() || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            // 自动收尾：块线程回到节目路径；ack 超时不影响清槽（块线程可能已停）。
            let (ack, replies) = std::sync::mpsc::channel();
            if commands
                .send(ControlCommand::CalPlaybackEnd { ack })
                .is_ok()
            {
                let _ = replies.recv_timeout(Duration::from_millis(500));
            }
            slot.finish(id);
        });
    if let Err(err) = spawned {
        eprintln!("khaen-rt-host: cannot spawn speaker-test auto-stop thread: {err}");
    }
}

/// 看门狗兜底时限：测试音时长 + 1 s（progress 卡住时也能恢复节目输出）。
pub fn auto_stop_timeout(ms: u64) -> Duration {
    Duration::from_millis(ms.saturating_add(WATCHDOG_MARGIN_MS))
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc::{self, Receiver};
    use std::thread::JoinHandle;

    use khaen_host::admission::AdmissionLimits;
    use khaen_host::control::RuntimeMode;

    use crate::control::command::CommandReply;
    use crate::control::core::{ControlCore, ControlCoreConfig};
    use crate::control::snapshot::SharedSnapshot;

    fn format(channels: usize) -> EngineConfig {
        EngineConfig {
            block_frames: 128,
            sample_rate: 48_000,
            channels,
        }
    }

    fn request(kind: TestToneKind, dac: usize, ms: u64) -> TestToneRequest {
        TestToneRequest {
            dac,
            ms,
            kind,
            freq_hz: match kind {
                TestToneKind::Sine => Some(DEFAULT_SINE_HZ),
                TestToneKind::Noise => None,
            },
            level_db: DEFAULT_DB,
        }
    }

    /// 单个频点的 DFT 幅值（矩形窗；测试用 0.5 s 整数周期频点，泄漏可忽略）。
    fn bin_power(samples: &[f32], sample_rate: u32, freq_hz: f64) -> f64 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (index, value) in samples.iter().enumerate() {
            let phase = std::f64::consts::TAU * freq_hz * index as f64 / f64::from(sample_rate);
            real += f64::from(*value) * phase.cos();
            imaginary -= f64::from(*value) * phase.sin();
        }
        (real * real + imaginary * imaginary).sqrt() / samples.len() as f64
    }

    fn parse(value: &str) -> Value {
        serde_json::from_str(value).expect("json response")
    }

    fn op(core: &mut ControlCore, value: Value) -> Value {
        parse(&core.handle_line(&value.to_string()))
    }

    fn tone_request(kind: &str, dac: usize, ms: u64) -> Value {
        json!({
            "op": "test_tone",
            "dac": dac,
            "ms": ms,
            "kind": kind,
            "audible": true,
            "confirm": true,
        })
    }

    /// 假块线程：回执 begin/end，只记录看到了什么（不跑真实播放）。
    fn fake_block_thread(
        rx: Receiver<ControlCommand>,
    ) -> (Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = Arc::clone(&seen);
        let handle = std::thread::spawn(move || {
            let mut active = false;
            while let Ok(command) = rx.recv() {
                match command {
                    ControlCommand::CalPlaybackBegin { playback, ack } => {
                        active = true;
                        let total_blocks = playback.total_blocks();
                        seen_thread
                            .lock()
                            .expect("seen lock")
                            .push(format!("begin:{total_blocks}"));
                        let _ = ack.send(CommandReply::CalPlaybackStarted { total_blocks });
                        drop(playback);
                    }
                    ControlCommand::CalPlaybackEnd { ack } => {
                        seen_thread
                            .lock()
                            .expect("seen lock")
                            .push("end".to_string());
                        let was_active = active;
                        active = false;
                        let _ = ack.send(CommandReply::CalPlaybackEnded { was_active });
                    }
                    ControlCommand::Mute { mute, ack } => {
                        seen_thread
                            .lock()
                            .expect("seen lock")
                            .push(format!("mute:{mute}"));
                        let _ = ack.send(CommandReply::Mute {
                            mute,
                            applied_block: 1,
                            idempotent: false,
                        });
                    }
                    _ => {}
                }
            }
        });
        (seen, handle)
    }

    fn core_with(channels: usize, commands: Sender<ControlCommand>) -> ControlCore {
        let snapshot: SharedSnapshot = Arc::new(Mutex::new(None));
        ControlCore::new(ControlCoreConfig {
            data_plane_format: format(channels),
            limits: AdmissionLimits::default(),
            commands,
            snapshot,
            mode: RuntimeMode::Aes67,
            initial_plan: None,
            initial_version: 0,
            command_timeout: Duration::from_millis(500),
            calibration: None,
            channels: None,
            reconfigure: None,
        })
    }

    #[test]
    fn sine_tone_only_writes_the_target_dac_with_linear_fades() {
        let format = format(2);
        let request = TestToneRequest {
            dac: 1,
            ms: 1_000,
            kind: TestToneKind::Sine,
            freq_hz: Some(1_000.0),
            level_db: -20.0,
        };
        let buffer = render_buffer(&format, &request).expect("sine buffer");
        assert_eq!(buffer.total_samples, 48_000);
        assert_eq!(buffer.total_blocks, 375);
        assert_eq!(buffer.data.len(), 375 * 128 * 2);
        // 非目标 DAC 全部为 0（不扰动其他通道）。
        for frame in 0..buffer.total_samples as usize {
            assert_eq!(buffer.data[frame * 2], 0.0, "left channel must stay silent");
        }
        // 第一帧正是淡入起点（0），带内稳态峰值约等于请求电平（-20 dBFS = 0.1）。
        assert_eq!(buffer.data[1], 0.0);
        let amplitude = 10.0_f32.powf(-20.0 / 20.0);
        let peak = buffer
            .data
            .iter()
            .fold(0.0_f32, |acc, value| acc.max(value.abs()));
        assert!(
            peak > amplitude * 0.9 && peak <= amplitude * 1.001,
            "peak {peak} must sit at the requested level {amplitude}"
        );
        // 块尾（真实时长之外的整块填充）是静音，不会多出声。
        let tail = &buffer.data[buffer.total_samples as usize * 2..];
        assert!(
            tail.iter().all(|value| *value == 0.0),
            "block padding must be silent"
        );
        // 淡出末端小：最后采样不超过 电平 x (2/960)。
        let last = buffer.data[(buffer.total_samples as usize - 1) * 2 + 1].abs();
        assert!(
            last <= amplitude * (2.0 / 960.0) + 0.001,
            "fade-out tail {last} must be near zero"
        );
    }

    #[test]
    fn noise_tone_is_deterministic_band_limited_and_only_target_dac() {
        let format = EngineConfig {
            block_frames: 128,
            sample_rate: 96_000,
            channels: 22,
        };
        let request = request(TestToneKind::Noise, 21, 500);
        let first = render_buffer(&format, &request).expect("noise buffer");
        let second = render_buffer(&format, &request).expect("noise buffer again");
        assert_eq!(
            first.data, second.data,
            "the same request must render identically"
        );
        // 只有目标 DAC（21）非零，其余 21 个通道全 0。
        for frame in 0..first.total_samples as usize {
            for channel in 0..22 {
                if channel != 21 {
                    assert_eq!(
                        first.data[frame * 22 + channel],
                        0.0,
                        "channel {channel} must stay silent"
                    );
                }
            }
        }
        // 4 阶 Butterworth 带通：过零率显著低于白噪声（同一口径白噪声约 96 kHz），
        // 且 500 Hz - 4 kHz 带内能量必须至少比 6 kHz 以上带外高 10 dB。
        let target: Vec<f32> = (0..first.total_samples as usize)
            .map(|frame| first.data[frame * 22 + 21])
            .collect();
        let mut crossings = 0_u64;
        let mut previous = 0.0_f32;
        for value in &target {
            let value = *value;
            if previous != 0.0 && value != 0.0 && (previous > 0.0) != (value > 0.0) {
                crossings += 1;
            }
            previous = value;
        }
        let estimated_hz = crossings as f64 * 2.0 / 0.5;
        assert!(
            (5_000.0..=25_000.0).contains(&estimated_hz),
            "band-limited noise crossing rate ~{estimated_hz} Hz (white noise would be ~96000 Hz)"
        );
        let average = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
        let in_band = average(
            &[600.0, 1_000.0, 1_600.0, 2_400.0, 3_400.0]
                .iter()
                .map(|freq| bin_power(&target, 96_000, *freq))
                .collect::<Vec<f64>>(),
        );
        let out_band = average(
            &[6_000.0, 8_000.0, 12_000.0, 20_000.0, 32_000.0]
                .iter()
                .map(|freq| bin_power(&target, 96_000, *freq))
                .collect::<Vec<f64>>(),
        );
        let ratio_db = 20.0 * (out_band / in_band).log10();
        assert!(
            ratio_db <= -10.0,
            "band-limited noise out-of-band energy {ratio_db} dB must be at least 10 dB below in-band"
        );
        let amplitude = 10.0_f32.powf(DEFAULT_DB / 20.0);
        let peak = first
            .data
            .iter()
            .fold(0.0_f32, |acc, value| acc.max(value.abs()));
        assert!(
            peak <= amplitude * 1.05,
            "noise peak {peak} must respect the level"
        );
        assert!(peak > amplitude * 0.2, "noise must be audible at -30 dBFS");
    }

    #[test]
    fn request_validation_requires_consent_and_strict_ranges() {
        let format = format(2);
        let base = json!({
            "dac": 1,
            "ms": 1_000,
            "kind": "sine",
            "audible": true,
            "confirm": true,
        });
        let parse_request = |value: Value| {
            let object = value.as_object().expect("object").clone();
            TestToneRequest::parse(&object, &format)
        };
        // 合法请求：默认电平 -30 dB，默认 sine 频率 1 kHz。
        let valid = parse_request(base.clone()).expect("valid request");
        assert_eq!(valid.level_db, DEFAULT_DB);
        assert_eq!(valid.freq_hz, Some(DEFAULT_SINE_HZ));
        // 缺少/否定 audible、confirm 一律拒绝（测试音会出声，不允许默认同意）。
        for field in ["audible", "confirm"] {
            let mut missing = base.clone();
            missing.as_object_mut().expect("object").remove(field);
            assert!(
                parse_request(missing).is_err(),
                "missing {field} must be rejected"
            );
            let mut denied = base.clone();
            denied[field] = json!(false);
            assert!(
                parse_request(denied).is_err(),
                "{field}:false must be rejected"
            );
        }
        // dac 越界、毫秒越界、种类未知、电平越界、sine freq_hz 越界全部拒绝。
        let mut cases: Vec<(&str, Value)> = Vec::new();
        let mut value = base.clone();
        value["dac"] = json!(2);
        cases.push(("dac out of range", value));
        let mut value = base.clone();
        value["dac"] = json!(-1);
        cases.push(("negative dac", value));
        let mut value = base.clone();
        value["ms"] = json!(499);
        cases.push(("ms too short", value));
        let mut value = base.clone();
        value["ms"] = json!(10_001);
        cases.push(("ms too long", value));
        let mut value = base.clone();
        value["kind"] = json!("square");
        cases.push(("unknown kind", value));
        let mut value = base.clone();
        value["db"] = json!(-61);
        cases.push(("db too quiet", value));
        let mut value = base.clone();
        value["db"] = json!(-11);
        cases.push(("db too loud", value));
        let mut value = base.clone();
        value["freq_hz"] = json!(50);
        cases.push(("freq_hz too low", value));
        for (label, value) in cases {
            assert!(parse_request(value).is_err(), "{label} must be rejected");
        }
        // noise 合法；dac=0 合法（第一个通道）；noise 带 freq_hz 宽容忽略（UI 可能统一带默认值）。
        let mut noise = base.clone();
        noise["kind"] = json!("noise");
        noise["dac"] = json!(0);
        assert_eq!(parse_request(noise).expect("noise request").freq_hz, None);
        let mut noise_with_freq = base.clone();
        noise_with_freq["kind"] = json!("noise");
        noise_with_freq["freq_hz"] = json!(1_000.0);
        assert_eq!(
            parse_request(noise_with_freq)
                .expect("noise ignores freq_hz")
                .freq_hz,
            None
        );
    }

    #[test]
    fn slot_status_reports_remaining_time_and_clears_on_cancel() {
        let slot = TestToneSlot::shared();
        assert!(!slot.active());
        let idle = slot.status_json();
        assert_eq!(idle["active"], json!(false));
        assert_eq!(idle["dac"], Value::Null);
        assert_eq!(idle["remaining_ms"], json!(0));
        let progress = Arc::new(CalibrationProgress::new(10));
        let (_id, _progress, cancelled) = slot.install(
            &request(TestToneKind::Sine, 1, 1_000),
            progress,
            10,
            128,
            48_000,
        );
        assert!(slot.active());
        let playing = slot.status_json();
        assert_eq!(playing["active"], json!(true));
        assert_eq!(playing["dac"], json!(1));
        assert_eq!(playing["kind"], json!("sine"));
        // 10 块 x 128 帧 @ 48 kHz = 26.6 ms -> 整数毫秒 26。
        assert_eq!(playing["remaining_ms"], json!(26));
        let progress = slot.progress_handle().expect("progress handle");
        assert_eq!(progress.total_blocks(), 10);
        assert!(slot.cancel_active());
        assert!(
            cancelled.load(Ordering::Acquire),
            "cancel must mark the watchdog"
        );
        assert!(!slot.active());
        assert!(!slot.cancel_active(), "cancel is idempotent");
        assert_eq!(slot.status_json()["active"], json!(false));
    }

    #[test]
    fn core_starts_and_stops_a_tone_and_reports_busy() {
        let (commands, receiver) = mpsc::channel();
        let (seen, handle) = fake_block_thread(receiver);
        let mut core = core_with(2, commands);

        let status = op(&mut core, json!({ "op": "status" }));
        let wired = status["wired"].as_array().expect("wired array");
        assert!(wired.iter().any(|value| value == "test_tone"));
        assert!(wired.iter().any(|value| value == "test_tone_stop"));
        assert_eq!(status["speaker_test"]["active"], json!(false));
        assert_eq!(status["speaker_test"]["dac"], Value::Null);

        let started = op(&mut core, tone_request("sine", 1, 500));
        assert_eq!(started["ok"], json!(true));
        assert_eq!(started["dac"], json!(1));
        assert_eq!(started["kind"], json!("sine"));
        assert_eq!(started["level_db"], json!(DEFAULT_DB));
        // 500 ms @ 48 kHz / 128 = 24000 帧 -> 188 块（含块尾填充）。
        assert_eq!(started["total_blocks"], json!(188));
        assert_eq!(started["remaining_ms"], json!(500));

        let playing = op(&mut core, json!({ "op": "status" }));
        assert_eq!(playing["speaker_test"]["active"], json!(true));
        assert_eq!(playing["speaker_test"]["dac"], json!(1));
        assert_eq!(playing["speaker_test"]["kind"], json!("sine"));

        // 已有测试音在播：再次 start 必须 busy（不排队、不叠加）。
        let busy = op(&mut core, tone_request("sine", 1, 500));
        assert_eq!(busy["ok"], json!(false));
        assert_eq!(busy["error"], json!(BUSY));

        let stopped = op(&mut core, json!({ "op": "test_tone_stop" }));
        assert_eq!(stopped["ok"], json!(true));
        assert_eq!(stopped["stopped"], json!(true));
        let stopped_again = op(&mut core, json!({ "op": "test_tone_stop" }));
        assert_eq!(stopped_again["ok"], json!(true));
        assert_eq!(
            stopped_again["stopped"],
            json!(false),
            "stop must be idempotent"
        );
        let idle = op(&mut core, json!({ "op": "status" }));
        assert_eq!(idle["speaker_test"]["active"], json!(false));

        drop(core);
        handle.join().expect("fake block loop");
        let seen = seen.lock().expect("seen lock").clone();
        assert_eq!(seen, vec!["begin:188".to_string(), "end".to_string()]);
    }

    #[test]
    fn mute_interrupts_a_playing_test_tone() {
        let (commands, receiver) = mpsc::channel();
        let (seen, handle) = fake_block_thread(receiver);
        let mut core = core_with(2, commands);
        assert_eq!(
            op(&mut core, tone_request("sine", 1, 500))["ok"],
            json!(true)
        );
        // 安全约束（SPEAKER-MAPPING P3.5）：/api/mute 必须能立即打断测试音
        // （测试音走校准播放路径，用户静音标志不影响它）。
        let muted = op(&mut core, json!({ "op": "mute", "mute": true }));
        assert_eq!(muted["ok"], json!(true));
        assert_eq!(muted["speaker_test_stopped"], json!(true));
        let status = op(&mut core, json!({ "op": "status" }));
        assert_eq!(status["speaker_test"]["active"], json!(false));
        // 之后的 stop 幂等（已经没有测试音）。
        assert_eq!(
            op(&mut core, json!({ "op": "test_tone_stop" }))["stopped"],
            json!(false)
        );
        drop(core);
        handle.join().expect("fake block loop");
        let seen = seen.lock().expect("seen lock").clone();
        assert_eq!(
            seen,
            vec![
                "begin:188".to_string(),
                "mute:true".to_string(),
                "end".to_string()
            ],
            "mute must end the calibration playback right after the mute command"
        );
    }

    #[test]
    fn invalid_requests_never_reach_the_block_loop() {
        let (commands, receiver) = mpsc::channel();
        let (seen, handle) = fake_block_thread(receiver);
        let mut core = core_with(2, commands);
        let mut cases: Vec<Value> = Vec::new();
        let mut value = tone_request("sine", 1, 500);
        value.as_object_mut().expect("object").remove("audible");
        cases.push(value);
        let mut value = tone_request("sine", 1, 500);
        value["confirm"] = json!(false);
        cases.push(value);
        cases.push(tone_request("sine", 2, 500));
        cases.push(tone_request("sine", 1, 499));
        cases.push(tone_request("sine", 1, 10_001));
        cases.push(tone_request("square", 1, 500));
        for value in cases {
            let response = op(&mut core, value);
            assert_eq!(response["ok"], json!(false));
            assert_eq!(
                response["error"],
                json!("bad_request"),
                "response: {response}"
            );
        }
        drop(core);
        handle.join().expect("fake block loop");
        assert!(
            seen.lock().expect("seen lock").is_empty(),
            "invalid requests must not queue a playback command"
        );
    }

    #[test]
    fn start_fails_honestly_when_the_block_loop_is_gone() {
        let (commands, receiver) = mpsc::channel::<ControlCommand>();
        drop(receiver);
        let mut core = core_with(2, commands);
        let started = op(&mut core, tone_request("sine", 1, 500));
        assert_eq!(started["ok"], json!(false));
        assert_eq!(started["error"], json!("not_applied"));
        let status = op(&mut core, json!({ "op": "status" }));
        assert_eq!(
            status["speaker_test"]["active"],
            json!(false),
            "nothing is playing when the block loop never registered the playback"
        );
    }

    #[test]
    fn auto_stop_watchdog_ends_the_tone_without_a_stop_request() {
        let (commands, receiver) = mpsc::channel();
        let (seen, handle) = fake_block_thread(receiver);
        let mut core = core_with(2, commands);
        let started = op(&mut core, tone_request("sine", 1, 500));
        assert_eq!(started["ok"], json!(true));
        assert_eq!(
            op(&mut core, json!({ "op": "status" }))["speaker_test"]["active"],
            json!(true)
        );
        // 假块线程不会推进 progress -> 看门狗走兜底时限（500 ms + 1 s）自动收尾。
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let status = op(&mut core, json!({ "op": "status" }));
            if status["speaker_test"]["active"] == json!(false) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "auto-stop watchdog did not finish the tone"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(core);
        handle.join().expect("fake block loop");
        let seen = seen.lock().expect("seen lock").clone();
        assert_eq!(seen.first().map(String::as_str), Some("begin:188"));
        assert_eq!(seen.last().map(String::as_str), Some("end"));
        assert_eq!(
            seen.len(),
            2,
            "auto-stop must send exactly one end: {seen:?}"
        );
    }
}
