//! 测试夹具：假块线程（真 SlotEngine）、内存双工、假 PCM/包源、方案构造与临时文件。
//!
//! 这些夹具只在 cfg(test) 下编译；协议核心/传输/桥的测试共用。

#![allow(dead_code)]

use std::io::{self, BufRead, Cursor, Read, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use khaen_host::aes67_input::Aes67InputConfig;
use khaen_host::alsa_backend::{PcmError, PcmFault, PcmOutput, BYTES_PER_SAMPLE};
use khaen_host::backend::DeviceConfig;
use khaen_host::control::RuntimeMode;
use khaen_host::engine::{EngineConfig, GraphInfo, MuteReceipt, Passthrough, SlotEngine};
use khaen_host::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_host::runtime::{
    PacketSource, PlanDraft, ReceiverConfig, Runtime, RuntimeConfig, RuntimeStatus,
    SupervisorOptions, UnloadedPolicy,
};
use khaen_host::safety::RuntimeState;
use khaen_host::supervisor::{LifecycleState, SupervisorStatus};
use khaen_host::{DeviceId, SafetyPolicy};
use serde_json::Value;

use crate::control::bridge::capture_snapshot;
use crate::control::command::{execute_command, ControlCommand, ControlTarget};
use crate::control::core::{ControlCore, ControlCoreConfig};
use crate::control::snapshot::SharedSnapshot;
use crate::control::transport::serve_connection;
use crate::recovery::DeviceLifecycle;

/// 测试采样率。
pub(crate) const SAMPLE_RATE: u32 = 48_000;
/// 测试块长（帧）。
pub(crate) const BLOCK_FRAMES: usize = 128;
/// 测试通道数。
pub(crate) const CHANNELS: usize = 2;

/// 测试用执行器格式（48 kHz / 128 帧 / 2 通道）。
pub(crate) fn test_config() -> EngineConfig {
    EngineConfig {
        block_frames: BLOCK_FRAMES,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
    }
}

// ===========================================================================
// 方案构造
// ===========================================================================

/// 逐 lane 的恒等方案（2 通道，可指定 protection 增益）。
///
/// 图执行器的 identity 判据（content 单 fold、crossover [1.0]、矩阵恒等、protection 0 dB、
/// routing lane i -> dac i）保证它逐位直通；改 lane 0 的 gain_db 后输出按该增益缩放。
pub(crate) fn identity_plan_with_gain(gain_db: f32) -> Plan {
    let lanes: u16 = CHANNELS as u16;
    let folds: Vec<Fold> = (0..lanes)
        .map(|lane| Fold {
            src: lane,
            dst: lane,
            gain: 1.0,
        })
        .collect();
    let filters: Vec<CrossoverFilter> = (0..lanes)
        .map(|lane| CrossoverFilter {
            lane,
            coeffs: vec![1.0],
        })
        .collect();
    let mut matrix_coeffs = vec![0.0f32; usize::from(lanes) * usize::from(lanes)];
    for lane in 0..usize::from(lanes) {
        matrix_coeffs[lane * usize::from(lanes) + lane] = 1.0;
    }
    let entries: Vec<ProtectionEntry> = (0..lanes)
        .map(|lane| ProtectionEntry {
            lane,
            delay: 0,
            polarity: 1,
            gain_db: if lane == 0 { gain_db } else { 0.0 },
            peq: Vec::new(),
        })
        .collect();
    Plan {
        block_size: BLOCK_FRAMES as u32,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: lanes,
            out_lanes: lanes,
            folds,
        }),
        crossover: Some(CrossoverSection { lanes, filters }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection { lanes, entries }),
        routing: Some(RoutingSection {
            dac_channels: lanes,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

/// 2 通道恒等方案（lane gain 0 dB）。
pub(crate) fn identity_plan() -> Plan {
    identity_plan_with_gain(0.0)
}

/// 变体方案：只改 protection lane 0 增益（用于验证两次 commit 的 graph_id 不同）。
pub(crate) fn variant_plan() -> Plan {
    identity_plan_with_gain(-3.0)
}

/// 方案编码成 ABI v3 字节（规范形式）。
pub(crate) fn encode_plan(plan: &Plan) -> Vec<u8> {
    plan.encode_checked().expect("encode plan")
}

/// 指定 lane 数的恒等方案（用于准入拒绝：33 lane 超默认上限 32）。
pub(crate) fn wide_plan(lanes: u16) -> Plan {
    let folds: Vec<Fold> = (0..lanes)
        .map(|lane| Fold {
            src: lane,
            dst: lane,
            gain: 1.0,
        })
        .collect();
    let filters: Vec<CrossoverFilter> = (0..lanes)
        .map(|lane| CrossoverFilter {
            lane,
            coeffs: vec![1.0],
        })
        .collect();
    let mut matrix_coeffs = vec![0.0f32; usize::from(lanes) * usize::from(lanes)];
    for lane in 0..usize::from(lanes) {
        matrix_coeffs[lane * usize::from(lanes) + lane] = 1.0;
    }
    let entries: Vec<ProtectionEntry> = (0..lanes)
        .map(|lane| ProtectionEntry {
            lane,
            delay: 0,
            polarity: 1,
            gain_db: 0.0,
            peq: Vec::new(),
        })
        .collect();
    Plan {
        block_size: BLOCK_FRAMES as u32,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: lanes,
            out_lanes: lanes,
            folds,
        }),
        crossover: Some(CrossoverSection { lanes, filters }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection { lanes, entries }),
        routing: Some(RoutingSection {
            dac_channels: lanes,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

/// 临时方案文件（Drop 时删除；文件名带进程号 + 原子计数，测试并行安全）。
pub(crate) struct TempPlan {
    path: PathBuf,
}

impl TempPlan {
    pub(crate) fn new(tag: &str, bytes: &[u8]) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "khaen-rt-host-control-{}-{tag}-{unique}.plan",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("write temp plan");
        Self { path }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn path_text(&self) -> String {
        self.path.display().to_string()
    }

    /// 文件字节的 sha256（十六进制）。
    pub(crate) fn sha256(&self) -> String {
        let bytes = std::fs::read(&self.path).expect("read temp plan");
        khaen_host::sha256_hex(&bytes)
    }
}

impl Drop for TempPlan {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ===========================================================================
// 假块线程：真 SlotEngine（块边界语义与 Runtime 的引擎一致）
// ===========================================================================

/// 假执行器：用真 SlotEngine 提供块边界语义（静音/换图/块号），RuntimeStatus 是合成值。
pub(crate) struct FakeTarget {
    engine: SlotEngine,
    config: EngineConfig,
    output: Vec<f32>,
}

impl FakeTarget {
    pub(crate) fn new(config: EngineConfig) -> Self {
        Self {
            engine: SlotEngine::new(config.clone(), Box::new(Passthrough)).expect("fake engine"),
            config,
            output: Vec::new(),
        }
    }

    /// 跑一块静音输入。
    pub(crate) fn run_block(&mut self) {
        let input = vec![0.0f32; self.engine.samples_per_block()];
        self.run_block_input(&input);
    }

    /// 跑一块指定输入，保存输出供断言。
    pub(crate) fn run_block_input(&mut self, input: &[f32]) {
        let processed = self.engine.run_block(input).expect("fake engine block");
        self.output.clear();
        self.output.extend_from_slice(processed);
    }

    /// 最近一块输出（帧交错）。
    pub(crate) fn last_output(&self) -> &[f32] {
        &self.output
    }

    /// 当前生效图信息（来自真 SlotEngine）。
    pub(crate) fn graph_info(&self) -> GraphInfo {
        self.engine.graph_info()
    }

    /// 引擎统计（块号/静音）。
    pub(crate) fn stats(&self) -> khaen_host::engine::EngineStats {
        self.engine.stats()
    }
}

impl ControlTarget for FakeTarget {
    fn data_plane_format(&self) -> EngineConfig {
        self.config.clone()
    }

    fn blocks(&self) -> u64 {
        self.engine.stats().blocks
    }

    fn request_mute(&mut self, mute: bool) -> MuteReceipt {
        self.engine.request_mute(mute)
    }

    fn stage_plan_draft(&mut self, draft: PlanDraft) -> u64 {
        let (version, graph_id, processor) = draft.into_parts();
        self.engine.stage_processor(processor, version, graph_id)
    }

    fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            state: if self.engine.stats().mute {
                RuntimeState::Muted
            } else {
                RuntimeState::Running
            },
            blocks: self.engine.stats().blocks,
            input: khaen_host::aes67_input::Aes67InputStats::default(),
            output: khaen_host::alsa_backend::AlsaStats::default(),
            receiver: khaen_host::runtime::ReceiverStats::default(),
            device: SupervisorStatus {
                state: LifecycleState::Absent,
                plan_rate: self.config.sample_rate,
                device_rate: None,
                rate_asserted: false,
                last_error: None,
                attempts: 0,
                since_ms: 0,
            },
            device_rate: None,
            plan_rate: self.config.sample_rate,
            rate_asserted: false,
            output_errors: 0,
            output_started: true,
            graph: self.engine.graph_info(),
            switch_pending: self.engine.switch_pending(),
            muted: self.engine.stats().mute,
            last_error: None,
        }
    }

    fn total_faults(&self) -> u64 {
        0
    }
}

// ===========================================================================
// Harness：核心 + 假块线程 + 快照 + 设备生命周期
// ===========================================================================

/// 一套控制通道夹具：ControlCore 经真 mpsc 连到一个假块线程（真 SlotEngine）。
pub(crate) struct Harness {
    core: Arc<Mutex<ControlCore>>,
    target: Arc<Mutex<FakeTarget>>,
    snapshot: SharedSnapshot,
    lifecycle: DeviceLifecycle,
    stop: Arc<AtomicBool>,
    loop_handle: Option<thread::JoinHandle<()>>,
}

impl Harness {
    /// 默认参数（2 s 命令超时，48 kHz/128/2）。
    pub(crate) fn new() -> Self {
        Self::with_timeout(Duration::from_secs(2))
    }

    /// 指定命令超时（超时语义测试用）。
    pub(crate) fn with_timeout(command_timeout: Duration) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let target = Arc::new(Mutex::new(FakeTarget::new(test_config())));
        let snapshot: SharedSnapshot = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let loop_target = Arc::clone(&target);
        let loop_stop = Arc::clone(&stop);
        let loop_handle = thread::spawn(move || loop {
            match command_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(command) => {
                    let mut guard = loop_target
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    execute_command(command, &mut *guard);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if loop_stop.load(Ordering::Acquire) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        });
        let core = ControlCore::new(ControlCoreConfig {
            data_plane_format: test_config(),
            limits: khaen_host::AdmissionLimits::default(),
            commands: command_tx,
            snapshot: Arc::clone(&snapshot),
            mode: RuntimeMode::Aes67,
            initial_plan: None,
            initial_version: 0,
            command_timeout,
        });
        Self {
            core: Arc::new(Mutex::new(core)),
            target,
            snapshot,
            lifecycle: DeviceLifecycle::new(),
            stop,
            loop_handle: Some(loop_handle),
        }
    }

    /// 共享的协议核心（服务端/多连接测试用）。
    pub(crate) fn core(&self) -> Arc<Mutex<ControlCore>> {
        Arc::clone(&self.core)
    }

    /// 发一行请求，解析成 JSON。
    pub(crate) fn call(&mut self, request: &str) -> Value {
        let response = self.call_raw(request);
        serde_json::from_str(&response)
            .unwrap_or_else(|_| panic!("response is not JSON: {response}"))
    }

    /// 发一行请求，返回原始响应字符串。
    pub(crate) fn call_raw(&mut self, request: &str) -> String {
        let mut guard = self
            .core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.handle_line(request)
    }

    /// 用字符串请求对象（serde_json::json!）调用。
    pub(crate) fn call_json(&mut self, request: &Value) -> Value {
        self.call(&request.to_string())
    }

    /// 跑一块静音输入。
    pub(crate) fn run_block(&mut self) {
        let mut target = self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.run_block();
    }

    /// 跑一块指定输入。
    pub(crate) fn run_block_input(&mut self, input: &[f32]) {
        let mut target = self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.run_block_input(input);
    }

    /// 最近一块输出。
    pub(crate) fn last_output(&mut self) -> Vec<f32> {
        let target = self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.last_output().to_vec()
    }

    /// 目标引擎统计。
    pub(crate) fn engine_stats(&mut self) -> khaen_host::engine::EngineStats {
        let target = self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.stats()
    }

    /// 目标图信息。
    pub(crate) fn graph_info(&mut self) -> GraphInfo {
        let target = self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.graph_info()
    }

    /// 发布一份新的状态快照。
    pub(crate) fn publish(&mut self) {
        let value = {
            let target = self
                .target
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            capture_snapshot(&*target, &self.lifecycle, None)
        };
        *self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
    }

    /// 发布一份"采集于 age 之前"的快照（age/stale 语义测试用）。
    pub(crate) fn publish_with_age(&mut self, age: Duration) {
        let mut value = {
            let target = self
                .target
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            capture_snapshot(&*target, &self.lifecycle, Some(100))
        };
        value.captured_at = Instant::now()
            .checked_sub(age)
            .expect("instant subtraction");
        *self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
    }

    /// 跑 n 块并发布快照。
    pub(crate) fn run_blocks(&mut self, count: u64) {
        for _ in 0..count {
            self.run_block();
        }
        self.publish();
    }

    /// 设备生命周期（可直接改，如 note_absent）。
    pub(crate) fn lifecycle_mut(&mut self) -> &mut DeviceLifecycle {
        &mut self.lifecycle
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // 停止假块线程：它按 20 ms 超时轮询停止标志，不会因为命令通道仍被核心持有而挂住。
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.loop_handle.take() {
            let _ = handle.join();
        }
    }
}

// ===========================================================================
// 内存双工（平台无关的传输测试）
// ===========================================================================

/// 内存读端（BufRead）。
pub(crate) struct MemoryReader {
    data: Cursor<Vec<u8>>,
}

impl MemoryReader {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self {
            data: Cursor::new(bytes),
        }
    }
}

impl Read for MemoryReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.data.read(buf)
    }
}

impl BufRead for MemoryReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.data.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.data.consume(amount);
    }
}

