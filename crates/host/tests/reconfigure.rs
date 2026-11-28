//! Runtime 热重配 seam（M3b）门禁：不退出进程换采样率/通道数/AES67 输入/输出设备。
//!
//! 覆盖（全部假 PCM/假包源，Windows 本机可跑）：
//! - 格式变更：重建引擎+输入抖动环+收包线程、块计数连续、旧图丢弃（或 stage 预编译草稿）；
//! - 只换 AES67 输入（映射/端口）不换格式：引擎与图保持；
//! - 目标配置非法：在碰任何句柄之前拒绝；
//! - 新设备打不开 / hw 回读不符 / 回读不可用：自动回滚到旧配置，旧设备/旧收包恢复；
//! - 注入假包源没有源工厂时，需要重绑收包的重配如实拒绝（fail-closed）；
//! - 收包重绑失败：恢复旧收包线程；
//! - 设备 absent 时重配成功：设备打开、设备丢失故障清除、预编译方案在新格式上 stage。
//!
//! 真实 ALSA/网卡路径（rt-host 生产装配）标 NOT_RUN，由 Linux CI/真机覆盖。

use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_host::admission::AdmissionLimits;
use khaen_host::aes67_input::Aes67InputConfig;
use khaen_host::alsa_backend::{PcmError, PcmFault, PcmOutput, BYTES_PER_SAMPLE};
use khaen_host::backend::{BackendError, DeviceConfig};
use khaen_host::engine::EngineConfig;
use khaen_host::runtime::{
    codes, compile_plan_draft, HwReadback, PacketSource, ReceiverConfig, Runtime, RuntimeConfig,
    SupervisorOptions, UnloadedPolicy,
};
use khaen_host::safety::SafetyPolicy;
use khaen_host::DeviceId;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;

// ===========================================================================
// 可拔插假 PCM（按设备名注册；open 可失败、写计数可断言）
// ===========================================================================

#[derive(Clone, Default)]
struct PcmState {
    present: Arc<AtomicBool>,
    opens: Arc<AtomicU32>,
    writes: Arc<AtomicU32>,
    closed: Arc<AtomicU32>,
    configs: Arc<Mutex<Vec<DeviceConfig>>>,
}

static REGISTRY: OnceLock<Mutex<Vec<(String, PcmState)>>> = OnceLock::new();

fn registry() -> &'static Mutex<Vec<(String, PcmState)>> {
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn register_pcm(device: &str) -> PcmState {
    let state = PcmState::default();
    state.present.store(true, Ordering::SeqCst);
    registry()
        .lock()
        .expect("registry")
        .push((device.to_string(), state.clone()));
    state
}

fn pcm_state(device: &str) -> PcmState {
    registry()
        .lock()
        .expect("registry")
        .iter()
        .find(|(name, _)| name == device)
        .map(|(_, state)| state.clone())
        .unwrap_or_else(|| panic!("pcm {device} not registered"))
}

/// 假 PCM：open 成功与否由注册状态决定；write 逐块校验字节数（脏写/长度错直接 panic）。
struct ReconfigPcm {
    state: PcmState,
    config: DeviceConfig,
}

