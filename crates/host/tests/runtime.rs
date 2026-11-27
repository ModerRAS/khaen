//! 单进程运行时门禁（T70d）：假包源 → SPSC → Aes67Input → SlotEngine → 假 PCM 设备。
//!
//! 覆盖：
//! - 假包源注入 RTP 包 → 输入块逐样本正确（L24 解码）与声道映射；
//! - 欠载整块静音、播放游标前进（用 late 计数证明）；
//! - 端到端：RTP 帧 → identity 方案图 → S24_3LE 逐字节输出；
//! - 方案未加载/未 commit 的行为（Passthrough / Silence 两种策略，写清楚断言）；
//! - 设备监督器状态变化（枚举→控制→速率断言→流式→拔出→安全静音→插回恢复）
//!   与 RuntimeStatus 字段一致；
//! - 收包线程 stop() 后 join 成功、幂等、不 panic；
//! - UDP socket 绑定/join 失败返回稳定错误码；
//! - 运行时配置自校验 fail-closed；
//! - run_block 稳态零堆分配（独立子进程计量）。
//!
//! 全部使用假实现，Windows 本机可跑；真实 UDP 音频通路与真实 ALSA 设备标 NOT_RUN（见报告）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_host::admission::AdmissionLimits;
use khaen_host::aes67_input::Aes67InputConfig;
use khaen_host::alsa_backend::{
    encode_s24_3le_block, AlsaStats, PcmError, PcmFault, PcmOutput, BYTES_PER_SAMPLE,
};
use khaen_host::backend::{BackendError, DeviceConfig};
use khaen_host::engine::{EngineConfig, GraphInfo};
use khaen_host::owner::{DeviceGuard, DeviceId, OwnerError};
use khaen_host::runtime::{
    codes, PacketSource, ReceiverConfig, Runtime, RuntimeConfig, RuntimeError, SupervisorOptions,
    UdpPacketSource, UnloadedPolicy,
};
use khaen_host::safety::{RuntimeState, SafetyPolicy};
use khaen_host::supervisor::{
    ClockSource, DeviceProbe, DeviceSupervisor, LifecycleState, ProbeError, RateControl,
    StreamControl, SupervisorErrorCode, SupervisorPolicy,
};

// ===========================================================================
// 测试常量 / 假 PCM 设备
// ===========================================================================

const SAMPLE_RATE: u32 = 48_000;
const PT: u8 = 98;
const SSRC: u32 = 0x0bad_cafe;
const L24_SCALE: f32 = 8_388_608.0;

/// 假 PCM 设备：只把每块写进来的 S24_3LE 字节拷进预分配缓冲（零分配），不做任何设备 IO。
#[derive(Debug)]
struct FakePcm {
    config: Option<DeviceConfig>,
    captured: Vec<u8>,
    writes: u64,
    prepares: u64,
}

impl FakePcm {
    fn blank() -> Self {
        Self {
            config: None,
            captured: Vec::new(),
            writes: 0,
            prepares: 0,
        }
    }
}

