//! 校准引擎 seam（T70f-c）：把「测量会话怎么播放/怎么采集」和「校准状态机」分开。
//!
//! 两种实现：
//! - [FakeCalFactory]：离线合成引擎（T67 FakeEngine 的包装），平台无关；
//!   Windows 本机测试与干跑用，产出的测量**必须**标 simulated（不冒充真机测量）。
//! - InProcessCalFactory（cfg(target_os = "linux")）：真机路径。播放不另开进程/设备：
//!   T61 时间轴在非实时线程预渲染成交错 f32，经 T70f-a 的命令通道交给块线程
//!   （ControlCommand::CalPlaybackBegin），块线程用 Runtime::run_calibration_block
//!   逐块写已打开的 MOTU；UMIK 采集在同一个非实时线程里用 ALSA capture 读块。
//!   **不抢设备 owner、不新开 socket、不退出进程**。
//!
//! 进度：CalProgress 是状态机/status 与引擎之间唯一的共享对象（原子量 + phase 字符串，
//! 只有控制面线程读写，不在 RT 路径上）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use khaen_calibration::stage_backend::{
    AudioCaptureResult, AudioSessionRequest, FakeEngine, FakeEngineConfig, PlaybackSource,
    StageAudioEngine, StageBackendConfig, StageDeviceConfig, StageError, StageProbe,
};

use crate::control::command::ControlCommand;

#[cfg(target_os = "linux")]
use crate::control::command::CommandReply;
#[cfg(target_os = "linux")]
use khaen_calibration::stage_backend::{E_CAPTURE_FAILED, E_PLAYBACK_FAILED};

use super::{CalEngineKind, CalibrationConfig};

/// 一次校准测量的进度（控制面线程读写；RT 路径只读原子量，不参与）。
#[derive(Debug)]
pub struct CalProgress {
    blocks_total: AtomicU64,
    blocks_done: AtomicU64,
    finished: AtomicBool,
    channel_index: AtomicU64,
    channels_total: AtomicU64,
    phase: Mutex<&'static str>,
}

impl Default for CalProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl CalProgress {
    /// 空进度（phase = idle）。
    pub fn new() -> Self {
        CalProgress {
            blocks_total: AtomicU64::new(0),
            blocks_done: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            channel_index: AtomicU64::new(0),
            channels_total: AtomicU64::new(0),
            phase: Mutex::new("idle"),
        }
    }

    /// 开始一次新测量：清零计数。
    pub fn reset(&self) {
        self.blocks_total.store(0, Ordering::Release);
        self.blocks_done.store(0, Ordering::Release);
        self.finished.store(false, Ordering::Release);
        self.channel_index.store(0, Ordering::Release);
        self.channels_total.store(0, Ordering::Release);
        self.set_phase("rendering");
    }

    /// 逐通道会话的进度：当前通道序号（0-based）与总通道数（多通道逐 session 测量）。
    pub fn set_channels(&self, index: u64, total: u64) {
        self.channel_index.store(index, Ordering::Release);
        self.channels_total.store(total, Ordering::Release);
    }

    /// 总块数（0 = 尚未开始/未知，不编数）。
    pub fn set_total(&self, total: u64) {
        self.blocks_total.store(total, Ordering::Release);
    }

    /// 已播块数。
    pub fn set_done(&self, done: u64) {
        self.blocks_done.store(done, Ordering::Release);
    }

    /// 整段扫频是否已经写出。
    pub fn set_finished(&self, finished: bool) {
        self.finished.store(finished, Ordering::Release);
    }

    /// 阶段名（rendering / playing_capture / finalizing / done / failed）。
    pub fn set_phase(&self, phase: &'static str) {
        let mut guard = self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = phase;
    }

    /// 进度快照（status 用）。
    pub fn snapshot(&self) -> ProgressSnapshot {
        let phase = *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ProgressSnapshot {
            total_blocks: self.blocks_total.load(Ordering::Acquire),
            blocks_done: self.blocks_done.load(Ordering::Acquire),
            finished: self.finished.load(Ordering::Acquire),
            channel_index: self.channel_index.load(Ordering::Acquire),
            channels_total: self.channels_total.load(Ordering::Acquire),
            phase,
        }
    }
}