impl PcmOutput for ReconfigPcm {
    fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
        let state = pcm_state(config.device.as_str());
        state.opens.fetch_add(1, Ordering::SeqCst);
        if !state.present.load(Ordering::SeqCst) {
            return Err(BackendError::Fault(format!(
                "fake pcm {} is absent",
                config.device.as_str()
            )));
        }
        state.configs.lock().expect("configs").push(config.clone());
        Ok(Self {
            state,
            config: config.clone(),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
        assert_eq!(
            bytes.len(),
            self.config.output_samples_per_block() * BYTES_PER_SAMPLE,
            "fake pcm block bytes",
        );
        self.state.writes.fetch_add(1, Ordering::SeqCst);
        Ok(self.config.block_frames)
    }

    fn prepare(&mut self, _fault: PcmFault) -> Result<(), BackendError> {
        Ok(())
    }

    fn available(&self) -> Result<usize, PcmError> {
        Ok(self.config.block_frames * 4)
    }

    fn close(&mut self) {
        self.state.closed.fetch_add(1, Ordering::SeqCst);
    }
}

// ===========================================================================
// 假包源 + 收包源工厂
// ===========================================================================

/// 空闲假包源：没有包，按有限时间返回 WouldBlock（收包线程能 join）。
struct IdleSource;

impl PacketSource for IdleSource {
    fn recv(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_micros(200));
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
}

/// 收包源工厂：记录创建次数；可选地让指定端口的 bind 失败（模拟端口占用）。
#[derive(Clone, Default)]
struct SourceFactoryState {
    created: Arc<AtomicU32>,
    fail_port: Arc<Mutex<Option<u16>>>,
}

fn source_factory(
    state: SourceFactoryState,
) -> impl Fn(
    &Aes67InputConfig,
    &ReceiverConfig,
) -> Result<Box<dyn PacketSource + Send>, khaen_host::runtime::ReceiverError>
       + Send
       + 'static {
    move |input: &Aes67InputConfig, _receiver: &ReceiverConfig| {
        if state.fail_port.lock().expect("fail port").as_ref() == Some(&input.port) {
            return Err(khaen_host::runtime::ReceiverError::new(
                codes::RECEIVER_BIND,
                format!("fake bind failure on udp port {}", input.port),
            ));
        }
        state.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(IdleSource) as Box<dyn PacketSource + Send>)
    }
}

// ===========================================================================
// 配置/方案夹具
// ===========================================================================

fn base_config(device: &str) -> RuntimeConfig {
    RuntimeConfig {
        additional_inputs: Vec::new(),
        capture: None,
        input: Aes67InputConfig {
            multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
            port: 5004,
            payload_type: 98,
            sample_rate: SAMPLE_RATE,
            channels: 2,
            channels_map: vec![0, 1],
            output_channels: 2,
            block_frames: BLOCK_FRAMES,
            jitter_target_frames: 0,
        },
        engine: EngineConfig {
            block_frames: BLOCK_FRAMES,
            sample_rate: SAMPLE_RATE,
            input_channels: 2,
            output_channels: 2,
        },
        device: DeviceConfig {
            device: DeviceId::new(device),
            sample_rate: SAMPLE_RATE,
            block_frames: BLOCK_FRAMES,
            input_channels: 2,
            output_channels: 2,
        },
        safety: SafetyPolicy {
            max_consecutive_underruns: 10_000,
            max_consecutive_overruns: 10_000,
        },
        receiver: ReceiverConfig::default(),
        supervisor: SupervisorOptions::default(),
        unloaded_policy: UnloadedPolicy::Passthrough,
    }
}

/// 派生目标配置：换设备/速率/通道/AES67 端口（块长保持不变，与 format op 一致）。
fn derive_config(
    base: &RuntimeConfig,
    device: &str,
    sample_rate: u32,
    channels: usize,
    port: u16,
) -> RuntimeConfig {
    let mut target = base.clone();
    target.input.port = port;
    target.input.sample_rate = sample_rate;
    target.input.channels = channels;
    target.input.channels_map = (0..channels as u16).collect();
    target.input.output_channels = channels;
    target.input.block_frames = BLOCK_FRAMES;
    target.engine = EngineConfig {
        block_frames: BLOCK_FRAMES,
        sample_rate,
        input_channels: channels,
        output_channels: channels,
    };
    target.device = DeviceConfig {
        device: DeviceId::new(device),
        sample_rate,
        block_frames: BLOCK_FRAMES,
        input_channels: channels,
        output_channels: channels,
    };
    target
}

