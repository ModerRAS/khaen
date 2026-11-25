//! ALSA 播放后端门禁：用假 `PcmOutput` 在任意平台（含 Windows 本机）验证平台无关核心——
//! S24_3LE 逐字节打包、块长/通道/字节数、短写续写、xrun 恢复、错误映射、统计、stop 幂等与零分配。
//!
//! 真实 ALSA 设备（Linux 适配层）无法在本机运行：Windows 上整段 `cfg(target_os = "linux")` 不参与编译，
//! 由 CI（Linux）覆盖；Linux 专属用例在报告里标 NOT_RUN。

use std::alloc::{GlobalAlloc, Layout, System};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use khaen_host::alsa_backend::{
    encode_s24_3le, encode_s24_3le_block, AlsaOutputBackend, AlsaStats, PcmError, PcmFault,
    PcmOutput, BUFFER_PERIODS, BYTES_PER_SAMPLE, MAX_XRUN_RECOVERIES,
};
use khaen_host::backend::{AudioBackend, BackendError, DeviceConfig};
use khaen_host::DeviceId;

// ===========================================================================
// 假 PCM 设备：预分配 captured 缓冲，实时路径不分配；故障/短写/挂起都可注入。
// ===========================================================================

#[derive(Debug)]
struct FakePcm {
    config: Option<DeviceConfig>,
    /// 最近一块写进来的 S24_3LE 字节（`open` 时按块字节数预分配）。
    captured: Vec<u8>,
    /// captured 内当前写到的偏移；写满后下一块从头覆盖。
    offset: usize,
    open_count: u32,
    writes: u64,
    /// 每次 write 最多接受的帧数（模拟设备短写）。
    short_write_frames: Option<usize>,
    /// 永远返回 `Ok(0)`（模拟设备没有进展）。
    stall: bool,
    /// 返回超过请求的帧数（模拟设备/驱动报告错误）。
    over_report: bool,
    /// 下一次 write 直接返回的故障（消费一次）。
    next_fault: Option<PcmError>,
    /// 每次 write 都返回可恢复故障（模拟持续 xrun）。
    fault_every_write: bool,
    /// `fault_every_write` 时返回的故障类型。
    fault_kind: PcmFault,
    /// xrun 之后、prepare 之前再写会得到 Fatal（验证核心确实先 prepare 再重试）。
    poisoned: bool,
    prepares: u64,
    last_prepare_fault: Option<PcmFault>,
    prepare_fails: bool,
    available_frames: usize,
    available_fails: bool,
}

impl FakePcm {
    fn blank() -> Self {
        Self {
            config: None,
            captured: Vec::new(),
            offset: 0,
            open_count: 0,
            writes: 0,
            short_write_frames: None,
            stall: false,
            over_report: false,
            next_fault: None,
            fault_every_write: false,
            fault_kind: PcmFault::Underrun,
            poisoned: false,
            prepares: 0,
            last_prepare_fault: None,
            prepare_fails: false,
            available_frames: 0,
            available_fails: false,
        }
    }
}

