//! 真实 ALSA 播放后端（T70a）：打开 PCM 播放流、把 f32 帧交错编码成 S24_3LE、按块写出、xrun 恢复。
//!
//! # 分层
//!
//! - 平台无关核心 [`AlsaOutputBackend`] 泛型于 [`PcmOutput`]：块校验、f32 → S24_3LE 打包、短写续写、
//!   xrun 恢复策略与统计都在这里，因此可以在没有声卡的环境里用假 PCM 设备做逐字节测试。
//! - Linux 适配层 `AlsaPcm`（`linux` 模块，`cfg(target_os = "linux")`）：用 `alsa` 0.12 打开 hw 设备，
//!   配置 S24_3LE、period = `block_frames`、buffer = 4 个 period，并把写失败映射成可恢复/致命故障。
//!
//! # 实时约束
//!
//! `playout` / `capture` 在设备正常时零分配、零锁、无日志、不 panic：S24_3LE 字节缓冲在 `start`
//! 时一次分配，转换只写这块缓冲，写出只做切片拷贝与整数计数。实时路径唯一的设备交互是 PCM 写
//! （阻塞式 `writei`，正常时由设备周期唤醒），除此之外没有文件/日志/网络 IO。
//!
//! xrun 恢复成功路径同样不分配；只有设备进入不可恢复故障的终止路径才构造
//! `BackendError::Fault(String)`（此时流已不可用，错误必须带回非实时侧），这是本模块唯一的分配例外。
//! [`AlsaStats::last_error`] 用 `&'static str` 记录故障类别，不为此分配。
//!
//! # 边界
//!
//! - 只做播放（输出型后端）；`capture` 没有采集来源，填充静音。
//! - 不做采样率转换：设备实际速率/通道/周期与 [`DeviceConfig`] 不一致时 fail-closed（打开失败）。
//!   设备名应指向直连 `hw:` 设备；若传入 `default`/`plug` 之类的插件设备，是否重采样由 ALSA 插件
//!   决定，本模块无法也不假装能观测。
//! - 不负责设备枚举、速率断言与设备所有权（T70 设备监督器）；本模块只按给定设备名打开 PCM。
//! - xrun 只计数并如实上报，不擅自改变上层安全状态；T24 安全监督器读 [`AlsaStats`] 决定是否安全静音。
//!
//! # S24_3LE 编码
//!
//! 采样先乘 2^23 再四舍五入到最近整数（半值远离零），饱和到 24-bit 二进制补码范围
//! `[-8_388_608, 8_388_607]`，最后按小端三字节输出。没有 dither：编码是确定性的，同一 f32 永远
//! 得到同一字节序列（便于对拍与零分配测试）。

use crate::backend::{AudioBackend, BackendError, DeviceConfig};

/// 每个样本的字节数：S24_3LE 为 3 字节 packed。
pub const BYTES_PER_SAMPLE: usize = 3;

/// Linux 适配层请求的缓冲 period 数：`buffer_frames = 4 * block_frames`。
pub const BUFFER_PERIODS: usize = 4;

/// 每块允许成功恢复的 xrun 次数上限；超过即视为设备故障，不再无限重试。
pub const MAX_XRUN_RECOVERIES: u32 = 2;

/// f32 满幅对应的 24-bit 缩放（2^23）。
const FULL_SCALE: f32 = 8_388_608.0;

/// 24-bit 有符号整数最大正满幅。
const S24_MAX: i32 = 8_388_607;

/// 24-bit 有符号整数最小负满幅。
const S24_MIN: i32 = -8_388_608;

/// 可恢复的 PCM 故障：设备状态被破坏，`prepare` 之后可以继续写。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmFault {
    /// 欠载（ALSA `EPIPE` / 状态 `XRUN`）：应用写得太慢，设备把缓冲放空了。
    Underrun,
    /// 设备被挂起（ALSA `ESTRPIPE` / 状态 `SUSPENDED`），例如 USB 重新枚举或系统休眠。
    Suspended,
}

impl PcmFault {
    /// 稳定字符串（统计/日志/协议用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            PcmFault::Underrun => "xrun: underrun",
            PcmFault::Suspended => "xrun: suspended",
        }
    }
}