fn identity_plan(channels: u16, block_frames: u32, sample_rate: u32) -> Plan {
    let count = channels as usize;
    let lanes = channels;
    let mut matrix_coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        matrix_coeffs[lane * count + lane] = 1.0;
    }
    Plan {
        block_size: block_frames,
        sample_rate,
        content: Some(ContentSection {
            in_channels: channels,
            out_lanes: lanes,
            folds: (0..lanes)
                .map(|lane| Fold {
                    src: lane,
                    dst: lane,
                    gain: 1.0,
                })
                .collect(),
        }),
        crossover: Some(CrossoverSection {
            lanes,
            filters: (0..lanes)
                .map(|lane| CrossoverFilter {
                    lane,
                    coeffs: vec![1.0],
                })
                .collect(),
        }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection {
            lanes,
            entries: (0..lanes)
                .map(|lane| ProtectionEntry {
                    lane,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                })
                .collect(),
        }),
        routing: Some(RoutingSection {
            dac_channels: channels,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

fn commit_identity(runtime: &mut Runtime<ReconfigPcm>, channels: u16) {
    let plan = identity_plan(channels, BLOCK_FRAMES as u32, SAMPLE_RATE);
    let draft = runtime
        .prepare_plan(&plan.encode(), &AdmissionLimits::default())
        .expect("prepare identity plan");
    runtime.commit_plan(draft).expect("commit identity plan");
}

fn unique(tag: &str) -> String {
    format!("host-reconfig-{}-{tag}", std::process::id())
}

// ===========================================================================
// 成功路径
// ===========================================================================

#[test]
fn reconfigure_format_change_rebuilds_engine_receiver_and_keeps_blocks() {
    let old_device = unique("pcm-old");
    let new_device = unique("pcm-new");
    register_pcm(&old_device);
    let new_state = register_pcm(&new_device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&old_device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state.clone()),
    )
    .expect("runtime");
    commit_identity(&mut runtime, 2);
    for _ in 0..3 {
        let _ = runtime.run_block();
    }
    let blocks_before = runtime.blocks();
    assert!(blocks_before >= 3);
    let creates_before = factory_state.created.load(Ordering::SeqCst);

    let target = derive_config(&config, &new_device, 96_000, 4, 5_006);
    let outcome = runtime
        .reconfigure(target.clone(), None, || {
            Ok(HwReadback {
                sample_rate: Some(96_000),
                channels: Some(4),
            })
        })
        .expect("reconfigure succeeds");

    // 机械结果：引擎/收包/输入都重建，块计数连续，旧图随旧引擎丢弃。
    assert!(outcome.engine_rebuilt);
    assert!(outcome.receiver_rebound);
    assert!(outcome.graph_dropped);
    assert!(!outcome.device_was_absent);
    assert_eq!(outcome.format.sample_rate, 96_000);
    assert_eq!(outcome.format.output_channels, 4);
    assert_eq!(
        outcome.hw,
        HwReadback {
            sample_rate: Some(96_000),
            channels: Some(4)
        }
    );
    assert_eq!(
        outcome.blocks, blocks_before,
        "hot reconfiguration must not reset the block counter"
    );
    assert_eq!(runtime.blocks(), blocks_before);
    assert_eq!(runtime.config().engine, target.engine);
    assert_eq!(runtime.config().input.port, 5_006);
    assert!(runtime.device_started());
    assert!(runtime.receiver_running());
    assert_eq!(
        factory_state.created.load(Ordering::SeqCst),
        creates_before + 1,
        "the AES67 receiver must be rebuilt for a port change",
    );
    assert_eq!(new_state.opens.load(Ordering::SeqCst), 1);
    assert_eq!(new_state.writes.load(Ordering::SeqCst), 0);
    // 旧图被丢弃（没有预编译草稿）；状态如实报没有图。
    assert!(runtime.graph_info().graph_id.is_none());

    // 块循环继续：新格式的块继续写新设备（每块字节数由假 PCM 断言）。
    let _ = runtime.run_block();
    assert_eq!(runtime.blocks(), blocks_before + 1);
    assert_eq!(new_state.writes.load(Ordering::SeqCst), 1);
}

#[test]
fn reconfigure_aes67_only_change_keeps_running_graph() {
    let device = unique("pcm-map");
    register_pcm(&device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state),
    )
    .expect("runtime");
    commit_identity(&mut runtime, 2);
    for _ in 0..3 {
        let _ = runtime.run_block();
    }
    let graph_before = runtime.graph_info();
    assert!(graph_before.graph_id.is_some());
    let blocks_before = runtime.blocks();

    // 只换 AES67 端口/映射，不换格式 → 引擎与图保持，只重建输入抖动环与收包 socket。
    let mut target = config.clone();
    target.input.port = 5_010;
    target.input.payload_type = 100;
    let outcome = runtime
        .reconfigure(target, None, || {
            Ok(HwReadback {
                sample_rate: Some(SAMPLE_RATE),
                channels: Some(2),
            })
        })
        .expect("reconfigure succeeds");
    assert!(!outcome.engine_rebuilt);
    assert!(!outcome.graph_dropped);
    assert!(outcome.receiver_rebound);
    assert_eq!(outcome.blocks, blocks_before);
    assert_eq!(runtime.config().input.payload_type, 100);
    assert_eq!(runtime.graph_info().graph_id, graph_before.graph_id);
    assert_eq!(runtime.graph_info().version, graph_before.version);
    // 块继续，且图没有被丢（继续用同一个 SlotEngine）。
    let _ = runtime.run_block();
    assert_eq!(runtime.blocks(), blocks_before + 1);
}

#[test]
fn reconfigure_stages_precompiled_plan_for_new_format() {
    let device = unique("pcm-stage");
    register_pcm(&device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state),
    )
    .expect("runtime");
    commit_identity(&mut runtime, 2);
    for _ in 0..2 {
        let _ = runtime.run_block();
    }

    let target = derive_config(&config, &device, SAMPLE_RATE, 3, 5_012);
    // 控制线程按新格式预编译的方案草稿：stage 到新引擎（不丢图）。
    let plan = identity_plan(3, BLOCK_FRAMES as u32, SAMPLE_RATE);
    let draft = compile_plan_draft(&plan, &target.engine, &AdmissionLimits::default(), 9)
        .expect("compile plan for target format");
    let outcome = runtime
        .reconfigure(target, Some(draft), || {
            Ok(HwReadback {
                sample_rate: Some(SAMPLE_RATE),
                channels: Some(3),
            })
        })
        .expect("reconfigure succeeds");
    assert!(outcome.engine_rebuilt);
    assert!(
        !outcome.graph_dropped,
        "a precompiled plan must be staged, not dropped"
    );
    // stage 的是"下一块边界生效"：重配当场还没有 running graph，块循环下一块才换上。
    assert_eq!(runtime.graph_info().version, None);
    assert!(runtime.status().switch_pending);
    let _ = runtime.run_block();
    assert_eq!(runtime.graph_info().version, Some(9));
}

