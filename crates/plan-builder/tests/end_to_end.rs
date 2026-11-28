//! 端到端离线验收（T32b）：plan-builder 组装 → host Runtime（假包源 + 假 PCM）→ S24_3LE。
//!
//! 只走平台无关部分：不碰 ALSA / 真实 UDP / 网络 / 设备，Windows 本机可跑。
//! 真实 ALSA 出声音与真实 UDP 收包通路标 NOT_RUN（由 CI / 设备任务覆盖）。
//!
//! 断言策略：
//! - 恒等 / 0 dB / 路由交换这类只含二进制精确系数的通路，期望字节用**手写小端三字节**
//!   与输入的 24-bit 原始值逐字节对照；
//! - 含 dB 保护增益的通路，期望 f32 由本文件独立重算后交给 host 的公开编码器
//!   encode_s24_3le_block 编码（不复制取整 / 饱和语义），并额外用 f64 + 1 LSB 容差
//!   核对数值量级。

use std::collections::VecDeque;
use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use khaen_host::admission::AdmissionLimits;
use khaen_host::aes67_input::Aes67InputConfig;
use khaen_host::alsa_backend::{
    encode_s24_3le_block, PcmError, PcmFault, PcmOutput, BYTES_PER_SAMPLE,
};
use khaen_host::backend::{BackendError, DeviceConfig};
use khaen_host::engine::{EngineConfig, GraphInfo};
use khaen_host::owner::DeviceId;
use khaen_host::plan::Plan;
use khaen_host::runtime::{
    PacketSource, ReceiverConfig, Runtime, RuntimeConfig, RuntimeError, SupervisorOptions,
    UnloadedPolicy,
};
use khaen_host::safety::{RuntimeState, SafetyPolicy};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

const BLOCK_FRAMES: usize = 128;
const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const RTP_PAYLOAD_TYPE: u8 = 98;
const RTP_SSRC: u32 = 0x0bad_cafe;
/// 24-bit 满量程缩放：host 的 L24 解码是 raw / 2^23。
const L24_SCALE: f32 = 8_388_608.0;
/// 每块 S24_3LE 字节数：128 帧 × 2 通道 × 3 字节 = 768。
const BLOCK_BYTES: usize = BLOCK_FRAMES * CHANNELS * BYTES_PER_SAMPLE;
/// 主链路保护增益（dB）：lane0 电平 +6 dB。
const PROTECTION_GAIN_DB: f32 = 6.0;

// ===========================================================================
// 最小假件：RTP 包源 + PCM 设备（不 import host 测试私有件，本文件自足）
// ===========================================================================

/// 假 PCM 设备：把每块写进来的 S24_3LE 字节整块拷进预分配缓冲，不做任何设备 IO。
#[derive(Debug)]
struct FakePcm {
    config: Option<DeviceConfig>,
    /// 最近一次 write 收到的整块字节（逐字节断言用）。
    captured: Vec<u8>,
    /// 成功写出的块数。
    writes: u64,
}