/// 进度快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// 总块数（0 = 未知）。
    pub total_blocks: u64,
    /// 已播块数。
    pub blocks_done: u64,
    /// 扫频是否已全部写出。
    pub finished: bool,
    /// 当前通道序号（0-based）。
    pub channel_index: u64,
    /// 本 pass 总通道数。
    pub channels_total: u64,
    /// 阶段名。
    pub phase: &'static str,
}

/// 校准引擎工厂：每次测量新建一个引擎（采集设备在测量会话内打开/关闭，不在播放模式占用）。
pub trait CalEngineFactory: Send + Sync {
    /// 工厂种类（证据）。
    fn kind(&self) -> CalEngineKind;

    /// 引擎名（写进 status/证据；不冒充真机）。
    fn engine_name(&self) -> &'static str;

    /// 是否离线合成（simulated）：true 时任何测量/设计都必须标 simulated/PREDICTED_ONLY。
    fn simulated(&self) -> bool;

    /// 为一次校准模式会话创建引擎（enter 时调用，失败 = 设备不可用/未声明）。
    fn create(
        &self,
        config: &StageBackendConfig,
        progress: Arc<CalProgress>,
    ) -> Result<Box<dyn StageAudioEngine>, StageError>;
}

// ===========================================================================
// Fake（平台无关；测试/干跑）
// ===========================================================================

/// 离线合成引擎工厂（T67 FakeEngine + 进度镜像）。
pub struct FakeCalFactory {
    config: FakeEngineConfig,
}

impl FakeCalFactory {
    /// 默认假引擎（确定性）。
    pub fn new() -> Self {
        Self {
            config: FakeEngineConfig::default(),
        }
    }

    /// 指定假引擎配置（负向用例：force_unavailable/force_timeout/force_clip；仅测试用）。
    #[cfg(test)]
    pub fn with_config(config: FakeEngineConfig) -> Self {
        Self { config }
    }
}

impl Default for FakeCalFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl CalEngineFactory for FakeCalFactory {
    fn kind(&self) -> CalEngineKind {
        CalEngineKind::Fake
    }

    fn engine_name(&self) -> &'static str {
        "fake-synth"
    }

    fn simulated(&self) -> bool {
        true
    }

    fn create(
        &self,
        _config: &StageBackendConfig,
        progress: Arc<CalProgress>,
    ) -> Result<Box<dyn StageAudioEngine>, StageError> {
        Ok(Box::new(FakeCalEngine {
            inner: FakeEngine::new(self.config),
            progress,
        }))
    }
}

/// 包一层 T67 FakeEngine：把离线合成会话的进度镜像给 status（假引擎不碰设备，永远不冒充真机）。
struct FakeCalEngine {
    inner: FakeEngine,
    progress: Arc<CalProgress>,
}

impl StageAudioEngine for FakeCalEngine {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn probe(&mut self, devices: &StageDeviceConfig) -> Result<StageProbe, StageError> {
        self.inner.probe(devices)
    }

    fn run_session(
        &mut self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
    ) -> Result<AudioCaptureResult, StageError> {
        let block = request.block_frames.max(1) as u64;
        self.progress
            .set_total(playback.total_frames().div_ceil(block));
        self.progress.set_done(0);
        self.progress.set_phase("playing_capture");
        let result = self.inner.run_session(request, playback)?;
        self.progress
            .set_done(playback.total_frames().div_ceil(block));
        self.progress.set_finished(true);
        self.progress.set_phase("finalizing");
        Ok(result)
    }
}

// ===========================================================================
// 真机（Linux）：块线程写 MOTU（校准播放 seam）+ 本线程 ALSA 采集 UMIK
// ===========================================================================

/// 真机进程内引擎工厂（Linux）。非 Linux 平台没有真实采集路径：由 [build_factory]
/// 降级到 fake 并标 simulated，绝不假装真机测量。
#[cfg(target_os = "linux")]
pub struct InProcessCalFactory {
    commands: Sender<ControlCommand>,
}