impl PcmOutput for FakePcm {
    fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
        if config.device.as_str() == "fake-open-failure" {
            return Err(BackendError::Fault("fake open failed".to_string()));
        }
        Ok(Self {
            config: Some(config.clone()),
            captured: vec![0u8; config.samples_per_block() * BYTES_PER_SAMPLE],
            open_count: 1,
            available_frames: config.block_frames * BUFFER_PERIODS,
            ..Self::blank()
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
        if let Some(fault) = self.next_fault.take() {
            if matches!(fault, PcmError::Recoverable(_)) {
                self.poisoned = true;
            }
            return Err(fault);
        }
        if self.fault_every_write {
            self.poisoned = true;
            return Err(PcmError::Recoverable(self.fault_kind));
        }
        if self.poisoned {
            return Err(PcmError::Fatal("fake write before prepare"));
        }
        let bytes_per_frame = match self.config.as_ref() {
            Some(config) => config.channels * BYTES_PER_SAMPLE,
            None => return Err(PcmError::Fatal("fake not open")),
        };
        if self.stall {
            return Ok(0);
        }
        let requested_frames = bytes.len() / bytes_per_frame;
        if self.over_report {
            return Ok(requested_frames + 1);
        }
        if self.offset == self.captured.len() {
            // 上一块已经写满：假设备只保留最近一块，从头上覆盖。
            self.offset = 0;
        }
        let accepted_frames = match self.short_write_frames {
            Some(limit) => requested_frames.min(limit),
            None => requested_frames,
        };
        let accepted_bytes = accepted_frames * bytes_per_frame;
        let end = self.offset + accepted_bytes;
        assert!(end <= self.captured.len(), "fake: block write overflow");
        self.captured[self.offset..end].copy_from_slice(&bytes[..accepted_bytes]);
        self.offset = end;
        self.writes += 1;
        Ok(accepted_frames)
    }

    fn prepare(&mut self, fault: PcmFault) -> Result<(), BackendError> {
        self.prepares += 1;
        self.last_prepare_fault = Some(fault);
        if self.prepare_fails {
            return Err(BackendError::Fault("fake prepare failed".to_string()));
        }
        self.poisoned = false;
        Ok(())
    }

    fn available(&self) -> Result<usize, PcmError> {
        if self.available_fails {
            return Err(PcmError::Fatal("fake available failed"));
        }
        Ok(self.available_frames)
    }

    fn close(&mut self) {
        self.config = None;
    }
}

// ===========================================================================
// 测试工具
// ===========================================================================

fn config() -> DeviceConfig {
    DeviceConfig {
        device: DeviceId::new("fake-pcm"),
        sample_rate: 48_000,
        block_frames: 8,
        channels: 2,
    }
}

fn silent_block(config: &DeviceConfig) -> Vec<f32> {
    vec![0.0f32; config.samples_per_block()]
}

fn started(config: &DeviceConfig) -> AlsaOutputBackend<FakePcm> {
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    backend.start(config).expect("start fake backend");
    backend
}

/// 测试里对 fake 做一次性注入（正常路径不经过这里）。
fn device_mut(backend: &mut AlsaOutputBackend<FakePcm>) -> &mut FakePcm {
    backend.output_mut().expect("backend is started")
}

// ===========================================================================
// S24_3LE 编码
// ===========================================================================

#[test]
fn encodes_s24_3le_golden_vectors() {
    // 1 LSB = 2^-23。半值按"远离零"取整（无 dither，确定性编码）。
    const STEP: f32 = 1.0 / 8_388_608.0;
    const MID: f32 = 1_193_046.0 / 8_388_608.0;
    let cases: &[(f32, [u8; 3])] = &[
        (0.0, [0x00, 0x00, 0x00]),
        (-0.0, [0x00, 0x00, 0x00]),
        (STEP, [0x01, 0x00, 0x00]),
        (-STEP, [0xFF, 0xFF, 0xFF]),
        (0.25 * STEP, [0x00, 0x00, 0x00]),
        (0.5 * STEP, [0x01, 0x00, 0x00]),
        (0.75 * STEP, [0x01, 0x00, 0x00]),
        (-0.5 * STEP, [0xFF, 0xFF, 0xFF]),
        (2.0 * STEP, [0x02, 0x00, 0x00]),
        (-2.0 * STEP, [0xFE, 0xFF, 0xFF]),
        (0.25, [0x00, 0x00, 0x20]),
        (0.5, [0x00, 0x00, 0x40]),
        (-0.5, [0x00, 0x00, 0xC0]),
        (MID, [0x56, 0x34, 0x12]),
        (1.0 - f32::EPSILON, [0xFF, 0xFF, 0x7F]),
        (1.0, [0xFF, 0xFF, 0x7F]),
        (-1.0, [0x00, 0x00, 0x80]),
        (1.5, [0xFF, 0xFF, 0x7F]),
        (-1.5, [0x00, 0x00, 0x80]),
        (2.0, [0xFF, 0xFF, 0x7F]),
        (-2.0, [0x00, 0x00, 0x80]),
        (f32::MAX, [0xFF, 0xFF, 0x7F]),
        (f32::MIN, [0x00, 0x00, 0x80]),
        (f32::INFINITY, [0xFF, 0xFF, 0x7F]),
        (f32::NEG_INFINITY, [0x00, 0x00, 0x80]),
        (f32::MIN_POSITIVE, [0x00, 0x00, 0x00]),
        (f32::EPSILON, [0x01, 0x00, 0x00]),
    ];
    for &(sample, expected) in cases {
        assert_eq!(encode_s24_3le(sample), expected, "sample {sample:?}");
    }
    // NaN 无法映射成有效样本：输出 0，而不是把垃圾字节送到设备。
    assert_eq!(encode_s24_3le(f32::NAN), [0x00, 0x00, 0x00]);
}

#[test]
fn encodes_block_in_frame_interleaved_order() {
    const STEP: f32 = 1.0 / 8_388_608.0;
    // 2 声道 2 帧：L0=+1 步、R0=-1 步、L1=+0.5、R1=-0.5。
    let stereo = [STEP, -STEP, 0.5, -0.5];
    let mut bytes = [0u8; 12];
    encode_s24_3le_block(&stereo, &mut bytes).expect("encode");
    assert_eq!(
        bytes,
        [
            0x01, 0x00, 0x00, // L0
            0xFF, 0xFF, 0xFF, // R0
            0x00, 0x00, 0x40, // L1
            0x00, 0x00, 0xC0, // R1
        ]
    );
    // 4 声道一帧：按样本顺序展开，不重排通道（重排是图/路由层的职责）。
    let quad = [1.0, -1.0, 0.0, 0.25];
    let mut quad_bytes = [0u8; 12];
    encode_s24_3le_block(&quad, &mut quad_bytes).expect("encode");
    assert_eq!(
        quad_bytes,
        [
            0xFF, 0xFF, 0x7F, // +1.0
            0x00, 0x00, 0x80, // -1.0
            0x00, 0x00, 0x00, // 0.0
            0x00, 0x00, 0x20, // +0.25
        ]
    );
    // 字节缓冲长度不符会拒绝，不写半个样本。
    assert_eq!(
        encode_s24_3le_block(&[0.0, 1.0], &mut [0u8; 5]).expect_err("wrong bytes"),
        BackendError::WrongBlockLength {
            expected: 6,
            actual: 5
        }
    );
}

// ===========================================================================
// 打开 / 配置
// ===========================================================================

#[test]
fn start_opens_device_and_allocates_block_buffer() {
    let config = config();
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    assert!(!backend.is_started());
    backend.start(&config).expect("start");
    assert!(backend.is_started());
    assert_eq!(backend.config(), Some(&config));
    assert_eq!(
        backend.block_bytes(),
        config.samples_per_block() * BYTES_PER_SAMPLE
    );
    assert_eq!(
        backend.available_frames().expect("available"),
        BUFFER_PERIODS * config.block_frames
    );
    assert_eq!(backend.stats(), AlsaStats::default());
    let fake = backend.output().expect("output");
    assert_eq!(fake.open_count, 1);
    assert_eq!(fake.config.as_ref(), Some(&config));
    assert_eq!(fake.captured.len(), backend.block_bytes());
}

#[test]
fn start_rejects_bad_config() {
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    let mut empty_device = config();
    empty_device.device = DeviceId::new("");
    assert_eq!(
        backend.start(&empty_device).unwrap_err(),
        BackendError::BadConfig("device name must not be empty")
    );
    let mut nul_device = config();
    nul_device.device = DeviceId::new("hw:bad\0name");
    assert_eq!(
        backend.start(&nul_device).unwrap_err(),
        BackendError::BadConfig("device name must not contain NUL")
    );
    let mut zero_rate = config();
    zero_rate.sample_rate = 0;
    assert_eq!(
        backend.start(&zero_rate).unwrap_err(),
        BackendError::BadConfig("sample_rate must be positive")
    );
    let mut zero_block = config();
    zero_block.block_frames = 0;
    assert_eq!(
        backend.start(&zero_block).unwrap_err(),
        BackendError::BadConfig("block_frames must be positive")
    );
    let mut zero_channels = config();
    zero_channels.channels = 0;
    assert_eq!(
        backend.start(&zero_channels).unwrap_err(),
        BackendError::BadConfig("channels must be positive")
    );
    let mut overflow = config();
    overflow.block_frames = usize::MAX / 2;
    overflow.channels = 3;
    assert_eq!(
        backend.start(&overflow).unwrap_err(),
        BackendError::BadConfig("block size overflow")
    );
    // 所有失败都不应留下半启动状态。
    assert!(!backend.is_started());
    assert_eq!(backend.block_bytes(), 0);
    assert_eq!(backend.stats(), AlsaStats::default());
}

#[test]
fn start_maps_open_failure_without_state_leak() {
    let mut bad_device = config();
    bad_device.device = DeviceId::new("fake-open-failure");
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    assert_eq!(
        backend.start(&bad_device).unwrap_err(),
        BackendError::Fault("fake open failed".to_string())
    );
    assert!(!backend.is_started());
    assert_eq!(backend.config(), None);
    assert_eq!(backend.block_bytes(), 0);
    assert_eq!(backend.stats(), AlsaStats::default());
}

// ===========================================================================
// 播放：编码、短写、统计
// ===========================================================================

#[test]
fn playout_writes_converted_block_and_updates_stats() {
    let config = config();
    let mut backend = started(&config);
    let mut samples = silent_block(&config);
    samples[0] = 0.5;
    samples[1] = -0.5;
    samples[2] = 1.0;

    backend.playout(&samples).expect("playout");

    let mut expected = vec![0u8; config.samples_per_block() * BYTES_PER_SAMPLE];
    encode_s24_3le_block(&samples, &mut expected).expect("encode");
    let stats = backend.stats();
    assert_eq!(stats.blocks_written, 1);
    assert_eq!(stats.bytes_written, expected.len() as u64);
    assert_eq!(stats.xruns, 0);
    assert_eq!(stats.last_error, None);
    let fake = backend.output().expect("output");
    assert_eq!(fake.writes, 1);
    assert_eq!(fake.offset, expected.len());
    assert_eq!(fake.captured, expected);
    // 独立于编码函数抽查字节序（+0.5 / -0.5）。
    assert_eq!(&fake.captured[..3], &[0x00, 0x00, 0x40]);
    assert_eq!(&fake.captured[3..6], &[0x00, 0x00, 0xC0]);
}

#[test]
fn playout_requires_start_and_full_block() {
    let config = config();
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    let samples = silent_block(&config);
    assert_eq!(
        backend.playout(&samples).unwrap_err(),
        BackendError::NotStarted
    );
    backend.start(&config).expect("start");
    assert_eq!(
        backend.playout(&samples[..3]).unwrap_err(),
        BackendError::WrongBlockLength {
            expected: config.samples_per_block(),
            actual: 3
        }
    );
    // 长度错误不算一块：统计保持清零。
    assert_eq!(backend.stats(), AlsaStats::default());
}

#[test]
fn playout_completes_block_across_short_writes() {
    let config = config();
    let mut backend = started(&config);
    device_mut(&mut backend).short_write_frames = Some(3);
    let mut samples = silent_block(&config);
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = (index % 8) as f32 / 16.0 - 0.25;
    }