/// PCM 写故障。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmError {
    /// 可恢复故障：由核心调用 [`PcmOutput::prepare`] 后重试本块。
    Recoverable(PcmFault),
    /// 不可恢复故障（设备消失、参数不支持等）；上下文用短静态字符串描述。
    Fatal(&'static str),
}

/// 平台无关的 PCM 播放设备（真实 ALSA 或测试假设备）。
///
/// 约定：[`PcmOutput::write`] 收到的是整块 S24_3LE 交织字节（`frames * channels * 3` 字节），
/// 返回本设备已接受的帧数；允许短写，核心会从剩余字节继续。返回 `Ok(0)` 视为设备没有进展
/// （核心报错，避免无限循环）。实现方在实时路径上不得分配、加锁、打印或 panic。
pub trait PcmOutput: Sized {
    /// 按配置打开设备；失败原因向上映射成 [`BackendError`]。
    fn open(config: &DeviceConfig) -> Result<Self, BackendError>;

    /// 写一块 S24_3LE 交织字节，返回本设备接受的帧数（允许短写）。
    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError>;

    /// xrun/挂起之后的恢复（Linux：`prepare` 或 `resume`）。
    fn prepare(&mut self, fault: PcmFault) -> Result<(), BackendError>;

    /// 当前可写帧数（诊断用；`&self`，Linux 用 `avail_update`）。
    fn available(&self) -> Result<usize, PcmError>;

    /// 关闭设备；幂等，停止播放并丢弃未播完的样本（不做阻塞 drain）。
    fn close(&mut self);
}

/// 后端累计统计（音频线程内只做整数加法，不分配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlsaStats {
    /// 成功写出的完整块数。
    pub blocks_written: u64,
    /// 累计可恢复故障（xrun/挂起）次数；每次发生都 +1，包括恢复失败前的最后一次。
    pub xruns: u64,
    /// 累计写出的字节数（只统计完整写出的块）。
    pub bytes_written: u64,
    /// 最近一次故障的稳定描述；没有出过错为 `None`。成功后不清空（"最近一次错误"语义）。
    pub last_error: Option<&'static str>,
}

/// ALSA 播放后端：泛型于 [`PcmOutput`]，测试用假设备、Linux 用 `AlsaPcm`。
///
/// 未启动时除 [`AlsaOutputBackend::new`] 外的搬运接口都返回 [`BackendError::NotStarted`]。
/// 实时约束见模块文档；`start`/`stop` 是控制路径，允许分配与设备 IO。
pub struct AlsaOutputBackend<P: PcmOutput> {
    /// 已打开的设备；`None` 表示未启动。
    output: Option<P>,
    /// 生效配置。
    config: Option<DeviceConfig>,
    /// S24_3LE 编码缓冲：`block_frames * channels * 3` 字节，`start` 时一次分配。
    bytes: Vec<u8>,
    /// 累计统计。
    stats: AlsaStats,
}

impl<P: PcmOutput> AlsaOutputBackend<P> {
    /// 新建（未启动；不碰任何设备）。
    pub fn new() -> Self {
        Self {
            output: None,
            config: None,
            bytes: Vec::new(),
            stats: AlsaStats::default(),
        }
    }

    /// 是否已启动（设备已打开）。
    pub fn is_started(&self) -> bool {
        self.output.is_some()
    }

    /// 当前生效配置；未启动为 `None`。
    pub fn config(&self) -> Option<&DeviceConfig> {
        self.config.as_ref()
    }

    /// 每块的 S24_3LE 字节数；未启动为 0。
    pub fn block_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// 统计快照（按值返回，不把内部引用带出去）。
    pub fn stats(&self) -> AlsaStats {
        self.stats
    }

    /// 已打开设备的只读引用（诊断/测试用；不在实时线程调用）。
    pub fn output(&self) -> Option<&P> {
        self.output.as_ref()
    }

    /// 已打开设备的可变引用（仅非实时路径，例如测试里配置假设备或诊断设置）。
    pub fn output_mut(&mut self) -> Option<&mut P> {
        self.output.as_mut()
    }