#[cfg(target_os = "linux")]
impl InProcessCalFactory {
    /// 构造：命令通道 = T70f-a 控制面发给块线程的同一条通道。
    pub fn new(commands: Sender<ControlCommand>) -> Self {
        Self { commands }
    }
}

#[cfg(target_os = "linux")]
impl CalEngineFactory for InProcessCalFactory {
    fn kind(&self) -> CalEngineKind {
        CalEngineKind::InProcess
    }

    fn engine_name(&self) -> &'static str {
        "rt-host-in-process-alsa"
    }

    fn simulated(&self) -> bool {
        false
    }

    fn create(
        &self,
        config: &StageBackendConfig,
        progress: Arc<CalProgress>,
    ) -> Result<Box<dyn StageAudioEngine>, StageError> {
        Ok(Box::new(InProcessCalEngine {
            devices: config.devices.clone(),
            post_seconds: config.sweep.post_seconds,
            commands: self.commands.clone(),
            progress,
        }))
    }
}

/// 采集累积器（真机引擎用）：字节 / 块日志 / 帧计数 / 严格递增时间戳。
#[cfg(target_os = "linux")]
struct CaptureAccumulator {
    raw: Vec<u8>,
    blocks: Vec<khaen_calibration::stage_backend::AudioCaptureBlock>,
    frames: u64,
    last_ns: u64,
    bytes_per_frame: usize,
}

#[cfg(target_os = "linux")]
impl CaptureAccumulator {
    fn new(devices: &StageDeviceConfig) -> Self {
        CaptureAccumulator {
            raw: Vec::new(),
            blocks: Vec::new(),
            frames: 0,
            last_ns: 0,
            bytes_per_frame: devices.capture_channels as usize * devices.bytes_per_sample(),
        }
    }

    /// 收一个采集块：字节入 raw，块日志带严格递增的墙钟 ns（证据/日志；时基用
    /// capture_start_render_frame，不依赖这些时间戳）。
    fn push(&mut self, bytes: Vec<u8>) -> Result<(), StageError> {
        let frames_read = bytes.len() / self.bytes_per_frame.max(1);
        if frames_read == 0 {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                "UMIK block has no frames",
            ));
        }
        self.raw.extend_from_slice(&bytes);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            .max(self.last_ns.saturating_add(1));
        self.last_ns = now;
        self.blocks
            .push(khaen_calibration::stage_backend::AudioCaptureBlock {
                mono_ns: now,
                bytes,
            });
        self.frames += frames_read as u64;
        Ok(())
    }
}

/// 真机引擎：预渲染 → 采集先起 → 命令块线程开始扫频播放 → 边读采集边等播完 → 收尾。
#[cfg(target_os = "linux")]
struct InProcessCalEngine {
    devices: StageDeviceConfig,
    post_seconds: f64,
    commands: Sender<ControlCommand>,
    progress: Arc<CalProgress>,
}

#[cfg(target_os = "linux")]
impl InProcessCalEngine {
    /// 打开 UMIK 采集 PCM（S24_3LE / 48 kHz / 设备通道宽度），返回句柄。
    fn open_capture(devices: &StageDeviceConfig) -> Result<alsa::pcm::PCM, StageError> {
        use alsa::pcm::{Access, Format, HwParams, PCM};
        use alsa::{Direction, ValueOr};

        let pcm = PCM::new(devices.capture_device.as_str(), Direction::Capture, false).map_err(
            |error| {
                StageError::unavailable(format!(
                    "open UMIK capture device {:?}: {error}",
                    devices.capture_device
                ))
            },
        )?;
        {
            let params = HwParams::any(&pcm)
                .map_err(|error| StageError::unavailable(format!("UMIK hwparams: {error}")))?;
            params
                .set_access(Access::RWInterleaved)
                .map_err(|error| StageError::unavailable(format!("UMIK access: {error}")))?;
            params
                .set_format(Format::S243LE)
                .map_err(|error| StageError::unavailable(format!("UMIK S24_3LE: {error}")))?;
            params
                .set_channels(u32::from(devices.capture_channels))
                .map_err(|error| {
                    StageError::unavailable(format!(
                        "UMIK channels {}: {error}",
                        devices.capture_channels
                    ))
                })?;
            params
                .set_rate(devices.sample_rate, ValueOr::Nearest)
                .map_err(|error| StageError::unavailable(format!("UMIK rate: {error}")))?;
            params
                .set_period_size(devices.block_frames as i64, ValueOr::Nearest)
                .map_err(|error| StageError::unavailable(format!("UMIK period: {error}")))?;
            params
                .set_buffer_size(devices.buffer_frames as i64)
                .map_err(|error| StageError::unavailable(format!("UMIK buffer: {error}")))?;
            pcm.hw_params(&params).map_err(|error| {
                StageError::unavailable(format!("UMIK commit hwparams: {error}"))
            })?;
        }
        pcm.start()
            .map_err(|error| StageError::unavailable(format!("UMIK start: {error}")))?;
        Ok(pcm)
    }