    backend.playout(&samples).expect("playout");

    let mut expected = vec![0u8; config.samples_per_block() * BYTES_PER_SAMPLE];
    encode_s24_3le_block(&samples, &mut expected).expect("encode");
    let fake = backend.output().expect("output");
    // 8 帧、每次最多 3 帧 → 3 次写：3 + 3 + 2。
    assert_eq!(fake.writes, 3);
    assert_eq!(fake.captured, expected);
    let stats = backend.stats();
    assert_eq!(stats.blocks_written, 1);
    assert_eq!(stats.bytes_written, expected.len() as u64);
    assert_eq!(stats.last_error, None);
}

#[test]
fn handles_motu_ultralite_topology() {
    // 目标设备是 MOTU UltraLite-mk5：48 kHz、22 通道、128 帧块（T70a 的真实拓扑）。
    let config = DeviceConfig {
        device: DeviceId::new("hw:CARD=UltraLite-mk5,DEV=0"),
        sample_rate: 48_000,
        block_frames: 128,
        channels: 22,
    };
    let mut backend = started(&config);
    let mut samples = vec![0.0f32; config.samples_per_block()];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = ((index % 201) as f32 / 201.0) - 0.5;
    }
    backend.playout(&samples).expect("playout");

    let expected_len = 128 * 22 * BYTES_PER_SAMPLE;
    assert_eq!(expected_len, 8_448);
    assert_eq!(backend.block_bytes(), expected_len);
    let mut expected = vec![0u8; expected_len];
    encode_s24_3le_block(&samples, &mut expected).expect("encode");
    let fake = backend.output().expect("output");
    assert_eq!(fake.captured, expected);
    assert_eq!(backend.stats().bytes_written, expected_len as u64);
}