// ===========================================================================
// 失败 / 回滚路径
// ===========================================================================

#[test]
fn reconfigure_rejects_invalid_target_before_touching_handles() {
    let device = unique("pcm-invalid");
    let state = register_pcm(&device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state.clone()),
    )
    .expect("runtime");
    let _ = runtime.run_block();
    let blocks_before = runtime.blocks();
    let opens_before = state.opens.load(Ordering::SeqCst);
    let creates_before = factory_state.created.load(Ordering::SeqCst);
    let mut target = config.clone();
    target.engine.output_channels = 4; // 与 device/input 不一致：validate 失败
    let failure = runtime
        .reconfigure(target, None, || Ok(HwReadback::default()))
        .expect_err("invalid target must be rejected");
    assert_eq!(failure.code, codes::RECONFIGURE_CONFIG);
    assert_eq!(failure.stage, "validate");
    assert!(failure.rolled_back);
    assert!(failure.device_started);
    assert!(failure.receiver_running);
    assert_eq!(
        runtime.blocks(),
        blocks_before,
        "rejected reconfiguration must not advance audio"
    );
    assert_eq!(
        state.opens.load(Ordering::SeqCst),
        opens_before,
        "must not open/close the device"
    );
    assert_eq!(
        factory_state.created.load(Ordering::SeqCst),
        creates_before,
        "must not rebuild the receiver",
    );
}