impl PcmOutput for FakePcm {
    fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
        Ok(Self {
            config: Some(config.clone()),
            captured: vec![0u8; config.output_samples_per_block() * BYTES_PER_SAMPLE],
            writes: 0,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
        let channels = match self.config.as_ref() {
            Some(config) => config.output_channels,
            None => return Err(PcmError::Fatal("fake pcm not open")),
        };
        assert_eq!(bytes.len(), self.captured.len(), "fake PCM block size");
        self.captured.copy_from_slice(bytes);
        self.writes += 1;
        Ok(bytes.len() / (channels * BYTES_PER_SAMPLE))
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
        self.config = None;
    }
}

#[derive(Clone, Default)]
struct FakeSourceHandle {
    packets: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl FakeSourceHandle {
    /// 注入一个完整 RTP 包（非实时路径调用）。
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
// 运行时夹具：48 kHz、块 128、2 通道（RTP 2 声道 → 2 DAC），未加载策略 Silence
// ===========================================================================

fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        additional_inputs: Vec::new(),
        input: Aes67InputConfig {
            multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
            port: 5004,
            payload_type: RTP_PAYLOAD_TYPE,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            channels_map: (0..CHANNELS as u16).collect(),
            output_channels: CHANNELS,
            block_frames: BLOCK_FRAMES,
            jitter_target_frames: 0,
        },
        engine: EngineConfig {
            block_frames: BLOCK_FRAMES,
            sample_rate: SAMPLE_RATE,
            input_channels: CHANNELS,
            output_channels: CHANNELS,
        },
        device: DeviceConfig {
            device: DeviceId::new("fake-e2e-pcm"),
            sample_rate: SAMPLE_RATE,
            block_frames: BLOCK_FRAMES,
            input_channels: CHANNELS,
            output_channels: CHANNELS,
        },
        safety: SafetyPolicy::default(),
        receiver: ReceiverConfig::default(),
        supervisor: SupervisorOptions::default(),
        unloaded_policy: UnloadedPolicy::Silence,
    }
}

fn new_runtime() -> (Runtime<FakePcm>, FakeSourceHandle) {
    let (source, handle) = fake_source();
    let runtime = Runtime::<FakePcm>::with_source(runtime_config(), source).expect("runtime");
    (runtime, handle)
}

/// 确定性已知样本（非随机）：24-bit 原始值都取 2 的幂的倍数，
/// 保证 0.5 矩阵交叉点与求和都在 f32 里**精确**（给手算字节断言留出确定性）。
fn block_signal(block: u32) -> Vec<[i32; CHANNELS]> {
    (0..BLOCK_FRAMES)
        .map(|frame| {
            let index = block as usize * BLOCK_FRAMES + frame;
            let k = (index % 7) as i32 - 3;
            let m = ((index * 3 + 1) % 5) as i32 - 2;
            [k * 262_144, m * 131_072]
        })
        .collect()
}

/// 组一个 RTP 包（12 字节基础头 + L24 大端 payload，每块一包）。
fn rtp_packet(sequence: u16, block: u32, frames: &[[i32; CHANNELS]]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(12 + frames.len() * CHANNELS * BYTES_PER_SAMPLE);
    packet.push(0x80); // v=2, 无 padding/extension/CSRC
    packet.push(RTP_PAYLOAD_TYPE);
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(&(block * BLOCK_FRAMES as u32).to_be_bytes());
    packet.extend_from_slice(&RTP_SSRC.to_be_bytes());
    for frame in frames {
        for raw in frame {
            let masked = (*raw as u32) & 0x00ff_ffff;
            packet.push((masked >> 16) as u8);
            packet.push((masked >> 8) as u8);
            packet.push(masked as u8);
        }
    }
    packet
}

/// 等待收包线程把注入的包推进 SPSC（计数在 push 之后更新，看到计数即槽位可见）。
fn wait_for_packets(runtime: &Runtime<FakePcm>, expected: u64) {
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

/// 注入下一块的 RTP 包 → 等收包线程推进 SPSC → 跑一块 → 返回假设备收到的 S24_3LE 字节。
fn feed_and_run(
    runtime: &mut Runtime<FakePcm>,
    handle: &FakeSourceHandle,
    sequence: u16,
    block: u32,
    frames: &[[i32; CHANNELS]],
) -> Vec<u8> {
    handle.send(rtp_packet(sequence, block, frames));
    wait_for_packets(runtime, u64::from(sequence) + 1);
    runtime.run_block().expect("block");
    runtime
        .backend()
        .output()
        .expect("fake pcm")
        .captured
        .clone()
}

// ===========================================================================
// 方案组装：identity 基线 + 非平凡主链路
// ===========================================================================

fn plan_config() -> PlanConfig {
    PlanConfig::new(BLOCK_FRAMES as u32, SAMPLE_RATE, CHANNELS as u16)
}

fn identity_builder() -> PlanBuilder {
    PlanBuilder::identity(plan_config())
}

/// 非平凡主链路：2 通道输入 → 2 lane → 矩阵交叉点（0.5）→ lane0 保护 +6 dB → 交换路由到 2 DAC。
///
/// 行优先矩阵 [1.0, 0.5, 0.5, 1.0]：lane0 = x0 + 0.5*x1，lane1 = 0.5*x0 + x1；
/// 路由 (lane0 → DAC1, lane1 → DAC0)，因此 DAC0 走 0 dB 的 lane1、DAC1 走 +6 dB 的 lane0。
fn nontrivial_builder() -> PlanBuilder {
    PlanBuilder::new(plan_config())
        .set_content_identity(CHANNELS as u16)
        .set_crossover_identity(CHANNELS as u16)
        .set_matrix_cells(
            CHANNELS as u16,
            CHANNELS as u16,
            1,
            1,
            1,
            [1.0, 0.5, 0.5, 1.0],
        )
        .set_protection_gain_db(0, PROTECTION_GAIN_DB)
        .set_routing_map(CHANNELS as u16, [(0, 1), (1, 0)])
}

/// 与 host graph/protection 段相同的 dB → 线性增益表达式。
///
/// host 没有公开访问器，这里只镜像这一行换算（不是复制编码器：浮点 → S24 的取整/饱和
/// 一律走公开的 encode_s24_3le_block）。同一目标上两边调用同一 f32::powf，
/// 期望值与 host 逐位一致；host 若改增益律这条端到端断言会先红。
fn protection_linear_gain() -> f32 {
    10f32.powf(PROTECTION_GAIN_DB / 20.0)
}

/// 主链路方案的期望输出样本（帧交错，DAC0/DAC1）：
/// DAC0 = 0.5*x0 + x1（0 dB lane1），DAC1 = (x0 + 0.5*x1)*g（+6 dB lane0）。
fn nontrivial_expected_samples(frames: &[[i32; CHANNELS]]) -> Vec<f32> {
    let gain = protection_linear_gain();
    let mut samples = Vec::with_capacity(frames.len() * CHANNELS);
    for [raw0, raw1] in frames {
        let x0 = *raw0 as f32 / L24_SCALE;
        let x1 = *raw1 as f32 / L24_SCALE;
        samples.push(0.5f32 * x0 + x1);
        samples.push((x0 + 0.5f32 * x1) * gain);
    }
    samples
}

/// 把期望 f32 块编码成 S24_3LE 字节（调用 host 公开编码器，不复制其语义）。
fn encode_expected(samples: &[f32]) -> Vec<u8> {
    let mut bytes = vec![0u8; samples.len() * BYTES_PER_SAMPLE];
    encode_s24_3le_block(samples, &mut bytes).expect("expected samples encode");
    bytes
}

fn nontrivial_expected_bytes(frames: &[[i32; CHANNELS]]) -> Vec<u8> {
    encode_expected(&nontrivial_expected_samples(frames))
}

/// 手写 S24_3LE 小端三字节：24-bit 二进制补码的低 3 字节。
fn hand_s24_le(raw: i32) -> [u8; BYTES_PER_SAMPLE] {
    let masked = (raw as u32) & 0x00ff_ffff;
    [
        (masked & 0xff) as u8,
        ((masked >> 8) & 0xff) as u8,
        ((masked >> 16) & 0xff) as u8,
    ]
}

/// 从 S24_3LE 三字节还原 24-bit 有符号整数。
fn decode_s24_le(bytes: &[u8]) -> i32 {
    let raw = i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
    (raw << 8) >> 8
}

/// 逐字节直通期望：identity 方案下 DAC 字节 = 输入 24-bit 原始值的小端补码。
fn identity_bytes(frames: &[[i32; CHANNELS]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frames.len() * CHANNELS * BYTES_PER_SAMPLE);
    for frame in frames {
        for raw in frame {
            bytes.extend_from_slice(&hand_s24_le(*raw));
        }
    }
    bytes
}

/// 路由交换期望：帧内两个通道互换后再按直通字节写出。
fn swapped_bytes(frames: &[[i32; CHANNELS]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frames.len() * CHANNELS * BYTES_PER_SAMPLE);
    for [raw0, raw1] in frames {
        bytes.extend_from_slice(&hand_s24_le(*raw1));
        bytes.extend_from_slice(&hand_s24_le(*raw0));
    }
    bytes
}

/// 主链路 DAC0 的独立手算断言：0 dB lane1 = 0.5*x0 + x1 = (raw0/2 + raw1) / 2^23，
/// 编码后 24-bit 整数就应精确等于 raw0/2 + raw1（信号取偶数原始值，除 2 无损）。
fn assert_dac0_hand_computed(captured: &[u8], frames: &[[i32; CHANNELS]], block: u32) {
    for (frame_index, [raw0, raw1]) in frames.iter().enumerate() {
        let expected = hand_s24_le(raw0 / 2 + raw1);
        let offset = frame_index * CHANNELS * BYTES_PER_SAMPLE;
        assert_eq!(
            &captured[offset..offset + BYTES_PER_SAMPLE],
            &expected,
            "block {block} frame {frame_index}: DAC0 手算字节不符"
        );
    }
}

/// 主链路 DAC1 的数值复核（f64 独立公式 + 1 LSB 容差）：证明逐字节断言不是自我一致，
/// 而是数量级正确（+6 dB lane 的量化误差不超过 1 LSB）。
fn assert_gain_lane_within_one_lsb(captured: &[u8], frames: &[[i32; CHANNELS]], block: u32) {
    let gain = 10f64.powf(f64::from(PROTECTION_GAIN_DB) / 20.0);
    let scale = f64::from(L24_SCALE);
    for (frame_index, [raw0, raw1]) in frames.iter().enumerate() {
        let lane0 = f64::from(*raw0) / scale + 0.5 * (f64::from(*raw1) / scale);
        let expected = lane0 * gain * scale;
        let offset = (frame_index * CHANNELS + 1) * BYTES_PER_SAMPLE;
        let actual = f64::from(decode_s24_le(&captured[offset..offset + BYTES_PER_SAMPLE]));
        assert!(
            (actual - expected).abs() <= 1.0,
            "block {block} frame {frame_index}: DAC1 = {actual} 与 f64 期望 {expected} 超过 1 LSB"
        );
    }
}

// ===========================================================================
// 1) 主链路：PlanBuilder 字节 → Runtime（假源 + 假 PCM）→ 假设备 S24_3LE 逐字节
// ===========================================================================

#[test]
fn plan_bytes_drive_runtime_to_expected_s24_3le_bytes() {
    let limits = AdmissionLimits::default();
    let plan_bytes = nontrivial_builder().build_bytes().expect("plan build");
    let plan = Plan::decode(&plan_bytes).expect("plan decode");
    assert_eq!(plan.stage_count(), 5, "主链路是五段完整方案");
    let graph_id = plan.graph_id_hex();

    let (mut runtime, handle) = new_runtime();
    assert_eq!(
        runtime.status().graph,
        GraphInfo::default(),
        "构造后尚未加载方案"
    );

    let applied = runtime
        .apply_plan(&plan_bytes, &limits)
        .expect("apply plan succeeds");
    assert_eq!(applied, 1, "第一块入口换图");
    let staged = runtime.status();
    assert!(staged.switch_pending, "apply 只 stage，块边界才生效");
    assert_eq!(staged.graph.version, None, "换图前状态不得谎报已生效");

    for block in 0..3u32 {
        let frames = block_signal(block);
        let captured = feed_and_run(&mut runtime, &handle, block as u16, block, &frames);
        assert_eq!(captured.len(), BLOCK_BYTES);
        assert_eq!(
            captured,
            nontrivial_expected_bytes(&frames),
            "block {block}: 假设备收到的 S24_3LE 必须与期望逐字节相等"
        );
        assert_dac0_hand_computed(&captured, &frames, block);
        assert_gain_lane_within_one_lsb(&captured, &frames, block);
    }

    let status = runtime.status();
    assert_eq!(status.state, RuntimeState::Running);
    assert_eq!(status.blocks, 3);
    assert_eq!(status.graph.version, Some(1));
    assert_eq!(status.graph.epoch, Some(1));
    assert_eq!(
        status.graph.applied_block,
        Some(1),
        "换图发生在第 1 块入口（applied_block 语义）"
    );
    assert_eq!(status.graph.graph_id.as_deref(), Some(graph_id.as_str()));
    assert!(!status.switch_pending);
    assert!(status.output_started);
    assert_eq!(status.output.blocks_written, 3);
    assert_eq!(status.output.bytes_written, (3 * BLOCK_BYTES) as u64);
    assert_eq!(
        runtime.backend().output().expect("fake pcm").writes,
        3,
        "每块一次完整 PCM 写"
    );
    assert_eq!(status.input.frames_filled, (3 * BLOCK_FRAMES) as u64);
    assert_eq!(status.input.underruns, 0, "输入块一直有效，零输出不是欠载");
}

// ===========================================================================
// 2) 未加载方案：Silence 策略 fail-closed 全零 S24 + graph 为空
// ===========================================================================

#[test]
fn unloaded_plan_outputs_silence_and_reports_empty_graph() {
    let (mut runtime, handle) = new_runtime();
    assert_eq!(runtime.config().unloaded_policy, UnloadedPolicy::Silence);
    assert_eq!(runtime.status().graph, GraphInfo::default());

    let frames = block_signal(0);
    let captured = feed_and_run(&mut runtime, &handle, 0, 0, &frames);
    assert_eq!(captured.len(), BLOCK_BYTES);
    assert!(
        captured.iter().all(|byte| *byte == 0),
        "Silence 策略必须 fail-closed 输出全零 S24_3LE"
    );

    let status = runtime.status();
    assert_eq!(
        status.graph,
        GraphInfo::default(),
        "未 apply 方案时没有生效图"
    );
    assert!(!status.switch_pending);
    assert_eq!(status.blocks, 1);
    assert_eq!(status.output.blocks_written, 1);
    assert_eq!(status.output.bytes_written, BLOCK_BYTES as u64);
    // 输入块是有效的（不是欠载静音）：零输出来自"未加载方案"策略本身。
    assert_eq!(status.input.frames_filled, BLOCK_FRAMES as u64);

    // 加载 identity 后下一块恢复直通字节，证明零输出不是通路坏了。
    let plan_bytes = identity_builder().build_bytes().expect("identity bytes");
    assert_eq!(
        runtime
            .apply_plan(&plan_bytes, &AdmissionLimits::default())
            .expect("apply identity"),
        2
    );
    let frames = block_signal(1);
    let captured = feed_and_run(&mut runtime, &handle, 1, 1, &frames);
    assert_eq!(captured, identity_bytes(&frames));
    assert_eq!(runtime.status().graph.version, Some(1));
}

// ===========================================================================
// 3) 块边界换图：A（identity）→ B（路由交换）在下一块入口生效
// ===========================================================================

#[test]
fn plan_switch_applies_at_next_block_boundary() {
    let limits = AdmissionLimits::default();
    let plan_a = identity_builder().build_bytes().expect("plan A");
    let plan_b = identity_builder()
        .set_routing_map(CHANNELS as u16, [(0, 1), (1, 0)])
        .build_bytes()
        .expect("plan B");
    let (mut runtime, handle) = new_runtime();

    assert_eq!(runtime.apply_plan(&plan_a, &limits).expect("apply A"), 1);
    let frames = block_signal(7);
    let first = feed_and_run(&mut runtime, &handle, 0, 0, &frames);
    assert_eq!(first, identity_bytes(&frames), "A 生效块逐字节直通");
    let after_a = runtime.status();
    assert_eq!(after_a.graph.version, Some(1));
    assert_eq!(after_a.graph.applied_block, Some(1));
    assert!(!after_a.switch_pending);

    // 运行中 prepare+commit B：只登记、不打断当前块，状态快照仍如实报告 A。
    assert_eq!(runtime.apply_plan(&plan_b, &limits).expect("apply B"), 2);
    let staged = runtime.status();
    assert!(staged.switch_pending, "B 已 stage 等块边界");
    assert_eq!(
        staged.graph.version,
        Some(1),
        "块边界之前仍是 A（不得谎报 B 已生效）"
    );
    assert_eq!(staged.graph.applied_block, Some(1));

    // 同一输入在下一块边界换成 B：输出按路由交换，与上一块不同。
    let second = feed_and_run(&mut runtime, &handle, 1, 1, &frames);
    assert_eq!(
        second,
        swapped_bytes(&frames),
        "B 从第 2 块入口起逐字节生效"
    );
    assert_ne!(first, second, "换图前后同一输入的设备字节必须不同");

    let after_b = runtime.status();
    assert_eq!(after_b.graph.version, Some(2));
    assert_eq!(after_b.graph.epoch, Some(2));
    assert_eq!(
        after_b.graph.applied_block,
        Some(2),
        "applied_block = 新图起始块号"
    );
    assert!(!after_b.switch_pending);
    assert_eq!(after_b.blocks, 2);
    assert_eq!(after_b.output.blocks_written, 2);
}

// ===========================================================================
// 4) 确定性：PlanBuilder 字节/graph_id 与两次 Runtime 运行的 S24 字节序列
// ===========================================================================

fn run_nontrivial_scenario() -> Vec<Vec<u8>> {
    let plan_bytes = nontrivial_builder().build_bytes().expect("plan build");
    let (mut runtime, handle) = new_runtime();
    assert_eq!(
        runtime
            .apply_plan(&plan_bytes, &AdmissionLimits::default())
            .expect("apply"),
        1
    );
    let mut blocks = Vec::new();
    for block in 0..3u32 {
        let frames = block_signal(block);
        blocks.push(feed_and_run(
            &mut runtime,
            &handle,
            block as u16,
            block,
            &frames,
        ));
    }
    blocks
}

#[test]
fn plan_build_and_runtime_output_are_deterministic() {
    // 跨 crate 视角：同一组装器输入 → 相同字节、相同 ABI graph_id。
    let first_bytes = nontrivial_builder().build_bytes().expect("build a");
    let second_bytes = nontrivial_builder().build_bytes().expect("build b");
    assert_eq!(first_bytes, second_bytes, "相同输入必须产出相同字节");
    let first_plan = Plan::decode(&first_bytes).expect("decode a");
    let second_plan = Plan::decode(&second_bytes).expect("decode b");
    assert_eq!(first_plan, second_plan);
    assert_eq!(
        first_plan.graph_id_hex(),
        second_plan.graph_id_hex(),
        "相同字节的 graph_id 必须相同"
    );
    assert_eq!(first_plan.graph_id_hex().len(), 64);

    // 同一输入 + 同一方案跑两次 Runtime：每块 S24_3LE 字节序列完全相同。
    let first_run = run_nontrivial_scenario();
    let second_run = run_nontrivial_scenario();
    assert_eq!(
        first_run, second_run,
        "同一输入/方案两次运行的 S24 字节序列必须相同"
    );
    assert_eq!(first_run.len(), 3);
    for (block, captured) in first_run.iter().enumerate() {
        let frames = block_signal(block as u32);
        assert_eq!(
            captured,
            &nontrivial_expected_bytes(&frames),
            "block {block}: 期望值本身也不得漂移"
        );
    }
}

// ===========================================================================
// 5) 失败路径：截断 / 坏 CRC 的方案字节 → 稳定错误码，不 panic、不污染执行器
// ===========================================================================

#[test]
fn prepare_plan_rejects_truncated_and_crc_corrupt_bytes() {
    let limits = AdmissionLimits::default();
    let good = identity_builder().build_bytes().expect("plan bytes");
    let mut truncated = good.clone();
    truncated.truncate(good.len() - 1);
    let mut corrupt = good.clone();
    // 改 body 里的一个字节但不更新 CRC-32C：解码必须 fail-closed 拒绝。
    corrupt[20] ^= 0xff;

    let (mut runtime, handle) = new_runtime();
    let error = runtime
        .prepare_plan(&truncated, &limits)
        .expect_err("截断方案必须被拒绝");
    assert_eq!(error.code(), "ERR_RUNTIME_PLAN");
    assert!(matches!(&error, RuntimeError::Plan(_)));

    let error = runtime
        .prepare_plan(&corrupt, &limits)
        .expect_err("坏 CRC 方案必须被拒绝");
    assert_eq!(error.code(), "ERR_RUNTIME_PLAN");
    assert!(matches!(&error, RuntimeError::Plan(_)));

    let status = runtime.status();
    assert_eq!(
        status.graph,
        GraphInfo::default(),
        "失败的 prepare 不得改变执行器"
    );
    assert!(!status.switch_pending);

    // 失败之后仍能正常加载并跑块（不 panic、状态未被污染）。
    assert_eq!(runtime.apply_plan(&good, &limits).expect("apply good"), 1);
    let frames = block_signal(3);
    let captured = feed_and_run(&mut runtime, &handle, 0, 0, &frames);
    assert_eq!(captured, identity_bytes(&frames), "恢复后仍是逐字节直通");
    assert_eq!(runtime.status().graph.version, Some(1));
}