// ===========================================================================
// xrun / 故障恢复
// ===========================================================================

#[test]
fn underrun_is_prepared_and_retried() {
    let config = config();
    let mut backend = started(&config);
    let samples = silent_block(&config);
    backend.playout(&samples).expect("first block");
    device_mut(&mut backend).next_fault = Some(PcmError::Recoverable(PcmFault::Underrun));

    backend.playout(&samples).expect("second block after xrun");

    let stats = backend.stats();
    assert_eq!(stats.blocks_written, 2);
    assert_eq!(stats.xruns, 1);
    assert_eq!(stats.bytes_written, 2 * backend.block_bytes() as u64);
    assert_eq!(stats.last_error, Some(PcmFault::Underrun.as_str()));
    let fake = backend.output().expect("output");
    assert_eq!(fake.prepares, 1);
    assert_eq!(fake.last_prepare_fault, Some(PcmFault::Underrun));
    assert_eq!(fake.writes, 2);
    assert!(!fake.poisoned);
}

#[test]
fn suspend_is_prepared_and_retried() {
    let config = config();
    let mut backend = started(&config);
    device_mut(&mut backend).next_fault = Some(PcmError::Recoverable(PcmFault::Suspended));

    backend
        .playout(&silent_block(&config))
        .expect("block after suspend");

    let fake = backend.output().expect("output");
    assert_eq!(backend.stats().xruns, 1);
    assert_eq!(fake.prepares, 1);
    assert_eq!(fake.last_prepare_fault, Some(PcmFault::Suspended));
    assert!(!fake.poisoned);
}