/// 内存写端。
#[derive(Default)]
pub(crate) struct MemoryWriter {
    pub(crate) out: Vec<u8>,
}

impl Write for MemoryWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 在内存双工上跑一遍协议，返回每条响应（已按行切分）。
pub(crate) fn serve_in_memory(core: &Mutex<ControlCore>, input: &[u8]) -> Vec<String> {
    let mut reader = MemoryReader::new(input.to_vec());
    let mut writer = MemoryWriter::default();
    serve_connection(&mut reader, &mut writer, core).expect("in-memory serving");
    String::from_utf8(writer.out)
        .expect("responses are utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

// ===========================================================================
// 假 PCM / 假包源（ControlTarget for Runtime 适配层测试）
// ===========================================================================

/// 永远成功的假 PCM 设备（块字节数按配置校验，写计数只在本测试里断言"有写"）。
pub(crate) struct FakePcm {
    config: DeviceConfig,
}

impl PcmOutput for FakePcm {
    fn open(config: &DeviceConfig) -> Result<Self, khaen_host::backend::BackendError> {
        Ok(Self {
            config: config.clone(),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
        let expected = self.config.samples_per_block() * BYTES_PER_SAMPLE;
        assert_eq!(bytes.len(), expected, "fake pcm block bytes");
        Ok(self.config.block_frames)
    }

    fn prepare(&mut self, _fault: PcmFault) -> Result<(), khaen_host::backend::BackendError> {
        Ok(())
    }

    fn available(&self) -> Result<usize, PcmError> {
        Ok(0)
    }

    fn close(&mut self) {}
}

/// 空闲假包源：没有 RTP 包，收包线程按有限时间返回 WouldBlock（可 join）。
pub(crate) struct IdleSource;

impl PacketSource for IdleSource {
    fn recv(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_micros(200));
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
}

/// 构造一个设备已打开（假 PCM）、无输入的 Runtime（用于 ControlTarget 适配层测试）。
pub(crate) fn fake_runtime() -> Runtime<FakePcm> {
    let config = RuntimeConfig {
        input: Aes67InputConfig {
            multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
            port: 5004,
            payload_type: 96,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            channels_map: vec![0, 1],
            output_channels: CHANNELS,
            block_frames: BLOCK_FRAMES,
            jitter_target_frames: 0,
        },
        engine: test_config(),
        device: DeviceConfig {
            device: DeviceId::new("khaen-rt-host-control-fake-pcm"),
            sample_rate: SAMPLE_RATE,
            block_frames: BLOCK_FRAMES,
            channels: CHANNELS,
        },
        safety: SafetyPolicy {
            max_consecutive_underruns: 10_000,
            max_consecutive_overruns: 10_000,
        },
        receiver: ReceiverConfig::default(),
        supervisor: SupervisorOptions::default(),
        unloaded_policy: UnloadedPolicy::Passthrough,
    };
    Runtime::<FakePcm>::with_source(config, IdleSource).expect("fake runtime")
}

/// 一个空的命令接收端（只构造 ControlCore、不跑假块线程时用；发送命令会超时/无回执）。
pub(crate) fn idle_command_channel() -> (mpsc::Sender<ControlCommand>, Receiver<ControlCommand>) {
    mpsc::channel()
}