    /// 发一条块边界命令等 ack（超时如实失败，不假报已开始）。非 RT 线程。
    fn send_command(
        &self,
        build: impl FnOnce(Sender<CommandReply>) -> ControlCommand,
    ) -> Result<CommandReply, StageError> {
        let (ack, replies) = std::sync::mpsc::channel();
        self.commands.send(build(ack)).map_err(|_| {
            StageError::new(
                E_PLAYBACK_FAILED,
                "rt-host block loop command channel is closed (data plane stopped); calibration playback was not started",
            )
        })?;
        match replies.recv_timeout(crate::control::core::COMMAND_ACK_TIMEOUT) {
            Ok(reply) => Ok(reply),
            Err(_) => Err(StageError::new(
                E_PLAYBACK_FAILED,
                "rt-host block loop did not acknowledge the calibration playback command within the timeout",
            )),
        }
    }

    /// 预渲染整段扫频为帧交错 f32。
    fn render_playback(
        &self,
        playback: &mut dyn PlaybackSource,
    ) -> Result<(Vec<f32>, usize, usize, u64), StageError> {
        let total_frames = playback.total_frames();
        let block = playback.block_frames() as usize;
        let channels = playback.channels() as usize;
        if block == 0 || channels == 0 || total_frames == 0 {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                "calibration playback source reported zero frames/block/channels",
            ));
        }
        self.progress.set_phase("rendering");
        self.progress.set_total(total_frames.div_ceil(block as u64));
        self.progress.set_done(0);
        let mut interleaved = vec![0.0f32; total_frames as usize * channels];
        let mut planar = vec![0.0f32; channels * block];
        let mut bytes = Vec::with_capacity(channels * block * 3);
        let mut written = 0u64;
        while written < total_frames {
            bytes.clear();
            let Some(frames) = playback.next_block(&mut planar, &mut bytes)? else {
                break;
            };
            let frames = frames as usize;
            if frames == 0 || frames > block {
                return Err(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!(
                        "calibration playback source returned {frames} frames for a {block}-frame block"
                    ),
                ));
            }
            for frame in 0..frames {
                let dst = (written as usize + frame) * channels;
                for channel in 0..channels {
                    interleaved[dst + channel] = planar[channel * block + frame];
                }
            }
            written += frames as u64;
        }
        if written != total_frames {
            return Err(StageError::new(
                E_PLAYBACK_FAILED,
                format!("calibration playback source delivered {written} of {total_frames} frames"),
            ));
        }
        Ok((interleaved, block, channels, total_frames))
    }

    /// 读一个采集块：返回设备原始字节（交错、capture_channels 宽）。
    fn read_capture_block(
        io: &alsa::pcm::IO<'_, u8>,
        buf: &mut [u8],
        devices: &StageDeviceConfig,
    ) -> Result<Vec<u8>, StageError> {
        let frames = io
            .readi(buf)
            .map_err(|error| StageError::new(E_CAPTURE_FAILED, format!("UMIK read: {error}")))?;
        let bytes = frames * devices.capture_channels as usize * devices.bytes_per_sample();
        if bytes == 0 || bytes > buf.len() {
            return Err(StageError::new(
                E_CAPTURE_FAILED,
                format!("UMIK read returned {bytes} bytes (buffer {})", buf.len()),
            ));
        }
        Ok(buf[..bytes].to_vec())
    }
}