#[test]
fn xrun_recovery_is_bounded() {
    let config = config();
    let mut backend = started(&config);
    {
        let fake = device_mut(&mut backend);
        fake.fault_every_write = true;
        fake.fault_kind = PcmFault::Underrun;
    }
    let error = backend.playout(&silent_block(&config)).expect_err("xrun");
    assert_eq!(
        error,
        BackendError::Fault("xrun recovery limit reached".to_string())
    );
    let stats = backend.stats();
    assert_eq!(stats.xruns, u64::from(MAX_XRUN_RECOVERIES) + 1);
    assert_eq!(stats.blocks_written, 0);
    assert_eq!(stats.bytes_written, 0);
    assert_eq!(stats.last_error, Some(PcmFault::Underrun.as_str()));
    let fake = backend.output().expect("output");
    assert_eq!(fake.prepares, u64::from(MAX_XRUN_RECOVERIES));
    assert_eq!(fake.writes, 0);
}

#[test]
fn prepare_failure_is_reported() {
    let config = config();
    let mut backend = started(&config);
    {
        let fake = device_mut(&mut backend);
        fake.next_fault = Some(PcmError::Recoverable(PcmFault::Underrun));
        fake.prepare_fails = true;
    }
    let error = backend
        .playout(&silent_block(&config))
        .expect_err("prepare fails");
    assert_eq!(
        error,
        BackendError::Fault("fake prepare failed".to_string())
    );
    let stats = backend.stats();
    assert_eq!(stats.xruns, 1);
    assert_eq!(stats.blocks_written, 0);
    assert_eq!(stats.last_error, Some("xrun prepare failed"));
    assert!(backend.output().expect("output").poisoned);
}