impl PcmOutput for FakePcm {
    fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
        Ok(Self {
            config: Some(config.clone()),
            captured: vec![0u8; config.output_samples_per_block() * BYTES_PER_SAMPLE],
            ..Self::blank()
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
        let bytes_per_frame = match self.config.as_ref() {
            Some(config) => config.output_channels * BYTES_PER_SAMPLE,
            None => return Err(PcmError::Fatal("fake pcm not open")),
        };
        assert_eq!(bytes.len(), self.captured.len(), "fake pcm block size");
        self.captured.copy_from_slice(bytes);
        self.writes += 1;
        Ok(bytes.len() / bytes_per_frame)
    }

    fn prepare(&mut self, _fault: PcmFault) -> Result<(), BackendError> {
        self.prepares += 1;
        Ok(())
    }

    fn available(&self) -> Result<usize, PcmError> {
        Ok(self
            .config
            .as_ref()
            .map_or(0, |config| config.block_frames * 4))
    }

    fn close(&mut self) {
        self.config = None;
    }
}

// ===========================================================================
// 假包源（PacketSource 注入）
// ===========================================================================

#[derive(Clone, Default)]
struct FakeSourceHandle {
    packets: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl FakeSourceHandle {
    /// 注入一个完整 RTP 包（计量窗口外调用）。
    fn send(&self, packet: Vec<u8>) {
        self.packets
            .lock()
            .expect("fake source queue")
            .push_back(packet);
    }
}

struct FakeSource {
    handle: FakeSourceHandle,
}

impl PacketSource for FakeSource {
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let packet = {
            let mut queue = self.handle.packets.lock().expect("fake source queue");
            queue.pop_front()
        };
        match packet {
            Some(packet) => {
                let len = packet.len().min(buf.len());
                buf[..len].copy_from_slice(&packet[..len]);
                Ok(len)
            }
            None => {
                // 空闲时短暂睡眠后返回 WouldBlock：收包线程能在有限时间内看到停止标志。
                thread::sleep(Duration::from_micros(200));
                Err(io::Error::from(io::ErrorKind::WouldBlock))
            }
        }
    }
}

fn fake_source() -> (FakeSource, FakeSourceHandle) {
    let handle = FakeSourceHandle::default();
    (
        FakeSource {
            handle: handle.clone(),
        },
        handle,
    )
}

// ===========================================================================
// 运行时测试夹具
// ===========================================================================

fn runtime_config(output_channels: usize, block_frames: usize) -> RuntimeConfig {
    RuntimeConfig {
        input: Aes67InputConfig {
            multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
            port: 5004,
            payload_type: PT,
            sample_rate: SAMPLE_RATE,
            channels: output_channels,
            channels_map: (0..output_channels as u16).collect(),
            output_channels,
            block_frames,
            jitter_target_frames: 0,
        },
        engine: EngineConfig {
            block_frames,
            sample_rate: SAMPLE_RATE,
            input_channels: output_channels,
            output_channels,
        },
        device: DeviceConfig {
            device: DeviceId::new("fake-runtime-pcm"),
            sample_rate: SAMPLE_RATE,
            block_frames,
            input_channels: output_channels,
            output_channels,
        },
        safety: SafetyPolicy::default(),
        receiver: ReceiverConfig::default(),
        supervisor: SupervisorOptions::default(),
        unloaded_policy: UnloadedPolicy::Passthrough,
    }
}

fn runtime_for(
    output_channels: usize,
    block_frames: usize,
) -> (Runtime<FakePcm>, FakeSourceHandle) {
    let (source, handle) = fake_source();
    let runtime =
        Runtime::<FakePcm>::with_source(runtime_config(output_channels, block_frames), source)
            .expect("runtime");
    (runtime, handle)
}

/// 等待收包线程把注入的包推进 SPSC（计数在 push 之后更新，因此看到计数即槽位可见）。
fn wait_for_packets<P: PcmOutput>(runtime: &Runtime<P>, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while runtime.receiver().stats().packets_received < expected {
        assert!(
            Instant::now() < deadline,
            "receiver delivered only {} of {expected} packets",
            runtime.receiver().stats().packets_received
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// 组一个 RTP 包（12 字节基础头 + L24 payload）。
fn packet(sequence: u16, timestamp: u32, samples: &[i32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + samples.len() * 3);
    out.push(0x80);
    out.push(PT);
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.extend_from_slice(&SSRC.to_be_bytes());
    for &sample in samples {
        let raw = (sample as u32) & 0x00ff_ffff;
        out.push((raw >> 16) as u8);
        out.push((raw >> 8) as u8);
        out.push(raw as u8);
    }
    out
}

/// 24-bit 满量程整数 → f32（测试侧独立期望公式：raw / 2^23）。
fn decoded(raw: i32) -> f32 {
    raw as f32 / L24_SCALE
}

fn decoded_all(samples: &[i32]) -> Vec<f32> {
    samples.iter().map(|&raw| decoded(raw)).collect()
}

/// 逐样本 bit 相等断言（identity 图承诺逐位直通）。
fn assert_bits(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "block length");
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "sample {index}: {actual} vs {expected}"
        );
    }
}

/// 全恒等方案（判据同 crates/host/tests/graph_executor.rs）：逐样本等于直通。
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

// ===========================================================================
// a) 假包源 → 输入块内容
// ===========================================================================

#[test]
fn fake_packet_source_feeds_input_block_sample_exact() {
    const SIGNAL: [i32; 16] = [
        0, 1, -1, 8_388_607, -8_388_608, 123_456, -123_456, 2_097_151, -2_097_152, 42, -42,
        1_000_000, -1_000_000, 7, -7, 3_333_333,
    ];
    let (mut runtime, handle) = runtime_for(2, 8);
    handle.send(packet(0, 0, &SIGNAL));
    wait_for_packets(&runtime, 1);

    let block = runtime.run_block().expect("block").to_vec();
    assert_bits(&block, &decoded_all(&SIGNAL));

    let status = runtime.status();
    assert_eq!(status.blocks, 1);
    assert_eq!(status.input.packets, 1);
    assert_eq!(status.input.frames_received, 8);
    assert_eq!(status.input.frames_filled, 8);
    assert_eq!(status.input.jitter_fill, 0);
    assert_eq!(status.input.ssrc, Some(SSRC));
    assert_eq!(status.receiver.packets_received, 1);
    assert_eq!(status.receiver.bytes_received, 60);
    assert_eq!(status.receiver.queue_full_drops, 0);
    assert_eq!(status.output.blocks_written, 1);
    assert!(status.output_started);
    assert_eq!(status.state, RuntimeState::Running);
}

#[test]
fn channel_map_places_rtp_channels_and_zero_fills() {
    const SIGNAL: [i32; 16] = [
        11, 22, 33, 44, 55, 66, 77, 88, 99, 111, 222, 333, 444, 555, 666, 777,
    ];
    let mut config = runtime_config(4, 8);
    config.input.channels = 2;
    // RTP ch0 -> 输出槽 2，RTP ch1 -> 输出槽 0；槽 1/3 未映射恒 0。
    config.input.channels_map = vec![2, 0];
    let (source, handle) = fake_source();
    let mut runtime = Runtime::<FakePcm>::with_source(config, source).expect("runtime");
    handle.send(packet(0, 0, &SIGNAL));
    wait_for_packets(&runtime, 1);

    let block = runtime.run_block().expect("block");
    assert_eq!(block.len(), 8 * 4);
    for frame in 0..8 {
        let left = decoded(SIGNAL[frame * 2]);
        let right = decoded(SIGNAL[frame * 2 + 1]);
        let base = frame * 4;
        assert_eq!(
            block[base].to_bits(),
            right.to_bits(),
            "slot 0 frame {frame}"
        );
        assert_eq!(block[base + 1].to_bits(), 0, "slot 1 frame {frame}");
        assert_eq!(
            block[base + 2].to_bits(),
            left.to_bits(),
            "slot 2 frame {frame}"
        );
        assert_eq!(block[base + 3].to_bits(), 0, "slot 3 frame {frame}");
    }
    assert_eq!(runtime.status().input.frames_filled, 8);
}

// ===========================================================================
// b) 欠载 → 整块静音 + 游标前进
// ===========================================================================

#[test]
fn underrun_outputs_silence_and_advances_play_cursor() {
    let (mut runtime, handle) = runtime_for(1, 4);
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let first = runtime.run_block().expect("first block").to_vec();
    assert_bits(&first, &decoded_all(&[1000, 2000, 3000, 4000]));

    // 数据耗尽：整块静音，underuruns +1，播放游标前进。
    let underrun = runtime.run_block().expect("underrun block").to_vec();
    assert!(
        underrun.iter().all(|sample| sample.to_bits() == 0),
        "underrun must output a silent block"
    );
    let status = runtime.status();
    assert_eq!(status.blocks, 2);
    assert_eq!(status.input.underruns, 1);
    assert_eq!(status.state, RuntimeState::Running);

    // 游标前进的证据：再灌一个"本应已播过"的时间戳（媒体帧 4..8）的包，必须按 late 丢弃。
    // 若游标没前进（仍停在 4），这个包会被接受而不是 late。
    handle.send(packet(1, 4, &[5000, 6000, 7000, 8000]));
    wait_for_packets(&runtime, 2);
    let silent = runtime
        .run_block()
        .expect("block after late packet")
        .to_vec();
    assert!(silent.iter().all(|sample| sample.to_bits() == 0));
    let status = runtime.status();
    assert_eq!(
        status.input.late, 1,
        "late packet proves the cursor advanced"
    );
    assert_eq!(status.input.underruns, 2);
    assert_eq!(status.blocks, 3);
}

#[test]
fn user_mute_takes_effect_at_block_boundary() {
    const SIGNAL: [i32; 16] = [
        1_000_000, -1_000_000, 500_000, -500_000, 42, -42, 7, -7, 0, 0, 100, -100, 3, -3, 9, -9,
    ];
    let (mut runtime, handle) = runtime_for(2, 8);

    // 第一块：未静音，输出等于输入编码。
    handle.send(packet(0, 0, &SIGNAL));
    wait_for_packets(&runtime, 1);
    runtime.run_block().expect("first block");
    assert!(!runtime.muted());
    let expected = {
        let mut bytes = vec![0u8; SIGNAL.len() * BYTES_PER_SAMPLE];
        encode_s24_3le_block(&decoded_all(&SIGNAL), &mut bytes).expect("encode");
        bytes
    };
    let captured = runtime
        .backend()
        .output()
        .expect("fake pcm")
        .captured
        .clone();
    assert_eq!(captured, expected);

    // 请求静音：块边界生效（回执 mute=true，但统计要等下一块）。
    let receipt = runtime.request_mute(true);
    assert!(receipt.mute);
    assert!(!runtime.muted(), "mute applies at the next block boundary");
    handle.send(packet(1, 8, &SIGNAL));
    wait_for_packets(&runtime, 2);
    runtime.run_block().expect("muted block");
    assert!(runtime.muted());
    assert!(runtime.status().muted);
    let captured = runtime
        .backend()
        .output()
        .expect("fake pcm")
        .captured
        .clone();
    assert!(
        captured.iter().all(|&byte| byte == 0),
        "muted block must write silent S24_3LE bytes"
    );

    // 解除静音：同样在下一块边界恢复。
    runtime.request_mute(false);
    handle.send(packet(2, 16, &SIGNAL));
    wait_for_packets(&runtime, 3);
    runtime.run_block().expect("unmuted block");
    assert!(!runtime.muted());
    let captured = runtime
        .backend()
        .output()
        .expect("fake pcm")
        .captured
        .clone();
    assert_eq!(captured, expected);
    assert_eq!(runtime.status().blocks, 3);
    assert_eq!(runtime.status().output.blocks_written, 3);
}

// ===========================================================================
// c) 端到端：RTP 帧 → identity 方案 → S24_3LE 逐字节
// ===========================================================================

#[test]
fn end_to_end_rtp_through_identity_plan_to_s24_3le_bytes() {
    const SIGNAL: [i32; 16] = [
        0, 1, -1, 8_388_607, -8_388_608, 123_456, -123_456, 2_097_151, -2_097_152, 42, -42,
        1_000_000, -1_000_000, 7, -7, 3_333_333,
    ];
    let (mut runtime, handle) = runtime_for(2, 8);
    let plan = identity_plan(2, 8, SAMPLE_RATE);
    let plan_bytes = plan.encode();
    let draft = runtime
        .prepare_plan(&plan_bytes, &AdmissionLimits::default())
        .expect("prepare identity plan");
    assert_eq!(draft.version(), 1);
    assert_eq!(draft.graph_id(), plan.graph_id_hex());
    assert_eq!(draft.latency_samples(), 0);
    let applied_block = runtime.commit_plan(draft).expect("commit identity plan");
    assert_eq!(
        applied_block, 1,
        "commit stages the graph for the next block"
    );
    assert!(runtime.status().switch_pending);
    assert_eq!(runtime.status().graph.version, None);

    handle.send(packet(0, 0, &SIGNAL));
    wait_for_packets(&runtime, 1);
    let block = runtime.run_block().expect("block").to_vec();
    assert_bits(&block, &decoded_all(&SIGNAL));

    let status = runtime.status();
    assert_eq!(status.graph.version, Some(1));
    assert_eq!(status.graph.epoch, Some(1));
    assert_eq!(status.graph.applied_block, Some(1));
    assert!(!status.switch_pending);
    assert_eq!(status.blocks, 1);
    assert_eq!(status.output.blocks_written, 1);
    assert_eq!(status.output.bytes_written, (SIGNAL.len() * 3) as u64);

    // 逐字节：输出 = 输入样本的 S24_3LE 编码（独立公式校验小端三字节）。
    let captured = runtime
        .backend()
        .output()
        .expect("fake pcm")
        .captured
        .clone();
    let mut expected = vec![0u8; SIGNAL.len() * BYTES_PER_SAMPLE];
    encode_s24_3le_block(&decoded_all(&SIGNAL), &mut expected).expect("encode");
    assert_eq!(captured.as_slice(), expected.as_slice());
    for (index, &raw) in SIGNAL.iter().enumerate() {
        let masked = (raw as u32) & 0x00ff_ffff;
        let offset = index * BYTES_PER_SAMPLE;
        assert_eq!(
            &captured[offset..offset + BYTES_PER_SAMPLE],
            &[
                (masked & 0xff) as u8,
                ((masked >> 8) & 0xff) as u8,
                ((masked >> 16) & 0xff) as u8,
            ],
            "sample {index} little-endian packing"
        );
    }
}

// ===========================================================================
// d) 未加载 / 未 commit 的行为
// ===========================================================================

#[test]
fn unloaded_policy_passthrough_until_plan_committed() {
    let (mut runtime, handle) = runtime_for(1, 4);
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let first = runtime.run_block().expect("block").to_vec();
    assert_bits(&first, &decoded_all(&[1000, 2000, 3000, 4000]));
    let status = runtime.status();
    assert_eq!(status.graph, GraphInfo::default(), "no plan applied yet");
    assert!(!status.switch_pending);

    // prepare 不改变正在跑的音频，也不 stage。
    let plan = identity_plan(1, 4, SAMPLE_RATE);
    let plan_bytes = plan.encode();
    let draft = runtime
        .prepare_plan(&plan_bytes, &AdmissionLimits::default())
        .expect("prepare");
    assert_eq!(draft.version(), 1);
    assert_eq!(runtime.status().graph, GraphInfo::default());
    assert!(!runtime.status().switch_pending);

    handle.send(packet(1, 4, &[5000, 6000, 7000, 8000]));
    wait_for_packets(&runtime, 2);
    let second = runtime.run_block().expect("block").to_vec();
    assert_bits(&second, &decoded_all(&[5000, 6000, 7000, 8000]));
    assert_eq!(
        runtime.status().graph,
        GraphInfo::default(),
        "prepare alone never switches"
    );

    // commit 仍在下一次 run_block 入口生效。
    let applied = runtime.commit_plan(draft).expect("commit");
    assert_eq!(applied, 3);
    assert!(runtime.status().switch_pending);
    assert_eq!(runtime.status().graph.version, None);
    handle.send(packet(2, 8, &[9000, 10000, 11000, 12000]));
    wait_for_packets(&runtime, 3);
    let third = runtime.run_block().expect("block").to_vec();
    assert_bits(&third, &decoded_all(&[9000, 10000, 11000, 12000]));
    let status = runtime.status();
    assert_eq!(status.graph.version, Some(1));
    assert_eq!(status.graph.applied_block, Some(3));
    assert!(!status.switch_pending);
}

#[test]
fn silence_policy_outputs_zeros_until_plan_applied() {
    let mut config = runtime_config(1, 4);
    config.unloaded_policy = UnloadedPolicy::Silence;
    let (source, handle) = fake_source();
    let mut runtime = Runtime::<FakePcm>::with_source(config, source).expect("runtime");
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let block = runtime.run_block().expect("block").to_vec();
    assert!(
        block.iter().all(|sample| sample.to_bits() == 0),
        "Silence policy must output silence before a plan is applied"
    );
    assert_eq!(runtime.status().graph.version, None);

    let plan = identity_plan(1, 4, SAMPLE_RATE);
    runtime
        .apply_plan(&plan.encode(), &AdmissionLimits::default())
        .expect("apply identity plan");
    handle.send(packet(1, 4, &[5000, 6000, 7000, 8000]));
    wait_for_packets(&runtime, 2);
    let block = runtime.run_block().expect("block").to_vec();
    assert_bits(&block, &decoded_all(&[5000, 6000, 7000, 8000]));
    assert_eq!(runtime.status().graph.version, Some(1));
}

#[test]
fn prepare_and_commit_keep_slot_engine_semantics() {
    let (mut runtime, handle) = runtime_for(1, 4);
    let plan = identity_plan(1, 4, SAMPLE_RATE);
    let plan_bytes = plan.encode();

    // 解码失败 → ERR_RUNTIME_PLAN。
    assert_eq!(
        runtime
            .prepare_plan(&[0u8; 8], &AdmissionLimits::default())
            .expect_err("bad plan bytes")
            .code(),
        "ERR_RUNTIME_PLAN"
    );
    // 准入拒绝 → admission_rejected（复用 PlanGraph::compile 内部的 admit，不重复实现）。
    let wide = identity_plan(2, 4, SAMPLE_RATE);
    let narrow_limits = AdmissionLimits {
        max_lanes: 1,
        ..AdmissionLimits::default()
    };
    let error = runtime
        .prepare_plan(&wide.encode(), &narrow_limits)
        .expect_err("admission rejected");
    assert_eq!(error.code(), "admission_rejected");
    assert!(matches!(&error, RuntimeError::Admission(_)));

    // 同一版本的两份草稿：commit 后另一份变成过期草稿。
    let draft_a = runtime
        .prepare_plan(&plan_bytes, &AdmissionLimits::default())
        .expect("prepare a");
    let draft_b = runtime
        .prepare_plan(&plan_bytes, &AdmissionLimits::default())
        .expect("prepare b");
    assert_eq!(draft_a.version(), 1);
    assert_eq!(draft_b.version(), 1);
    assert_eq!(runtime.commit_plan(draft_b).expect("commit b"), 1);
    handle.send(packet(0, 0, &[1, 2, 3, 4]));
    wait_for_packets(&runtime, 1);
    let _ = runtime.run_block().expect("block");
    assert_eq!(runtime.status().graph.version, Some(1));

    let error = runtime.commit_plan(draft_a).expect_err("stale draft");
    assert_eq!(error.code(), "engine_token_invalid");
    assert_eq!(
        runtime.status().graph.version,
        Some(1),
        "stale draft changed nothing"
    );

    // 下一版本正常提交，块边界换图后 epoch 递增。
    let draft_c = runtime
        .prepare_plan(&plan_bytes, &AdmissionLimits::default())
        .expect("prepare c");
    assert_eq!(draft_c.version(), 2);
    assert_eq!(runtime.commit_plan(draft_c).expect("commit c"), 2);
    handle.send(packet(1, 4, &[5, 6, 7, 8]));
    wait_for_packets(&runtime, 2);
    let _ = runtime.run_block().expect("block");
    let status = runtime.status();
    assert_eq!(status.graph.version, Some(2));
    assert_eq!(status.graph.epoch, Some(2));
    assert_eq!(status.graph.applied_block, Some(2));
}

// ===========================================================================
// e) 设备监督器状态 × RuntimeStatus
// ===========================================================================

#[derive(Clone, Default)]
struct ProbeState {
    present: Arc<AtomicBool>,
    control_calls: Arc<AtomicU32>,
}

impl ProbeState {
    fn set_present(&self, present: bool) {
        self.present.store(present, Ordering::Relaxed);
    }
}

struct TestProbe {
    device_id: DeviceId,
    state: ProbeState,
}

impl DeviceProbe for TestProbe {
    fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    fn is_present(&mut self) -> bool {
        self.state.present.load(Ordering::Relaxed)
    }