#[test]
fn reconfigure_rolls_back_when_new_device_open_fails() {
    let old_device = unique("pcm-rollback-old");
    let new_device = unique("pcm-rollback-new");
    let old_state = register_pcm(&old_device);
    let new_state = register_pcm(&new_device);
    new_state.present.store(false, Ordering::SeqCst);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&old_device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state),
    )
    .expect("runtime");
    let _ = runtime.run_block();
    let blocks_before = runtime.blocks();
    let old_opens_before = old_state.opens.load(Ordering::SeqCst);

    let target = derive_config(&config, &new_device, 96_000, 4, 5_020);
    let failure = runtime
        .reconfigure(target, None, || Ok(HwReadback::default()))
        .expect_err("opening the new device must fail");
    assert_eq!(failure.code, codes::RECONFIGURE_DEVICE);
    assert_eq!(failure.stage, "device_open");
    assert!(failure.rolled_back, "old configuration must be restored");
    assert!(failure.device_started, "the old device must be reopened");
    assert!(
        failure.receiver_running,
        "the old receiver must be running again"
    );
    // 旧配置原样保留，块循环继续（同一个进程/运行时）。
    assert_eq!(runtime.config().engine, config.engine);
    assert_eq!(runtime.config().device.device, config.device.device);
    assert!(runtime.device_started());
    assert!(runtime.receiver_running());
    assert_eq!(
        old_state.opens.load(Ordering::SeqCst),
        old_opens_before + 1,
        "rollback must reopen the previous device",
    );
    let _ = runtime.run_block();
    assert_eq!(runtime.blocks(), blocks_before + 1);
}

#[test]
fn reconfigure_rolls_back_when_hw_readback_mismatches() {
    let old_device = unique("pcm-verify-old");
    let new_device = unique("pcm-verify-new");
    let old_state = register_pcm(&old_device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&old_device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state),
    )
    .expect("runtime");
    let _ = runtime.run_block();
    let blocks_before = runtime.blocks();
    let old_opens_before = old_state.opens.load(Ordering::SeqCst);

    let _ = register_pcm(&new_device);
    let target = derive_config(&config, &new_device, 96_000, 4, 5_030);
    // 设备打开了，但回读的速率/通道与目标不一致（例如设备跑在别的速率上）→ 回滚。
    let failure = runtime
        .reconfigure(target, None, || {
            Ok(HwReadback {
                sample_rate: Some(48_000),
                channels: Some(4),
            })
        })
        .expect_err("hw readback mismatch must roll back");
    assert_eq!(failure.code, codes::RECONFIGURE_VERIFY);
    assert_eq!(failure.stage, "hw_verify");
    assert!(failure.rolled_back);
    assert_eq!(runtime.config().engine, config.engine);
    assert!(runtime.device_started());
    assert_eq!(old_state.opens.load(Ordering::SeqCst), old_opens_before + 1);
    assert_eq!(runtime.blocks(), blocks_before);
    let _ = runtime.run_block();
    assert_eq!(runtime.blocks(), blocks_before + 1);
}

#[test]
fn reconfigure_fails_closed_when_readback_is_unavailable() {
    let old_device = unique("pcm-noverify-old");
    let new_device = unique("pcm-noverify-new");
    let _old_state = register_pcm(&old_device);
    let _new_state = register_pcm(&new_device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&old_device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state),
    )
    .expect("runtime");
    let target = derive_config(&config, &new_device, SAMPLE_RATE, 2, 5_040);
    let failure = runtime
        .reconfigure(target, None, || Ok(HwReadback::default()))
        .expect_err("missing hw readback must be fail-closed");
    assert_eq!(failure.code, codes::RECONFIGURE_VERIFY);
    assert!(failure.message.contains("readback unavailable"));
    assert_eq!(runtime.config().device.device, config.device.device);
}