#[test]
fn fatal_write_error_is_mapped() {
    let config = config();
    let mut backend = started(&config);
    device_mut(&mut backend).next_fault = Some(PcmError::Fatal("fake device gone"));
    let error = backend.playout(&silent_block(&config)).expect_err("fatal");
    assert_eq!(error, BackendError::Fault("fake device gone".to_string()));
    let stats = backend.stats();
    assert_eq!(stats.blocks_written, 0);
    assert_eq!(stats.bytes_written, 0);
    assert_eq!(stats.last_error, Some("fake device gone"));
}

#[test]
fn stalled_write_is_reported_instead_of_looping() {
    let config = config();
    let mut backend = started(&config);
    device_mut(&mut backend).stall = true;
    let error = backend.playout(&silent_block(&config)).expect_err("stall");
    assert_eq!(
        error,
        BackendError::Fault("pcm write made no progress".to_string())
    );
    assert_eq!(
        backend.stats().last_error,
        Some("pcm write made no progress")
    );
    assert_eq!(backend.stats().blocks_written, 0);
}

#[test]
fn over_reported_write_is_rejected() {
    let config = config();
    let mut backend = started(&config);
    device_mut(&mut backend).over_report = true;
    let error = backend
        .playout(&silent_block(&config))
        .expect_err("over report");
    assert_eq!(
        error,
        BackendError::Fault("pcm write over-reported frames".to_string())
    );
    assert_eq!(
        backend.stats().last_error,
        Some("pcm write over-reported frames")
    );
    assert_eq!(backend.stats().blocks_written, 0);
}

// ===========================================================================
// 采集（输出型后端：没有来源，填静音）
// ===========================================================================

#[test]
fn capture_fills_silence_and_validates_length() {
    let config = config();
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    let mut out = vec![0.0f32; config.samples_per_block()];
    assert_eq!(
        backend.capture(&mut out).unwrap_err(),
        BackendError::NotStarted
    );
    backend.start(&config).expect("start");
    out.fill(1.0);
    backend.capture(&mut out).expect("capture");
    assert!(
        out.iter().all(|sample| sample.to_bits() == 0),
        "capture must fill +0.0"
    );
    assert_eq!(
        backend.capture(&mut out[..3]).unwrap_err(),
        BackendError::WrongBlockLength {
            expected: config.samples_per_block(),
            actual: 3
        }
    );
    // 采集不改变播放统计。
    assert_eq!(backend.stats(), AlsaStats::default());
}

// ===========================================================================
// 停止 / 重启 / 诊断
// ===========================================================================

#[test]
fn stop_is_idempotent_and_restart_resets_stats() {
    let config = config();
    let mut backend = started(&config);
    let samples = silent_block(&config);
    backend.playout(&samples).expect("playout");

    backend.stop().expect("stop");
    assert!(!backend.is_started());
    assert_eq!(backend.config(), None);
    assert!(backend.output().is_none());
    assert_eq!(backend.block_bytes(), 0);
    // 统计保留到下一次 start，便于 stop 之后读最后一轮状态。
    assert_eq!(backend.stats().blocks_written, 1);

    let mut out = silent_block(&config);
    assert_eq!(
        backend.playout(&samples).unwrap_err(),
        BackendError::NotStarted
    );
    assert_eq!(
        backend.capture(&mut out).unwrap_err(),
        BackendError::NotStarted
    );
    assert_eq!(
        backend.available_frames().unwrap_err(),
        BackendError::NotStarted
    );
    backend.stop().expect("stop is idempotent");

    backend.start(&config).expect("restart");
    assert_eq!(backend.stats(), AlsaStats::default());
    assert_eq!(backend.output().expect("output").open_count, 1);
    backend.playout(&samples).expect("playout after restart");
    assert_eq!(backend.stats().blocks_written, 1);

    // 运行中再次 start 等价于停止后重新打开，统计同样清零。
    backend.start(&config).expect("restart while running");
    assert_eq!(backend.stats(), AlsaStats::default());
    assert!(backend.is_started());
}