    /// 设备当前可写帧数（诊断用，非实时路径调用）。
    ///
    /// 未启动返回 [`BackendError::NotStarted`]；设备故障映射成 [`BackendError::Fault`]。
    pub fn available_frames(&self) -> Result<usize, BackendError> {
        let output = self.output.as_ref().ok_or(BackendError::NotStarted)?;
        output.available().map_err(|err| match err {
            PcmError::Recoverable(fault) => BackendError::Fault(fault.as_str().to_string()),
            PcmError::Fatal(reason) => BackendError::Fault(reason.to_string()),
        })
    }

    /// 关闭设备并清掉配置与块缓冲；统计保留到下一次 `start`。幂等。
    fn shutdown(&mut self) {
        if let Some(mut output) = self.output.take() {
            output.close();
        }
        self.config = None;
        self.bytes = Vec::new();
    }
}

impl<P: PcmOutput> Default for AlsaOutputBackend<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: PcmOutput> core::fmt::Debug for AlsaOutputBackend<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AlsaOutputBackend")
            .field("config", &self.config)
            .field("block_bytes", &self.bytes.len())
            .field("stats", &self.stats)
            .field("started", &self.output.is_some())
            .finish()
    }
}

impl<P: PcmOutput> AudioBackend for AlsaOutputBackend<P> {
    fn start(&mut self, config: &DeviceConfig) -> Result<(), BackendError> {
        let block_bytes = validate_config(config)?;
        // 重复 start 等价于"停止后重新打开"：先释放旧句柄与旧缓冲（非实时路径允许分配/IO）。
        self.shutdown();
        self.stats = AlsaStats::default();
        let output = P::open(config)?;
        self.bytes = vec![0u8; block_bytes];
        self.output = Some(output);
        self.config = Some(config.clone());
        Ok(())
    }

    fn capture(&mut self, out: &mut [f32]) -> Result<(), BackendError> {
        let expected = match self.config.as_ref() {
            Some(config) => config.samples_per_block(),
            None => return Err(BackendError::NotStarted),
        };
        if out.len() != expected {
            return Err(BackendError::WrongBlockLength {
                expected,
                actual: out.len(),
            });
        }
        // 输出型后端没有采集来源：如实填充静音，不假装采集到设备数据。
        out.fill(0.0);
        Ok(())
    }

    fn playout(&mut self, block: &[f32]) -> Result<(), BackendError> {
        let (expected_samples, bytes_per_frame) = match self.config.as_ref() {
            Some(config) => (
                config.samples_per_block(),
                config.channels * BYTES_PER_SAMPLE,
            ),
            None => return Err(BackendError::NotStarted),
        };
        if block.len() != expected_samples {
            return Err(BackendError::WrongBlockLength {
                expected: expected_samples,
                actual: block.len(),
            });
        }
        let output = match self.output.as_mut() {
            Some(output) => output,
            None => return Err(BackendError::NotStarted),
        };
        if let Err(err) = encode_s24_3le_block(block, &mut self.bytes) {
            // 块长已校验，正常不会到这里；真到了说明内部缓冲与配置不一致，按故障上报。
            self.stats.last_error = Some("s24_3le buffer mismatch");
            return Err(err);
        }

        let total_bytes = self.bytes.len();
        let mut written = 0usize;
        let mut recoveries = 0u32;
        while written < total_bytes {
            match output.write(&self.bytes[written..]) {
                Ok(frames) => {
                    let advanced = frames.checked_mul(bytes_per_frame);
                    let Some(advanced) = advanced else {
                        self.stats.last_error = Some("pcm write returned invalid frame count");
                        return Err(BackendError::Fault(
                            "pcm write returned invalid frame count".to_string(),
                        ));
                    };
                    if advanced == 0 {
                        self.stats.last_error = Some("pcm write made no progress");
                        return Err(BackendError::Fault(
                            "pcm write made no progress".to_string(),
                        ));
                    }
                    if advanced > total_bytes - written {
                        self.stats.last_error = Some("pcm write over-reported frames");
                        return Err(BackendError::Fault(
                            "pcm write over-reported frames".to_string(),
                        ));
                    }
                    written += advanced;
                }
                Err(PcmError::Recoverable(fault)) => {
                    // xrun 也计数、也记 last_error；恢复成功则本块继续，恢复不成功就是设备故障。
                    self.stats.xruns += 1;
                    self.stats.last_error = Some(fault.as_str());
                    if recoveries >= MAX_XRUN_RECOVERIES {
                        return Err(BackendError::Fault(
                            "xrun recovery limit reached".to_string(),
                        ));
                    }
                    recoveries += 1;
                    if let Err(err) = output.prepare(fault) {
                        self.stats.last_error = Some("xrun prepare failed");
                        return Err(err);
                    }
                }
                Err(PcmError::Fatal(reason)) => {
                    self.stats.last_error = Some(reason);
                    return Err(BackendError::Fault(reason.to_string()));
                }
            }
        }

        self.stats.blocks_written += 1;
        self.stats.bytes_written += total_bytes as u64;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        self.shutdown();
        Ok(())
    }
}