#[test]
fn reconfigure_without_source_factory_refuses_receiver_rebind() {
    let device = unique("pcm-factory-none");
    let state = register_pcm(&device);
    let config = base_config(&device);
    let mut runtime =
        Runtime::<ReconfigPcm>::with_source(config.clone(), IdleSource).expect("runtime");
    let _ = runtime.run_block();
    let opens_before = state.opens.load(Ordering::SeqCst);
    let mut target = config.clone();
    target.input.port = 5_099;
    let failure = runtime
        .reconfigure(target, None, || {
            Ok(HwReadback {
                sample_rate: Some(SAMPLE_RATE),
                channels: Some(2),
            })
        })
        .expect_err("receiver rebind without factory must be refused");
    assert_eq!(failure.code, codes::RECONFIGURE_RECEIVER);
    assert_eq!(failure.stage, "receiver_source");
    assert!(failure.rolled_back);
    assert!(failure.device_started);
    assert!(failure.receiver_running);
    assert_eq!(runtime.config().input.port, 5004);
    assert_eq!(state.opens.load(Ordering::SeqCst), opens_before);
    let _ = runtime.run_block();
}

#[test]
fn reconfigure_receiver_rebind_failure_restores_old_receiver() {
    let device = unique("pcm-rx-fail");
    register_pcm(&device);
    let factory_state = SourceFactoryState::default();
    *factory_state.fail_port.lock().expect("fail port") = Some(5_050);
    let config = base_config(&device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state.clone()),
    )
    .expect("runtime");
    let _ = runtime.run_block();
    let target = derive_config(&config, &device, SAMPLE_RATE, 2, 5_050);
    let failure = runtime
        .reconfigure(target, None, || Ok(HwReadback::default()))
        .expect_err("receiver rebind must fail");
    assert_eq!(failure.code, codes::RECONFIGURE_RECEIVER);
    assert_eq!(failure.stage, "receiver_spawn");
    assert!(failure.rolled_back, "the old receiver must be restored");
    assert!(failure.receiver_running);
    assert!(runtime.receiver_running());
    assert_eq!(runtime.config().input.port, 5004);
    let _ = runtime.run_block();
}

#[test]
fn reconfigure_recovers_absent_device_and_stages_plan() {
    let old_device = unique("pcm-absent-old");
    let new_device = unique("pcm-absent-new");
    register_pcm(&old_device);
    register_pcm(&new_device);
    let factory_state = SourceFactoryState::default();
    let config = base_config(&old_device);
    let mut runtime = Runtime::<ReconfigPcm>::with_source_deferred_factory(
        config.clone(),
        IdleSource,
        source_factory(factory_state),
    )
    .expect("deferred runtime");
    assert!(!runtime.device_started());
    let blocks_before = runtime.blocks();

    // 设备 absent 时重配成功：新设备打开、清掉设备丢失故障、预编译方案在新格式上 stage。
    let target = derive_config(&config, &new_device, SAMPLE_RATE, 3, 5_060);
    let plan = identity_plan(3, BLOCK_FRAMES as u32, SAMPLE_RATE);
    let draft = compile_plan_draft(&plan, &target.engine, &AdmissionLimits::default(), 3)
        .expect("compile plan for target format");
    let outcome = runtime
        .reconfigure(target.clone(), Some(draft), || {
            Ok(HwReadback {
                sample_rate: Some(SAMPLE_RATE),
                channels: Some(3),
            })
        })
        .expect("reconfigure should recover the absent device");
    assert!(outcome.device_was_absent);
    assert!(outcome.engine_rebuilt);
    assert!(!outcome.graph_dropped);
    assert!(runtime.device_started());
    assert_eq!(runtime.config().engine, target.engine);
    assert_eq!(outcome.blocks, blocks_before);
    let _ = runtime.run_block();
    assert_eq!(runtime.graph_info().version, Some(3));
}
