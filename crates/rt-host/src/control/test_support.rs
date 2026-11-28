//! 测试夹具：假块线程（真 SlotEngine）、内存双工、假 PCM/包源、方案构造与临时文件。
//!
//! 这些夹具只在 cfg(test) 下编译；协议核心/传输/桥的测试共用。

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{self, BufRead, Cursor, Read, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    CalibrationPlayback, HwReadback, PacketSource, PlanDraft, ReceiverConfig, ReconfigureFailure,
    ReconfigureOutcome, Runtime, RuntimeConfig, RuntimeStatus, SupervisorOptions, UnloadedPolicy,
};
use khaen_host::safety::RuntimeState;
use khaen_host::supervisor::{LifecycleState, SupervisorStatus};
use khaen_host::{DeviceId, SafetyPolicy};
use serde_json::Value;

use crate::control::bridge::capture_snapshot;
use crate::control::cal::CalibrationSetup;
use crate::control::command::{
    execute_command, CommandContext, ControlCommand, ControlTarget, ReconfigureDone,
    ReconfigureRequest,
};
use crate::control::core::{ControlCore, ControlCoreConfig};
use crate::control::reconfigure::{
    shared_tracker, ChannelsConfig, ChannelsView, DeviceProbe, SharedReconfigure,
};
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
        input_channels: CHANNELS,
        output_channels: CHANNELS,
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
    /// 用户静音请求（与引擎实际静音分开记录，热重配前后要恢复）。
    pub(crate) user_mute: bool,
    /// 收到的热重配请求数。
    pub(crate) reconfigure_calls: u64,
    /// 接下来这些热重配请求要失败（FIFO；测试注入回滚路径）。
    pub(crate) reconfigure_failures: VecDeque<ReconfigureFailure>,
    /// 最近一次热重配的目标视图。
    pub(crate) last_reconfigure_target: Option<ChannelsView>,
    /// 引擎重建前的累计块数（热重配换引擎后块计数保持连续，与真实 Runtime 同语义）。
    pub(crate) blocks_offset: u64,
}

impl FakeTarget {
    pub(crate) fn new(config: EngineConfig) -> Self {
        Self {
            engine: SlotEngine::new(config.clone(), Box::new(Passthrough)).expect("fake engine"),
            config,
            output: Vec::new(),
            user_mute: false,
            reconfigure_calls: 0,
            reconfigure_failures: VecDeque::new(),
            last_reconfigure_target: None,
            blocks_offset: 0,
        }
    }

    /// 连续块计数（引擎重建前后累计，与真实 Runtime 的 blocks 语义一致）。
    pub(crate) fn blocks_total(&self) -> u64 {
        self.engine
            .stats()
            .blocks
            .saturating_add(self.blocks_offset)
    }

    /// 注入一次热重配失败（下一次 reconfigure 返回它）。
    pub(crate) fn fail_next_reconfigure(&mut self, failure: ReconfigureFailure) {
        self.reconfigure_failures.push_back(failure);
    }

    /// 当前运行格式（与 FakeTarget 的 data_plane_format 一致）。
    pub(crate) fn format(&self) -> EngineConfig {
        self.config.clone()
    }

    /// 跑一块静音输入。
    pub(crate) fn run_block(&mut self) {
        let input = vec![0.0f32; self.engine.output_samples_per_block()];
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
        self.blocks_total()
    }