/// 校验配置并返回每块 S24_3LE 字节数。
fn validate_config(config: &DeviceConfig) -> Result<usize, BackendError> {
    if config.device.as_str().is_empty() {
        return Err(BackendError::BadConfig("device name must not be empty"));
    }
    if config.device.as_str().bytes().any(|unit| unit == 0) {
        // PCM 设备名最终要变成 C 字符串；内嵌 NUL 直接拒绝，绝不让底层 panic。
        return Err(BackendError::BadConfig("device name must not contain NUL"));
    }
    if config.sample_rate == 0 {
        return Err(BackendError::BadConfig("sample_rate must be positive"));
    }
    if config.block_frames == 0 {
        return Err(BackendError::BadConfig("block_frames must be positive"));
    }
    if config.channels == 0 {
        return Err(BackendError::BadConfig("channels must be positive"));
    }
    let samples = config
        .block_frames
        .checked_mul(config.channels)
        .ok_or(BackendError::BadConfig("block size overflow"))?;
    samples
        .checked_mul(BYTES_PER_SAMPLE)
        .ok_or(BackendError::BadConfig("block size overflow"))
}

/// 单个 f32 样本编码成 S24_3LE 三字节（规则见模块文档）。
pub fn encode_s24_3le(sample: f32) -> [u8; BYTES_PER_SAMPLE] {
    let value = quantize_s24(sample);
    [
        (value & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        ((value >> 16) & 0xFF) as u8,
    ]
}

/// 把一块 f32 帧交错样本编码进 `out`；`out.len()` 必须等于 `samples.len() * 3`。
///
/// 字节布局是"逐帧、帧内逐通道"的 S24_3LE：第 `f` 帧第 `c` 通道占
/// `(f * channels + c) * 3` 起的三个字节。通道重排属于图/路由层，本函数不做。
pub fn encode_s24_3le_block(samples: &[f32], out: &mut [u8]) -> Result<(), BackendError> {
    let expected = samples
        .len()
        .checked_mul(BYTES_PER_SAMPLE)
        .ok_or(BackendError::BadConfig("block size overflow"))?;
    if out.len() != expected {
        return Err(BackendError::WrongBlockLength {
            expected,
            actual: out.len(),
        });
    }
    // 用 chunks_mut 而不是 chunks_exact_mut：长度已在上面校验过，这里只做逐样本拷贝（不 panic、不分配）。
    for (chunk, sample) in out.chunks_mut(BYTES_PER_SAMPLE).zip(samples.iter()) {
        chunk.copy_from_slice(&encode_s24_3le(*sample));
    }
    Ok(())
}

/// f32 → 24-bit 有符号整数：饱和 + 四舍五入（半值远离零）。
fn quantize_s24(sample: f32) -> i32 {
    if sample.is_nan() {
        // NaN 无法映射成有效样本；输出 0 而不是传给设备当垃圾。
        return 0;
    }
    let scaled = sample * FULL_SCALE;
    // 先比较再取整：饱和判断放在 round 之前，避免 +1.0 溢出 24-bit 正满幅；
    // 同时天然处理 ±inf（比较成立）。
    if scaled >= S24_MAX as f32 {
        return S24_MAX;
    }
    if scaled <= S24_MIN as f32 {
        return S24_MIN;
    }
    scaled.round() as i32
}

#[cfg(target_os = "linux")]
pub use linux::AlsaPcm;

/// Linux 上可直接使用的真实 ALSA 播放后端。
#[cfg(target_os = "linux")]
pub type LinuxAlsaBackend = AlsaOutputBackend<AlsaPcm>;

#[cfg(target_os = "linux")]
mod linux {
    //! Linux ALSA 适配层：把 `alsa` 0.12 的播放 PCM 包成平台无关核心要求的 PCM 设备。
    //!
    //! 只负责设备相关部分：打开 `hw:` 设备、协商 S24_3LE / period / buffer、把写失败归类成
    //! 可恢复故障（xrun/挂起）或致命错误、按故障类型 prepare/resume、关闭句柄。块校验、编码、
    //! 统计与重试次数上限都在平台无关核心里。
    //!
    //! 本模块只在 `cfg(target_os = "linux")` 下编译：Windows 本机无法编译/运行，由 CI（Linux）
    //! 覆盖；平台无关核心在 Windows 上用假 PCM 设备测试。

    use alsa::pcm::{Access, Format, Frames, HwParams, State, PCM};
    use alsa::{Direction, ValueOr};

    use super::{PcmError, PcmFault, PcmOutput, BUFFER_PERIODS};
    use crate::backend::{BackendError, DeviceConfig};

    /// 真实的 ALSA 播放 PCM 流。
    #[derive(Debug)]
    pub struct AlsaPcm {
        /// 已打开的 PCM；`close` 之后为 `None`。
        pcm: Option<PCM>,
    }

    impl PcmOutput for AlsaPcm {
        fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
            let device = config.device.as_str();
            let channels = u32::try_from(config.channels)
                .map_err(|_| BackendError::BadConfig("channels does not fit in u32"))?;
            let period = Frames::try_from(config.block_frames)
                .map_err(|_| BackendError::BadConfig("block_frames does not fit in i64"))?;
            let buffer = period
                .checked_mul(BUFFER_PERIODS as Frames)
                .ok_or(BackendError::BadConfig("buffer frame count overflow"))?;

            let pcm = PCM::new(device, Direction::Playback, false)
                .map_err(|err| alsa_fault("open playback pcm", err))?;
            {
                let hwp = HwParams::any(&pcm).map_err(|err| alsa_fault("query hw params", err))?;
                hwp.set_access(Access::RWInterleaved)
                    .map_err(|err| alsa_fault("set access RW_INTERLEAVED", err))?;
                hwp.set_format(Format::S243LE)
                    .map_err(|err| alsa_fault("set format S24_3LE", err))?;
                hwp.set_channels(channels)
                    .map_err(|err| alsa_fault("set channels", err))?;
                hwp.set_rate(config.sample_rate, ValueOr::Nearest)
                    .map_err(|err| alsa_fault("set rate", err))?;
                hwp.set_period_size(period, ValueOr::Nearest)
                    .map_err(|err| alsa_fault("set period size", err))?;
                hwp.set_buffer_size(buffer)
                    .map_err(|err| alsa_fault("set buffer size", err))?;
                pcm.hw_params(&hwp)
                    .map_err(|err| alsa_fault("apply hw params", err))?;
            }

            // 回读校验：设备是执行者，参数不一致就 fail-closed（不重采样、不静默降级）。
            // T70 的语义是"速率/周期是方案数据"，这里只如实拒绝，不替上层猜。
            // 用独立作用域包住 current：HwParams 持有 PCM 的借用并在 Drop 里释放，
            // 出块后才能把 pcm 移动进 Self。
            {
                let current = pcm
                    .hw_params_current()
                    .map_err(|err| alsa_fault("read back hw params", err))?;
                let actual_format = current
                    .get_format()
                    .map_err(|err| alsa_fault("read back format", err))?;
                if actual_format != Format::S243LE {
                    return Err(BackendError::Fault(format!(
                        "alsa format mismatch: asked S24_3LE, got {actual_format:?}"
                    )));
                }
                let actual_rate = current
                    .get_rate()
                    .map_err(|err| alsa_fault("read back rate", err))?;
                if actual_rate != config.sample_rate {
                    return Err(BackendError::Fault(format!(
                        "alsa rate mismatch: asked {} Hz, got {actual_rate} Hz",
                        config.sample_rate
                    )));
                }
                let actual_channels = current
                    .get_channels()
                    .map_err(|err| alsa_fault("read back channels", err))?;
                if actual_channels != channels {
                    return Err(BackendError::Fault(format!(
                        "alsa channels mismatch: asked {channels}, got {actual_channels}"
                    )));
                }
                let actual_period = current
                    .get_period_size()
                    .map_err(|err| alsa_fault("read back period size", err))?;
                if actual_period != period {
                    return Err(BackendError::Fault(format!(
                        "alsa period mismatch: asked {period} frames, got {actual_period} frames"
                    )));
                }
                let actual_buffer = current
                    .get_buffer_size()
                    .map_err(|err| alsa_fault("read back buffer size", err))?;
                if actual_buffer < buffer {
                    return Err(BackendError::Fault(format!(
                        "alsa buffer too small: asked {buffer} frames, got {actual_buffer} frames"
                    )));
                }
            }

            // hw_params 之后流处于 PREPARED；显式 prepare 一次，把状态与错误都确认清楚。
            pcm.prepare()
                .map_err(|err| alsa_fault("prepare pcm", err))?;
            Ok(Self { pcm: Some(pcm) })
        }

        fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
            let pcm = self
                .pcm
                .as_ref()
                .ok_or(PcmError::Fatal("alsa pcm is closed"))?;
            // io_bytes 只是把 PCM 包一层（内部一个 Cell 标志），不分配；Drop 时复位标志。
            let io = pcm.io_bytes();
            match io.writei(bytes) {
                Ok(frames) => Ok(frames),
                // 不直接比较 errno（本 crate 只依赖 alsa，不依赖 libc）：EPIPE/ESTRPIPE 之后
                // alsa-lib 会把流状态切成 XRUN/SUSPENDED，用状态机区分可恢复与致命。
                // 代价是被信号打断（EINTR）这类非 xrun 错误会按致命上报（fail-closed），
                // 由上层监督器决定重开，不在实时线程里猜。
                Err(_err) => Err(classify_state(pcm)),
            }
        }

        fn prepare(&mut self, fault: PcmFault) -> Result<(), BackendError> {
            let pcm = self.pcm.as_ref().ok_or(BackendError::NotStarted)?;
            match fault {
                PcmFault::Underrun => pcm
                    .prepare()
                    .map_err(|err| alsa_fault("prepare after underrun", err)),
                PcmFault::Suspended => {
                    // ALSA 文档：挂起后先 snd_pcm_resume；设备还没恢复（-EAGAIN）再 prepare。
                    // 不在实时线程上循环 sleep 等设备回来：一次 resume 不成就按故障上报，
                    // 让上层（设备监督器）决定重开，避免把音频线程挂住。
                    if pcm.resume().is_ok() {
                        return Ok(());
                    }
                    pcm.prepare()
                        .map_err(|err| alsa_fault("prepare after suspend", err))
                }
            }
        }

        fn available(&self) -> Result<usize, PcmError> {
            let pcm = self
                .pcm
                .as_ref()
                .ok_or(PcmError::Fatal("alsa pcm is closed"))?;
            match pcm.avail_update() {
                // acheck! 只把非负值当成功，所以这里 frames >= 0。
                Ok(frames) => Ok(frames as usize),
                Err(_err) => Err(classify_state(pcm)),
            }
        }

        fn close(&mut self) {
            if let Some(pcm) = self.pcm.take() {
                // 停止播放并丢弃未播完的样本（stop 不做阻塞 drain）。
                let _ = pcm.drop();
                // pcm 在此离开作用域时 snd_pcm_close 释放句柄。
            }
        }
    }

    /// 非实时路径（打开/恢复失败）的 ALSA 错误映射：上下文 + ALSA 描述一起上报。
    ///
    /// 打开与 prepare 失败都是终止性路径，这里允许分配错误字符串；实时播放的正常路径不走这里。
    fn alsa_fault(context: &str, err: alsa::Error) -> BackendError {
        BackendError::Fault(format!("{context}: {err}"))
    }

    /// 用 ALSA 状态机把一次失败的读写归类：XRUN/SUSPENDED 可恢复，其余致命。
    fn classify_state(pcm: &PCM) -> PcmError {
        match pcm.state() {
            State::XRun => PcmError::Recoverable(PcmFault::Underrun),
            State::Suspended => PcmError::Recoverable(PcmFault::Suspended),
            State::Disconnected => PcmError::Fatal("alsa pcm disconnected"),
            _ => PcmError::Fatal("alsa pcm write failed"),
        }
    }
}