#[test]
fn available_frames_reports_device_capacity() {
    let config = config();
    let mut backend = AlsaOutputBackend::<FakePcm>::new();
    assert_eq!(
        backend.available_frames().unwrap_err(),
        BackendError::NotStarted
    );
    backend.start(&config).expect("start");
    assert_eq!(
        backend.available_frames().expect("available"),
        BUFFER_PERIODS * config.block_frames
    );
    device_mut(&mut backend).available_fails = true;
    assert_eq!(
        backend.available_frames().unwrap_err(),
        BackendError::Fault("fake available failed".to_string())
    );
}

// ===========================================================================
// 实时路径零分配（子进程计量）
// ===========================================================================

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: 只是把调用转发给 System 分配器并顺手计数；不改变分配语义、不释放非法指针。
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: 参数来自全局分配器协议，原样转发给 System。
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: 同上。
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: 同上。
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 同上。
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const ZERO_ALLOC_CHILD_ENV: &str = "KHAEN_ALSA_ZERO_ALLOC_CHILD";
const ZERO_ALLOC_TEST_NAME: &str = "playout_performs_no_heap_allocation";

/// 父进程：本测试二进制里其它测试线程会并发分配，全局计数不干净；改为只跑本用例的子进程计量。
fn run_zero_alloc_child() {
    let exe = std::env::current_exe().expect("current test executable");
    let output = Command::new(exe)
        .args([
            "--exact",
            ZERO_ALLOC_TEST_NAME,
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ZERO_ALLOC_CHILD_ENV, "1")
        .output()
        .expect("spawn zero-alloc child test process");
    assert!(
        output.status.success(),
        "zero-alloc child failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn playout_performs_no_heap_allocation() {
    if std::env::var_os(ZERO_ALLOC_CHILD_ENV).is_none() {
        run_zero_alloc_child();
        return;
    }

    // 子进程里只跑这一个测试：构造期分配（设备/缓冲）在计量窗口之外完成。
    let config = config();
    let mut backend = started(&config);
    let mut input = silent_block(&config);
    let mut captured = silent_block(&config);
    for index in 0..32u32 {
        input.fill(index as f32 * 1.0e-4);
        backend.capture(&mut captured).expect("warmup capture");
        backend.playout(&input).expect("warmup playout");
    }

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for index in 0..256u32 {
        input.fill(index as f32 * 1.0e-4);
        backend.capture(&mut captured).expect("capture");
        backend.playout(&input).expect("playout");
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocated, 0, "capture/playout allocated {allocated} times");
    assert_eq!(backend.stats().blocks_written, 32 + 256);
}

// ===========================================================================
// Linux 适配层（本机 Windows 为 NOT_RUN，由 CI 覆盖）
// ===========================================================================

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_reports_missing_device_without_panicking() {
    // 只验证 Linux 适配层把"设备不存在"映射成 BackendError::Fault（不需要真实声卡）。
    use khaen_host::alsa_backend::LinuxAlsaBackend;

    let config = DeviceConfig {
        device: DeviceId::new("hw:CARD=KhaenNoSuchCard,DEV=0"),
        sample_rate: 48_000,
        block_frames: 128,
        channels: 22,
    };
    let mut backend = LinuxAlsaBackend::new();
    let error = backend
        .start(&config)
        .expect_err("missing device must fail");
    assert!(
        matches!(error, BackendError::Fault(_)),
        "unexpected error: {error:?}"
    );
    assert!(!backend.is_started());
}