    fn request_mute(&mut self, mute: bool) -> MuteReceipt {
        self.user_mute = mute;
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
            input_streams: vec![khaen_host::aes67_input::Aes67InputStats::default()],
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

    fn begin_calibration_playback(
        &mut self,
        _playback: CalibrationPlayback,
    ) -> Result<u64, String> {
        Err(
            "FakeTarget has no calibration playback seam (the seam is on Runtime; see cal tests)"
                .to_string(),
        )
    }

    fn end_calibration_playback(&mut self) -> bool {
        false
    }

    fn user_mute_requested(&self) -> bool {
        self.user_mute
    }

    fn device_started(&self) -> bool {
        true
    }

    fn receiver_running(&self) -> bool {
        true
    }

    fn reconfigure(
        &mut self,
        request: ReconfigureRequest,
    ) -> Result<ReconfigureDone, ReconfigureFailure> {
        self.reconfigure_calls = self.reconfigure_calls.saturating_add(1);
        if let Some(failure) = self.reconfigure_failures.pop_front() {
            return Err(failure);
        }
        let engine_changed = request.config.engine != self.config;
        if engine_changed {
            // 换引擎前记住累计块数：热重配不重置音频时间（与真实 Runtime 同语义）。
            self.blocks_offset = self.blocks_total();
            self.engine = SlotEngine::new(request.config.engine.clone(), Box::new(Passthrough))
                .expect("fake engine rebuild");
            self.config = request.config.engine.clone();
        }
        let target = ChannelsView::from_config(&request.config);
        self.last_reconfigure_target = Some(target.clone());
        let hw = HwReadback {
            sample_rate: Some(self.config.sample_rate),
            channels: Some(u32::try_from(self.config.output_channels).unwrap_or(0)),
        };
        Ok(ReconfigureDone {
            outcome: ReconfigureOutcome {
                format: self.config.clone(),
                receiver_rebound: true,
                engine_rebuilt: engine_changed,
                graph_dropped: engine_changed,
                device_was_absent: false,
                hw,
                blocks: self.blocks_total(),
                alsa_stats_reset: true,
                input_stats_reset: true,
            },
            target,
        })
    }
}

// ===========================================================================
// Harness：核心 + 假块线程 + 快照 + 设备生命周期
// ===========================================================================

/// 一套控制通道夹具：ControlCore 经真 mpsc 连到一个假块线程（真 SlotEngine）。
///
/// 热重配（M3b）：块线程上下文带共享设备生命周期与追踪器（与真实 drive_loop 同构），
/// 命令执行后立即发布快照（与 ControlBridge::poll_commands 一致）；channels_* 默认关闭，
/// 用 [`Harness::with_channels`] 打开（初始配置 = [`fake_runtime_config`] + 假只读探测）。
pub(crate) struct Harness {
    core: Arc<Mutex<ControlCore>>,
    target: Arc<Mutex<FakeTarget>>,
    snapshot: SharedSnapshot,
    lifecycle: Arc<Mutex<DeviceLifecycle>>,
    reconfigure: SharedReconfigure,
    probe: Option<FakeProbeState>,
    stop: Arc<AtomicBool>,
    loop_handle: Option<thread::JoinHandle<()>>,
}

impl Harness {
    /// 默认参数（2 s 命令超时，48 kHz/128/2；channels_* 未接通）。
    pub(crate) fn new() -> Self {
        Self::with_timeout(Duration::from_secs(2))
    }

    /// 指定命令超时（超时语义测试用）。
    pub(crate) fn with_timeout(command_timeout: Duration) -> Self {
        Self::build(command_timeout, None, None)
    }

    /// 带进程内校准装配（T70f-c：cal op 全流程测试用）。
    pub(crate) fn with_calibration(setup: CalibrationSetup) -> Self {
        Self::build(Duration::from_secs(2), Some(setup), None)
    }

    /// 带热重配（M3b）：channels_* 接通（初始配置见 [`fake_runtime_config`]）。
    pub(crate) fn with_channels() -> Self {
        Self::with_channels_and_timeout(Duration::from_secs(2))
    }

    /// 带热重配 + 指定命令超时（ack 超时/在飞语义测试用）。
    pub(crate) fn with_channels_and_timeout(command_timeout: Duration) -> Self {
        Self::build(
            command_timeout,
            None,
            Some((fake_runtime_config(), FakeProbeState::default())),
        )
    }

    /// 带热重配 + 校准（互斥测试用）。
    pub(crate) fn with_channels_and_calibration(setup: CalibrationSetup) -> Self {
        Self::build(
            Duration::from_secs(2),
            Some(setup),
            Some((fake_runtime_config(), FakeProbeState::default())),
        )
    }