    fn bring_up_control(&mut self) -> Result<(), ProbeError> {
        self.state.control_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct RateState {
    device_rate: Arc<AtomicU32>,
}

struct TestRateControl {
    state: RateState,
}

impl RateControl for TestRateControl {
    fn read_rate(&mut self) -> Result<u32, ProbeError> {
        Ok(self.state.device_rate.load(Ordering::Relaxed))
    }

    fn assert_rate(&mut self, rate: u32) -> Result<u32, ProbeError> {
        self.state.device_rate.store(rate, Ordering::Relaxed);
        Ok(rate)
    }
}

#[derive(Clone, Default)]
struct StreamState {
    open_calls: Arc<AtomicU32>,
    close_calls: Arc<AtomicU32>,
    last_rate: Arc<AtomicU32>,
}

struct TestStream {
    state: StreamState,
}

impl StreamControl for TestStream {
    fn open_stream(&mut self, rate: u32) -> Result<(), ProbeError> {
        self.state.open_calls.fetch_add(1, Ordering::Relaxed);
        self.state.last_rate.store(rate, Ordering::Relaxed);
        Ok(())
    }

    fn close_stream(&mut self) -> Result<(), ProbeError> {
        self.state.close_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct TestClock {
    now_ms: Arc<AtomicU64>,
}

impl ClockSource for TestClock {
    fn now_ms(&mut self) -> u64 {
        self.now_ms.load(Ordering::Relaxed)
    }
}

#[test]
fn device_supervisor_transitions_are_visible_in_runtime_status() {
    const DEVICE_ID: &str = "runtime-test-device-supervisor";

    let probe_state = ProbeState::default();
    let rate_state = RateState::default();
    rate_state.device_rate.store(SAMPLE_RATE, Ordering::Relaxed);
    let stream_state = StreamState::default();
    let clock = TestClock::default();
    let supervisor = DeviceSupervisor::with_components(
        SAMPLE_RATE,
        SupervisorPolicy::default(),
        TestProbe {
            device_id: DeviceId::new(DEVICE_ID),
            state: probe_state.clone(),
        },
        TestRateControl {
            state: rate_state.clone(),
        },
        TestStream {
            state: stream_state.clone(),
        },
        clock,
    );

    let mut config = runtime_config(1, 4);
    config.safety = SafetyPolicy {
        max_consecutive_underruns: 1000,
        max_consecutive_overruns: 1000,
    };
    let (source, handle) = fake_source();
    let mut runtime =
        Runtime::<FakePcm>::with_source_and_driver(config, source, Box::new(supervisor))
            .expect("runtime");

    // 初始：设备不在总线上，未接任何块。
    let status = runtime.status();
    assert_eq!(status.device.state, LifecycleState::Absent);
    assert_eq!(status.state, RuntimeState::Idle);
    assert_eq!(status.device_rate, None);
    assert_eq!(status.plan_rate, SAMPLE_RATE);
    assert!(!status.rate_asserted);
    assert!(status.last_error.is_none());
    assert!(status.output_started);
    assert!(status.receiver.running);
    let status = runtime.poll_device();
    assert_eq!(status.state, LifecycleState::Absent);

    // 设备出现：枚举 → 控制通道 → 速率断言（连续回读一致）→ 按方案速率开流。
    probe_state.set_present(true);
    let mut states = Vec::new();
    let mut reached_streaming = false;
    for _ in 0..16 {
        let polled = runtime.poll_device();
        states.push(polled.state);
        if polled.state == LifecycleState::Streaming {
            reached_streaming = true;
            break;
        }
    }
    assert!(
        reached_streaming,
        "supervisor did not reach Streaming: {states:?}"
    );
    assert!(states.contains(&LifecycleState::Enumerated));
    assert!(states.contains(&LifecycleState::ControlReady));
    assert!(states.contains(&LifecycleState::RateAsserted));
    assert_eq!(stream_state.open_calls.load(Ordering::Relaxed), 1);
    assert_eq!(stream_state.last_rate.load(Ordering::Relaxed), SAMPLE_RATE);

    let status = runtime.status();
    assert_eq!(status.device.state, LifecycleState::Streaming);
    assert_eq!(status.device_rate, Some(SAMPLE_RATE));
    assert_eq!(status.device.device_rate, status.device_rate);
    assert_eq!(status.plan_rate, SAMPLE_RATE);
    assert_eq!(status.device.plan_rate, status.plan_rate);
    assert!(status.rate_asserted);
    assert_eq!(status.device.rate_asserted, status.rate_asserted);
    assert!(status.device.last_error.is_none());
    assert!(status.last_error.is_none());
    assert!(matches!(
        DeviceGuard::acquire(DEVICE_ID),
        Err(OwnerError::AlreadyOwned(_))
    ));

    // 流式下正常出块：安全监督器进 Running，输出非静音。
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let block = runtime.run_block().expect("block").to_vec();
    assert!(block.iter().any(|sample| sample.to_bits() != 0));
    assert_eq!(runtime.status().state, RuntimeState::Running);

    // 拔掉：Streaming → Absent，清速率状态，fail-closed 安全静音。
    probe_state.set_present(false);
    let polled = runtime.poll_device();
    assert_eq!(polled.state, LifecycleState::Absent);
    assert_eq!(stream_state.close_calls.load(Ordering::Relaxed), 1);
    let status = runtime.status();
    assert_eq!(status.device.state, LifecycleState::Absent);
    assert_eq!(status.device_rate, None);
    assert!(!status.rate_asserted);
    assert_eq!(status.state, RuntimeState::Faulted);
    assert!(runtime.safety().requires_mute());
    assert_eq!(
        status.device.last_error.as_ref().map(|error| error.code),
        Some(SupervisorErrorCode::DeviceAbsent)
    );
    assert_eq!(
        status.last_error,
        Some(SupervisorErrorCode::DeviceAbsent.as_str())
    );
    // Absent 时监督器释放设备所有权。
    let released = DeviceGuard::acquire(DEVICE_ID).expect("owner released while absent");
    drop(released);

    // 安全静音块：即使输入有数据，输出也必须整块为 0。
    handle.send(packet(1, 4, &[5000, 6000, 7000, 8000]));
    wait_for_packets(&runtime, 2);
    let muted = runtime.run_block().expect("muted block").to_vec();
    assert!(
        muted.iter().all(|sample| sample.to_bits() == 0),
        "faulted runtime must output silence"
    );
    assert_eq!(runtime.status().blocks, 2);

    // 插回：整条链路重跑（重新枚举/断言/开流），设备故障清除，恢复出声。
    probe_state.set_present(true);
    let mut recovered = false;
    for _ in 0..16 {
        if runtime.poll_device().state == LifecycleState::Streaming {
            recovered = true;
            break;
        }
    }
    assert!(recovered, "supervisor did not recover after replug");
    let status = runtime.status();
    assert_eq!(status.device.state, LifecycleState::Streaming);
    assert!(status.rate_asserted);
    assert!(status.device.last_error.is_none());
    assert!(status.last_error.is_none());
    assert_eq!(status.state, RuntimeState::Running);
    assert_eq!(stream_state.open_calls.load(Ordering::Relaxed), 2);
    handle.send(packet(2, 8, &[9000, 10000, 11000, 12000]));
    wait_for_packets(&runtime, 3);
    let block = runtime.run_block().expect("block after recovery").to_vec();
    assert!(block.iter().any(|sample| sample.to_bits() != 0));
}

// ===========================================================================
// g) 收包线程 stop / join
// ===========================================================================

#[test]
fn receiver_stop_joins_thread_and_is_idempotent() {
    let (mut runtime, handle) = runtime_for(1, 4);
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let block = runtime.run_block().expect("block").to_vec();
    assert_eq!(block.len(), 4);
    assert!(runtime.receiver_running());

    let stats = runtime.stop_receiver().expect("stop joins receiver thread");
    assert!(!stats.running);
    assert!(!runtime.receiver_running());
    assert_eq!(stats.packets_received, 1);

    // 幂等：第二次 stop 立即返回同一份统计。
    let again = runtime.stop_receiver().expect("second stop");
    assert_eq!(again.packets_received, 1);
    assert!(!again.running);

    // stop 之后块循环仍可跑：没有新包 → 欠载整块静音，不 panic。
    let silent = runtime.run_block().expect("block after stop").to_vec();
    assert!(silent.iter().all(|sample| sample.to_bits() == 0));
    drop(runtime);

    // 未显式 stop 时，Drop 负责 join，测试不挂即通过。
    let (runtime, _source) = runtime_for(1, 4);
    drop(runtime);
}

// ===========================================================================
// UDP socket 错误码（不跑真实音频）
// ===========================================================================

#[test]
fn recovered_underrun_fault_is_cleared_only_explicitly() {
    let (mut runtime, handle) = runtime_for(1, 4);
    // 没有包：连续欠载触发安全故障（默认阈值 3）。
    for _ in 0..3 {
        let _ = runtime.run_block().expect("block");
    }
    // 欠载故障先作为待上报故障，控制路径 poll 一次才落到安全监督器（与 rt-host 一致）。
    runtime.poll_device();
    assert_eq!(runtime.status().state, RuntimeState::Faulted);

    // 恢复输入：喂够一个块，欠载计数清零；故障仍锁存（不自动恢复）。
    for index in 0..4u16 {
        handle.send(packet(index, u32::from(index) * 4, &[100, 200, 300, 400]));
    }
    wait_for_packets(&runtime, 4);
    for _ in 0..3 {
        let _ = runtime.run_block().expect("filled block");
    }
    assert_eq!(runtime.status().state, RuntimeState::Faulted);

    // 显式清除后才能恢复 Running；再次调用返回 false。
    assert!(
        runtime.clear_recovered_underrun_fault(),
        "{:?}",
        runtime.status()
    );
    assert_eq!(runtime.status().state, RuntimeState::Running);
    assert!(!runtime.clear_recovered_underrun_fault());
}
#[test]
fn udp_source_reports_stable_socket_error_codes() {
    // bind 失败：先占住一个端口，再让 UdpPacketSource 绑同一个端口。
    let blocker = UdpSocket::bind("0.0.0.0:0").expect("bind blocker");
    let blocked_port = blocker.local_addr().expect("blocker addr").port();
    let mut input = runtime_config(1, 4).input;
    input.port = blocked_port;
    let error =
        UdpPacketSource::bind(&input, &ReceiverConfig::default()).expect_err("port already in use");
    assert_eq!(error.code, codes::RECEIVER_BIND);
    assert!(
        error.message.contains("bind udp"),
        "unexpected message: {}",
        error.message
    );
    drop(blocker);

    // IO I2b：单播目的地址不 join 组播，直接 bind 该本端地址（端口 0 = 临时端口）。
    let mut unicast = runtime_config(1, 4).input;
    unicast.port = 0;
    unicast.multicast_addr = Ipv4Addr::new(127, 0, 0, 1);
    let source = UdpPacketSource::bind(&unicast, &ReceiverConfig::default())
        .expect("unicast bind must succeed");
    assert_eq!(
        source.socket_addr().expect("local addr").ip().to_string(),
        "127.0.0.1"
    );
}

#[test]
fn runtime_new_maps_receiver_bind_error() {
    let blocker = UdpSocket::bind("0.0.0.0:0").expect("bind blocker");
    let blocked_port = blocker.local_addr().expect("blocker addr").port();
    let mut config = runtime_config(1, 4);
    config.input.port = blocked_port;
    let error = Runtime::<FakePcm>::new(config).expect_err("bind must fail");
    assert_eq!(error.code(), codes::RECEIVER_BIND);
}

// ===========================================================================
// 配置自校验 + 初始状态
// ===========================================================================

#[test]
fn runtime_config_validation_fails_closed() {
    let config = runtime_config(2, 8);
    config.validate().expect("valid config");

    let mut block_mismatch = config.clone();
    block_mismatch.input.block_frames = 4;
    assert_eq!(
        block_mismatch.validate().expect_err("block").code(),
        codes::CONFIG
    );

    let mut rate_mismatch = config.clone();
    rate_mismatch.device.sample_rate = 44_100;
    assert_eq!(
        rate_mismatch.validate().expect_err("rate").code(),
        codes::CONFIG
    );

    let mut channel_mismatch = config.clone();
    channel_mismatch.engine.output_channels = 4;
    assert_eq!(
        channel_mismatch.validate().expect_err("channels").code(),
        codes::CONFIG
    );

    let mut packet_too_small = config.clone();
    packet_too_small.receiver.packet_bytes = 12;
    assert_eq!(
        packet_too_small.validate().expect_err("packet").code(),
        codes::CONFIG
    );

    let mut empty_queue = config.clone();
    empty_queue.receiver.pending_packets = 0;
    assert_eq!(
        empty_queue.validate().expect_err("queue").code(),
        codes::CONFIG
    );

    let mut enabled_without_commands = config.clone();
    enabled_without_commands.supervisor.enabled = true;
    assert_eq!(
        enabled_without_commands
            .validate()
            .expect_err("supervisor commands")
            .code(),
        codes::CONFIG
    );

    let mut empty_device = config.clone();
    empty_device.device.device = DeviceId::new("");
    assert_eq!(
        empty_device.validate().expect_err("device").code(),
        codes::CONFIG
    );

    // 未加载方案时 RuntimeStatus 的初始字段（NoDeviceDriver 如实报 Absent）。
    let (runtime, _handle) = runtime_for(1, 4);
    let status = runtime.status();
    assert_eq!(status.state, RuntimeState::Idle);
    assert_eq!(status.blocks, 0);
    assert_eq!(status.input.packets, 0);
    assert_eq!(status.output, AlsaStats::default());
    assert_eq!(status.device.state, LifecycleState::Absent);
    assert_eq!(status.device_rate, None);
    assert_eq!(status.plan_rate, SAMPLE_RATE);
    assert!(!status.rate_asserted);
    assert_eq!(status.graph, GraphInfo::default());
    assert!(!status.switch_pending);
    assert_eq!(status.last_error, None);
    assert!(status.output_started);
    assert!(status.receiver.running);
    assert!(
        !status.receiver.rcvbuf_applied,
        "SO_RCVBUF not wired on this platform"
    );
}

#[test]
fn runtime_config_allows_input_and_output_channel_widths_to_differ() {
    // 2 输入 → 22 输出：输入/引擎/设备各自的宽度按方向分别校验，不再要求相等。
    let mut split = runtime_config(2, 8);
    split.engine.output_channels = 22;
    split.device.output_channels = 22;
    split.validate().expect("2-in / 22-out runtime config");

    // 输入方向仍必须一致：engine.input_channels 改了但 Aes67/设备仍 2。
    let mut input_mismatch = split.clone();
    input_mismatch.engine.input_channels = 4;
    assert_eq!(
        input_mismatch.validate().expect_err("input width").code(),
        codes::CONFIG
    );

    // 输出方向也必须一致：engine.output_channels 改了但设备仍 22。
    let mut output_mismatch = split.clone();
    output_mismatch.engine.output_channels = 21;
    assert_eq!(
        output_mismatch.validate().expect_err("output width").code(),
        codes::CONFIG
    );
}

// ===========================================================================
// f) 稳态零分配（独立子进程计量）
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

const ZERO_ALLOC_CHILD_ENV: &str = "KHAEN_RUNTIME_ZERO_ALLOC_CHILD";
const ZERO_ALLOC_TEST_NAME: &str = "run_block_performs_no_heap_allocation";

/// 父进程：同二进制其它测试线程会并发分配，全局计数不干净；改为只跑本用例的子进程计量。
fn run_zero_alloc_child() {
    let exe = std::env::current_exe().expect("current test executable");
    let output = std::process::Command::new(exe)
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

fn zero_alloc_child_body() {
    // 构造期分配（线程/缓冲/方案编译）全部在计量窗口之外完成。
    let (source, handle) = fake_source();
    let mut runtime =
        Runtime::<FakePcm>::with_source(runtime_config(2, 8), source).expect("runtime");
    let signal = wave(16);
    for sequence in 0..32u16 {
        let timestamp = u32::from(sequence) * 8;
        handle.send(packet(sequence, timestamp, &signal));
    }
    thread::sleep(Duration::from_millis(50));

    // 预热：让收包线程、SPSC、图进入稳态。
    for _ in 0..2 {
        let _ = runtime.run_block().expect("warmup block");
    }
    let plan = identity_plan(2, 8, SAMPLE_RATE);
    let draft = runtime
        .prepare_plan(&plan.encode(), &AdmissionLimits::default())
        .expect("prepare");
    runtime.commit_plan(draft).expect("commit");
    let _ = runtime.run_block().expect("apply switch");

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..16 {
        let block = runtime.run_block().expect("steady-state block");
        assert_eq!(block.len(), 16);
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocated, 0,
        "run_block allocated {allocated} times in steady state (plan switch included)"
    );
    assert!(runtime.status().blocks >= 19);
}

#[test]
fn run_block_performs_no_heap_allocation() {
    if std::env::var_os(ZERO_ALLOC_CHILD_ENV).is_none() {
        run_zero_alloc_child();
        return;
    }
    zero_alloc_child_body();
}

/// 确定性的 24-bit 满量程测试信号（值域 (-0.5, 0.5)）。
fn wave(count: usize) -> Vec<i32> {
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let value = (index as i32 * 1009) % 8_388_607;
        out.push(value - 4_194_303);
    }
    out
}
// ===========================================================================
// h) 设备缺失/掉线：延迟打开 + fail-closed + 重开恢复（T42o）
// ===========================================================================

#[derive(Clone, Default)]
struct TogglePcmState {
    present: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    opens: Arc<AtomicU32>,
    writes: Arc<AtomicU32>,
    closed: Arc<AtomicU32>,
}

static TOGGLE_REGISTRY: std::sync::OnceLock<Mutex<Vec<(String, TogglePcmState)>>> =
    std::sync::OnceLock::new();

fn toggle_registry() -> &'static Mutex<Vec<(String, TogglePcmState)>> {
    TOGGLE_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn register_toggle_pcm(device: &str, state: &TogglePcmState) {
    toggle_registry()
        .lock()
        .expect("toggle registry")
        .push((device.to_string(), state.clone()));
}

fn toggle_state(device: &str) -> TogglePcmState {
    toggle_registry()
        .lock()
        .expect("toggle registry")
        .iter()
        .find(|(name, _)| name == device)
        .map(|(_, state)| state.clone())
        .expect("toggle pcm registered")
}

/// 可拔插假 PCM：present=false 时 open 失败；connected=false 时 write 报设备掉线。
struct TogglePcm {
    state: TogglePcmState,
    config: Option<DeviceConfig>,
}

impl PcmOutput for TogglePcm {
    fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
        let state = toggle_state(config.device.as_str());
        state.opens.fetch_add(1, Ordering::SeqCst);
        if !state.present.load(Ordering::SeqCst) {
            return Err(BackendError::Fault(
                "alsa pcm open failed: device absent".to_string(),
            ));
        }
        state.connected.store(true, Ordering::SeqCst);
        Ok(Self {
            state,
            config: Some(config.clone()),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
        let config = self
            .config
            .as_ref()
            .ok_or(PcmError::Fatal("toggle pcm not open"))?;
        assert_eq!(
            bytes.len(),
            config.output_samples_per_block() * BYTES_PER_SAMPLE
        );
        self.state.writes.fetch_add(1, Ordering::SeqCst);
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(PcmError::Fatal("alsa pcm disconnected"));
        }
        Ok(config.block_frames)
    }

    fn prepare(&mut self, _fault: PcmFault) -> Result<(), BackendError> {
        Ok(())
    }

    fn available(&self) -> Result<usize, PcmError> {
        Ok(self
            .config
            .as_ref()
            .map_or(0, |config| config.block_frames * 4))
    }

    fn close(&mut self) {
        self.state.closed.fetch_add(1, Ordering::SeqCst);
        self.state.connected.store(false, Ordering::SeqCst);
        self.config = None;
    }
}

fn toggle_runtime_config(device: &str) -> RuntimeConfig {
    let mut config = runtime_config(1, 4);
    config.device.device = DeviceId::new(device);
    config
}

#[test]
fn deferred_device_fails_closed_and_recovers_after_reopen() {
    let device = format!("host-test-toggle-pcm-{}", std::process::id());
    let state = TogglePcmState::default();
    register_toggle_pcm(&device, &state);
    let (source, handle) = fake_source();
    let mut runtime =
        Runtime::<TogglePcm>::with_source_deferred(toggle_runtime_config(&device), source)
            .expect("deferred runtime");
    assert!(
        !runtime.device_started(),
        "deferred constructor must not open the device"
    );
    assert!(!runtime.status().output_started);

    // 设备缺失时块路径 fail-closed：不推进块数、不写设备；方案仍可加载（版本语义保持）。
    let plan = identity_plan(1, 4, SAMPLE_RATE);
    let version = runtime
        .apply_plan(&plan.encode(), &AdmissionLimits::default())
        .expect("plan applied while device absent");
    assert_eq!(version, 1);
    // 启动时设备缺失：控制路径显式上报设备故障，状态必须是 faulted（fail-closed）。
    runtime.report_device_loss("output device unavailable at startup");
    assert_eq!(runtime.status().state, RuntimeState::Faulted);
    assert!(runtime.safety().requires_mute());
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let error = runtime
        .run_block()
        .expect_err("device absent must fail closed");
    assert!(
        matches!(error, RuntimeError::Backend(BackendError::NotStarted)),
        "{error:?}"
    );
    assert_eq!(runtime.blocks(), 0, "absent device must not advance blocks");
    assert_eq!(state.writes.load(Ordering::SeqCst), 0);
    assert!(
        runtime.open_device().is_err(),
        "device absent -> open must fail"
    );
    assert!(!runtime.device_started());

    // 设备回来：打开成功、积压旧包被丢弃、抖动缓冲重建；方案在第一块边界生效。
    state.present.store(true, Ordering::SeqCst);
    runtime.open_device().expect("reopen device");
    assert!(runtime.device_started());
    assert!(
        runtime.clear_device_loss_fault(),
        "startup device-loss fault is cleared explicitly after reopen"
    );
    assert_eq!(runtime.status().state, RuntimeState::Running);
    assert_eq!(
        runtime.input().jitter_fill(),
        0,
        "jitter buffer rebuilt empty"
    );
    handle.send(packet(1, 4, &[5000, 6000, 7000, 8000]));
    wait_for_packets(&runtime, 2);
    let block = runtime.run_block().expect("block after reopen").to_vec();
    assert_bits(&block, &decoded_all(&[5000, 6000, 7000, 8000]));
    assert_eq!(runtime.blocks(), 1);
    let graph = runtime.graph_info();
    assert_eq!(graph.version, Some(version));
    let graph_id = graph.graph_id.expect("graph id after first block");
    assert_eq!(runtime.status().state, RuntimeState::Running);
    assert_eq!(state.writes.load(Ordering::SeqCst), 1);

    // 运行中掉线：write 失败 → 关设备、进 faulted；进程不 panic，块路径继续 fail-closed。
    state.connected.store(false, Ordering::SeqCst);
    handle.send(packet(2, 8, &[1, 2, 3, 4]));
    wait_for_packets(&runtime, 3);
    let error = runtime
        .run_block()
        .expect_err("disconnect must fail closed");
    assert!(
        matches!(error, RuntimeError::Backend(BackendError::Fault(_))),
        "{error:?}"
    );
    assert_eq!(runtime.status().output_errors, 1);
    runtime.close_device().expect("close lost device");
    assert!(!runtime.device_started());
    runtime.poll_device();
    assert_eq!(runtime.status().state, RuntimeState::Faulted);

    // 设备仍不在：重开失败，块路径不写设备（fail-closed）。
    state.present.store(false, Ordering::SeqCst);
    assert!(runtime.open_device().is_err());
    let writes_after_loss = state.writes.load(Ordering::SeqCst);
    assert!(matches!(
        runtime.run_block(),
        Err(RuntimeError::Backend(BackendError::NotStarted))
    ));
    assert_eq!(state.writes.load(Ordering::SeqCst), writes_after_loss);

    // 设备回来：重开 + 显式清设备丢失故障 → running；图版本/graph_id 不变，块计数继续增长。
    state.present.store(true, Ordering::SeqCst);
    state.connected.store(true, Ordering::SeqCst);
    runtime.open_device().expect("reopen after replug");
    assert!(
        runtime.clear_device_loss_fault(),
        "device-loss fault is cleared explicitly"
    );
    assert_eq!(runtime.status().state, RuntimeState::Running);
    let graph_after = runtime.graph_info();
    assert_eq!(graph_after.version, Some(version));
    assert_eq!(graph_after.graph_id.as_deref(), Some(graph_id.as_str()));
    handle.send(packet(3, 12, &[9, 10, 11, 12]));
    wait_for_packets(&runtime, 4);
    let block = runtime
        .run_block()
        .expect("block after second reopen")
        .to_vec();
    assert_bits(&block, &decoded_all(&[9, 10, 11, 12]));
    assert_eq!(runtime.blocks(), 3, "blocks keep growing across recovery");
    assert_eq!(state.writes.load(Ordering::SeqCst), 3);
    assert!(state.opens.load(Ordering::SeqCst) >= 4);
    assert!(state.closed.load(Ordering::SeqCst) >= 1);
}

#[test]
fn open_device_restarts_jitter_prefill_after_device_loss() {
    const JITTER_TARGET: usize = 8;
    let device = format!("host-test-prefill-pcm-{}", std::process::id());
    let state = TogglePcmState::default();
    register_toggle_pcm(&device, &state);
    let mut config = toggle_runtime_config(&device);
    // 抖动目标 8 帧 > 一块 4 帧：重新打开后必须重新预填（Warmup）才能出声。
    config.input.jitter_target_frames = JITTER_TARGET;
    let (source, handle) = fake_source();
    let mut runtime =
        Runtime::<TogglePcm>::with_source_deferred(config, source).expect("deferred runtime");
    state.present.store(true, Ordering::SeqCst);
    runtime.open_device().expect("open device");

    // 第一块：只到 4/8 帧水位 → Warmup 整块静音，播放游标不前进。
    handle.send(packet(0, 0, &[1000, 2000, 3000, 4000]));
    wait_for_packets(&runtime, 1);
    let warmup = runtime.run_block().expect("warmup block").to_vec();
    assert!(
        warmup.iter().all(|sample| sample.to_bits() == 0),
        "prefill gate must output silence"
    );
    assert_eq!(runtime.status().input.underruns, 1);
    assert_eq!(runtime.input().jitter_fill(), 4);

    // 凑够水位：第二块播放第一包的 4 帧。
    handle.send(packet(1, 4, &[5000, 6000, 7000, 8000]));
    wait_for_packets(&runtime, 2);
    let block = runtime.run_block().expect("filled block").to_vec();
    assert_bits(&block, &decoded_all(&[1000, 2000, 3000, 4000]));
    assert_eq!(runtime.input().jitter_fill(), 4);

    // 运行中掉线 → 关设备、faulted；重开后抖动缓冲是全新的（预填重新开始）。
    state.connected.store(false, Ordering::SeqCst);
    handle.send(packet(2, 8, &[1, 2, 3, 4]));
    wait_for_packets(&runtime, 3);
    let error = runtime
        .run_block()
        .expect_err("disconnect must fail closed");
    assert!(
        matches!(error, RuntimeError::Backend(BackendError::Fault(_))),
        "{error:?}"
    );
    runtime.close_device().expect("close lost device");
    runtime.poll_device();
    assert_eq!(runtime.status().state, RuntimeState::Faulted);

    state.present.store(true, Ordering::SeqCst);
    state.connected.store(true, Ordering::SeqCst);
    runtime.open_device().expect("reopen after replug");
    assert!(runtime.clear_device_loss_fault());
    assert_eq!(
        runtime.input().jitter_fill(),
        0,
        "jitter buffer is empty after reopen"
    );
    // 新流锚定后只到 4/8 帧：再次 Warmup（证明预填重新开始，而不是沿用旧水位）。
    handle.send(packet(3, 500, &[1100, 2200, 3300, 4400]));
    wait_for_packets(&runtime, 4);
    let warmup = runtime.run_block().expect("warmup after reopen").to_vec();
    assert!(
        warmup.iter().all(|sample| sample.to_bits() == 0),
        "prefill must restart after device reopen"
    );
    assert_eq!(runtime.status().input.underruns, 1);
    assert_eq!(runtime.input().jitter_fill(), 4);

    handle.send(packet(4, 504, &[5500, 6600, 7700, 8800]));
    wait_for_packets(&runtime, 5);
    let block = runtime.run_block().expect("filled after reopen").to_vec();
    assert_bits(&block, &decoded_all(&[1100, 2200, 3300, 4400]));
    assert_eq!(runtime.status().state, RuntimeState::Running);
}