#[cfg(target_os = "linux")]
impl StageAudioEngine for InProcessCalEngine {
    fn name(&self) -> &'static str {
        "rt-host-in-process-alsa"
    }

    fn probe(&mut self, devices: &StageDeviceConfig) -> Result<StageProbe, StageError> {
        // 只验证 UMIK 采集设备可打开且参数可设置；播放设备由块线程持有（不抢 owner），
        // 这里如实把 playback_ok 标为 true 并写进 notes 说明依据。
        let pcm = Self::open_capture(devices)?;
        drop(pcm);
        Ok(StageProbe {
            playback_ok: true,
            capture_ok: true,
            capture_sample_rates: vec![devices.sample_rate],
            capture_max_channels: devices.capture_channels,
            playback_sample_rates: vec![devices.sample_rate],
            notes: format!(
                "capture opened/closed on probe ({}ch @ {} Hz); playback owned by the rt-host block thread (calibration playback seam), no device-owner switch",
                devices.capture_channels, devices.sample_rate
            ),
        })
    }

    fn run_session(
        &mut self,
        request: &AudioSessionRequest,
        playback: &mut dyn PlaybackSource,
    ) -> Result<AudioCaptureResult, StageError> {
        let start = std::time::Instant::now();
        let (interleaved, block, channels, total_frames) = self.render_playback(playback)?;
        let total_blocks = total_frames.div_ceil(block as u64);
        self.progress.set_phase("playing_capture");

        // 采集先起（旧 cal_control/cal_umik 顺序：先开 UMIK，再让 DAC 出声）。
        let pcm = Self::open_capture(&self.devices)?;
        let capture_first = std::time::Instant::now();
        let io = pcm.io_bytes();
        let bytes_per_block =
            block * self.devices.capture_channels as usize * self.devices.bytes_per_sample();
        let mut read_buf = vec![0u8; bytes_per_block];
        let first_bytes = Self::read_capture_block(&io, &mut read_buf, &self.devices)?;
        let mut capture = CaptureAccumulator::new(&self.devices);
        capture.push(first_bytes)?;

        // 采集稳定期（旧口径 settle_ms）：先读够静音，再让块线程开始出声。
        let settle = std::time::Duration::from_millis(self.devices.settle_ms);
        while capture_first.elapsed() < settle {
            let bytes = Self::read_capture_block(&io, &mut read_buf, &self.devices)?;
            capture.push(bytes)?;
        }

        // 命令块线程开始校准播放（失败 → 明确错误，采集流直接关掉）。
        let host_progress = Arc::new(khaen_host::runtime::CalibrationProgress::new(total_blocks));
        let playback = khaen_host::runtime::CalibrationPlayback::new(
            interleaved,
            block,
            channels,
            Arc::clone(&host_progress),
        )
        .map_err(|error| {
            StageError::new(
                E_PLAYBACK_FAILED,
                format!("calibration playback buffer rejected: {error}"),
            )
        })?;
        let ack = self.send_command(|ack| ControlCommand::CalPlaybackBegin { playback, ack })?;
        let playback_started = std::time::Instant::now();
        match ack {
            CommandReply::CalPlaybackStarted {
                total_blocks: started,
            } => {
                if started != total_blocks {
                    return Err(StageError::new(
                        E_PLAYBACK_FAILED,
                        format!(
                            "block thread registered {started} blocks, expected {total_blocks}"
                        ),
                    ));
                }
            }
            CommandReply::Rejected { code, message } => {
                return Err(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("block thread rejected calibration playback: {code}: {message}"),
                ));
            }
            other => {
                return Err(StageError::new(
                    E_PLAYBACK_FAILED,
                    format!("unexpected calibration playback reply: {other:?}"),
                ));
            }
        }
        // seed：capture sample 0 对应的 render frame（capture 先起 → 负值）。
        let elapsed_seconds = capture_first
            .elapsed()
            .saturating_sub(playback_started.elapsed())
            .as_secs_f64();
        let seed = -(elapsed_seconds * request.sample_rate as f64);
        // 需要覆盖：|seed| + timeline + post 尾巴（+ 0.2 s 裕量）。
        let rate = request.sample_rate.max(1) as f64;
        let target_frames = (-seed).max(0.0).ceil() as u64
            + request.expected_capture_frames
            + (self.post_seconds.max(0.0) * rate).ceil() as u64
            + (0.2 * rate).ceil() as u64;
        let deadline = start + std::time::Duration::from_millis(request.timeout_ms.max(1));

        let mut failure: Option<StageError> = None;
        while capture.frames < target_frames {
            if std::time::Instant::now() > deadline {
                failure = Some(StageError::timeout(format!(
                    "UMIK capture did not reach {target_frames} frames before the session timeout ({} ms); captured {}, sweep blocks {}/{}",
                    request.timeout_ms,
                    capture.frames,
                    host_progress.blocks_done(),
                    host_progress.total_blocks()
                )));
                break;
            }
            self.progress.set_done(host_progress.blocks_done());
            self.progress.set_finished(host_progress.finished());
            match Self::read_capture_block(&io, &mut read_buf, &self.devices) {
                Ok(bytes) => {
                    if let Err(error) = capture.push(bytes) {
                        failure = Some(error);
                        break;
                    }
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        self.progress.set_done(host_progress.blocks_done());
        self.progress.set_finished(host_progress.finished());
        // 无论成功失败都结束校准播放：块线程不能一直写静音/扫频。
        let end = self.send_command(|ack| ControlCommand::CalPlaybackEnd { ack });
        let end_note = match end {
            Ok(CommandReply::CalPlaybackEnded { was_active }) => {
                format!("calibration playback ended by control thread (was_active={was_active})")
            }
            Ok(other) => format!("unexpected calibration playback end reply: {other:?}"),
            Err(error) => format!("calibration playback end ack failed: {error}"),
        };
        if let Some(error) = failure {
            let code = error.code.clone();
            let message = format!("{end_note}; {error}");
            return Err(StageError::new(code, message));
        }
        let wall_seconds = start.elapsed().as_secs_f64();
        Ok(AudioCaptureResult {
            raw: capture.raw,
            blocks: capture.blocks,
            capture_frames: capture.frames,
            // 真实 UMIK 的 xrun/overrun：ALSA readi 出错即失败（fail-closed），
            // 没有独立计数来源时不编数值（0 = 本次会话内没有发生可恢复错误）。
            xruns: 0,
            underruns: 0,
            seed_render_frame: seed,
            wall_seconds,
            notes: format!(
                "in-process: sweep rendered then played by the rt-host block thread through the existing MOTU PCM; UMIK captured on {} ({}ch S24_3LE @ {} Hz, period {}, buffer {}); seed estimated from capture/playback start instants; {end_note}",
                self.devices.capture_device,
                self.devices.capture_channels,
                self.devices.sample_rate,
                self.devices.block_frames,
                self.devices.buffer_frames
            ),
        })
    }
}

/// 按配置构造工厂。非 Linux 没有真机采集路径：auto 也降级为 fake 并标 simulated
/// （status 里如实写 real_capture=false + simulated=true）。
pub fn build_factory(
    config: &CalibrationConfig,
    commands: Sender<ControlCommand>,
) -> Box<dyn CalEngineFactory> {
    match config.engine {
        CalEngineKind::Fake => Box::new(FakeCalFactory::new()),
        CalEngineKind::InProcess => {
            #[cfg(target_os = "linux")]
            {
                Box::new(InProcessCalFactory::new(commands))
            }
            #[cfg(not(target_os = "linux"))]
            {
                // 非 Linux 没有真实采集路径：显式请求真机引擎也如实降级为 fake 并标 simulated，
                // 绝不假装真机测量（status 里 engine/simulated/real_capture 说明这一点）。
                drop(commands);
                Box::new(FakeCalFactory::new())
            }
        }
        CalEngineKind::Auto => {
            #[cfg(target_os = "linux")]
            {
                Box::new(InProcessCalFactory::new(commands))
            }
            #[cfg(not(target_os = "linux"))]
            {
                drop(commands);
                Box::new(FakeCalFactory::new())
            }
        }
    }
}