    fn build(
        command_timeout: Duration,
        calibration: Option<CalibrationSetup>,
        channels: Option<(RuntimeConfig, FakeProbeState)>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let target = Arc::new(Mutex::new(FakeTarget::new(test_config())));
        let snapshot: SharedSnapshot = Arc::new(Mutex::new(None));
        let lifecycle = Arc::new(Mutex::new(DeviceLifecycle::new()));
        let reconfigure = shared_tracker();
        let stop = Arc::new(AtomicBool::new(false));
        let probe = channels.as_ref().map(|(_, probe)| probe.clone());
        let channels_config = channels.map(|(config, probe)| ChannelsConfig {
            config,
            probe: Arc::new(FakeProbe::new(probe)),
            input_source: None,
        });
        let loop_target = Arc::clone(&target);
        let loop_snapshot = Arc::clone(&snapshot);
        let loop_lifecycle = Arc::clone(&lifecycle);
        let loop_reconfigure = Arc::clone(&reconfigure);
        let loop_stop = Arc::clone(&stop);
        let clock = Arc::new(AtomicU64::new(1));
        let loop_handle = thread::spawn(move || loop {
            match command_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(command) => {
                    // 与真实 drive_loop 同构：先取块边界命令，块线程上下文带生命周期/追踪器，
                    // 命令后立即发布快照（ControlBridge::poll_commands 的后置发布）。
                    let now_ms = clock.fetch_add(1, Ordering::SeqCst);
                    let value = {
                        let mut guard = loop_target
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let mut lifecycle_guard = loop_lifecycle
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let mut ctx = CommandContext {
                            lifecycle: &mut lifecycle_guard,
                            reconfigure: &loop_reconfigure,
                            now_ms,
                        };
                        execute_command(command, &mut *guard, &mut ctx);
                        drop(lifecycle_guard);
                        let lifecycle_guard = loop_lifecycle
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        capture_snapshot(&*guard, &lifecycle_guard, None)
                    };
                    *loop_snapshot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
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
            calibration,
            channels: channels_config,
            reconfigure: Some(Arc::clone(&reconfigure)),
        });
        Self {
            core: Arc::new(Mutex::new(core)),
            target,
            snapshot,
            lifecycle,
            reconfigure,
            probe,
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
            let lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            capture_snapshot(&*target, &lifecycle, None)
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
            let lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            capture_snapshot(&*target, &lifecycle, Some(100))
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
    pub(crate) fn lifecycle_mut(&mut self) -> std::sync::MutexGuard<'_, DeviceLifecycle> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 热重配追踪器快照（generation/last_gap/last_result/在飞）。
    pub(crate) fn reconfigure_tracker(&self) -> crate::control::ReconfigureTracker {
        self.reconfigure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 当前 generation（已生效热重配次数）。
    pub(crate) fn generation(&self) -> u64 {
        self.reconfigure_tracker().generation
    }

    /// 假块线程收到的热重配请求数。
    pub(crate) fn reconfigure_calls(&mut self) -> u64 {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reconfigure_calls
    }

    /// 注入一次热重配失败（块线程返回 ReconfigureFailed）。
    pub(crate) fn fail_next_reconfigure(&mut self, failure: ReconfigureFailure) {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fail_next_reconfigure(failure);
    }

    /// 最近一次热重配目标视图（块线程侧）。
    pub(crate) fn last_reconfigure_target(&mut self) -> Option<ChannelsView> {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_reconfigure_target
            .clone()
    }

    /// 连续块计数（热重配换引擎前后累计；与真实 Runtime 的 blocks 语义一致）。
    pub(crate) fn blocks(&mut self) -> u64 {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .blocks_total()
    }

    /// 假块线程当前运行格式（热重配成功后会变）。
    pub(crate) fn current_format(&mut self) -> EngineConfig {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .format()
    }

    /// 用户静音请求（块线程侧当前值）。
    pub(crate) fn user_mute_requested(&mut self) -> bool {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .user_mute
    }

    /// 假只读探测状态（channels 未接通时为 None）。
    pub(crate) fn probe(&self) -> Option<FakeProbeState> {
        self.probe.clone()
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
        let expected = self.config.output_samples_per_block() * BYTES_PER_SAMPLE;
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
        additional_inputs: Vec::new(),
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
            input_channels: CHANNELS,
            output_channels: CHANNELS,
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
// ===========================================================================
// 热重配夹具（M3b）：假只读探测 + 假运行时配置
// ===========================================================================

/// 假只读设备探测状态：可用性 + 回读值（prepare 探测用；commit 回读校验在块线程夹具里）。
#[derive(Clone)]
pub(crate) struct FakeProbeState {
    /// 探测是否可用（false 模拟设备 absent/读不到 hw params）。
    pub(crate) available: Arc<AtomicBool>,
    /// 探测返回的 hw params。
    pub(crate) readback: Arc<Mutex<HwReadback>>,
}

impl Default for FakeProbeState {
    fn default() -> Self {
        Self {
            available: Arc::new(AtomicBool::new(true)),
            readback: Arc::new(Mutex::new(HwReadback {
                sample_rate: Some(SAMPLE_RATE),
                channels: Some(CHANNELS as u32),
            })),
        }
    }
}

impl FakeProbeState {
    /// 设置探测可用性（false = 设备不可用）。
    pub(crate) fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }

    /// 设置探测回读值（模拟设备当前 hw params）。
    pub(crate) fn set_readback(&self, sample_rate: u32, channels: u32) {
        *self
            .readback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = HwReadback {
            sample_rate: Some(sample_rate),
            channels: Some(channels),
        };
    }
}

/// 假只读探测（实现 [`DeviceProbe`]）。
pub(crate) struct FakeProbe {
    state: FakeProbeState,
}

impl FakeProbe {
    /// 新建。
    pub(crate) fn new(state: FakeProbeState) -> Self {
        Self { state }
    }
}

impl DeviceProbe for FakeProbe {
    fn probe(&self) -> Result<HwReadback, String> {
        if !self.state.available.load(Ordering::SeqCst) {
            return Err(
                "fake ALSA device is absent or its hw params are unreadable (read-only probe)"
                    .to_string(),
            );
        }
        Ok(*self
            .state
            .readback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()))
    }
}

/// 与 [`test_config`] 一致的完整运行时配置（Harness 的 channels 上下文用）。
pub(crate) fn fake_runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        additional_inputs: Vec::new(),
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
            device: khaen_host::DeviceId::new("khaen-rt-host-control-fake-pcm"),
            sample_rate: SAMPLE_RATE,
            block_frames: BLOCK_FRAMES,
            input_channels: CHANNELS,
            output_channels: CHANNELS,
        },
        safety: SafetyPolicy::default(),
        receiver: ReceiverConfig::default(),
        supervisor: SupervisorOptions::default(),
        unloaded_policy: UnloadedPolicy::Passthrough,
    }
}
