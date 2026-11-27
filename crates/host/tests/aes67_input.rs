//! AES67 输入后端测试（T70b）：全部合成数据，Windows 本机可跑，不碰真实 socket/组播。
//!
//! 覆盖：
//! - L24（24-bit 大端线性）逐样本解码正确（含满量程边界）；
//! - 声道映射（2ch RTP → 输出 4ch 的 0/2 位置，未映射位置填 0）；
//! - 乱序包按 RTP 时间戳就位、丢包按序号缺口估算且乱序补齐回退；
//! - SSRC 变化 / 时间戳大跳变重置缓冲并计数（不炸、不出旧流垃圾）；
//! - 欠载/预热整块静音、统计增加、播放游标不倒退；
//! - 1 kHz 正弦 feed 后 fill_block 逐样本一致；
//! - 构造后 feed/fill 零堆分配（线程局部计数分配器）；
//! - 配置校验、畸形 RTP 包拒绝、late/duplicate/overflow、序号回绕。
//!
//! 零分配门禁说明：本文件所有测试共用同一个测试二进制，因此计数分配器只统计**当前
//! 测试线程**的分配（thread_local）。其它测试在自己的线程上分配不会污染计量窗口，
//! 不需要单独拆一个测试二进制（这也是本任务只允许一个测试文件的原因）。

// clippy 1.99 的 missing_const_for_thread_local 对"已经是 const 块"的初始化误报
// （见 tests/aes67_input.rs 的 thread_local!），这里显式放行。
#![allow(clippy::missing_const_for_thread_local)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::Ipv4Addr;

use khaen_host::aes67_input::{
    Aes67Input, Aes67InputConfig, BlockStatus, FeedOutcome, InputError, L24_SAMPLE_BYTES,
};

/// 统计分配次数的全局分配器：只累加当前线程的分配计数（thread_local Cell，无锁无分配）。
struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// 当前测试线程累计分配次数。
fn allocation_count() -> usize {
    ALLOCATIONS.with(Cell::get)
}

/// 清零当前测试线程的分配计数（只影响本线程）。
fn reset_allocation_count() {
    ALLOCATIONS.with(|count| count.set(0));
}

/// 测试用的 RTP 动态 payload type。
const PT: u8 = 98;
/// 测试用的 SSRC。
const SSRC: u32 = 0x0bad_cafe;
/// 满量程 24-bit 比例（与实现/aoip crate 的 L24 归一化一致）。
const L24_SCALE: f32 = 8_388_608.0;

/// 生成基础配置：单流、给定通道映射（0..channels 顺序映射）。
fn config(channels: usize, output_channels: usize, block_frames: usize) -> Aes67InputConfig {
    Aes67InputConfig {
        multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
        port: 5004,
        payload_type: PT,
        sample_rate: 48_000,
        channels,
        channels_map: (0..channels as u16).collect(),
        output_channels,
        block_frames,
        jitter_target_frames: 0,
    }
}

/// f32 → 24-bit 满量程整数（测试值都选可精确表示的二进制小数，避免编码歧义）。
fn raw(value: f32) -> i32 {
    (value * L24_SCALE) as i32
}

/// 24-bit 满量程整数 → f32（测试侧独立的期望解码公式：先按 24-bit 补码符号扩展，再 / 2^23）。
fn decoded(value: i32) -> f32 {
    let raw = (value as u32) & 0x00ff_ffff;
    let signed = if raw >= 0x0080_0000 {
        raw as i32 - 0x0100_0000
    } else {
        raw as i32
    };
    signed as f32 / L24_SCALE
}

/// 一组 f32 波形 → 24-bit 满量程样本。
fn samples(values: &[f32]) -> Vec<i32> {
    values.iter().map(|&value| raw(value)).collect()
}

/// 把一个 24-bit 满量程样本写成大端 3 字节。
fn l24(value: i32) -> [u8; L24_SAMPLE_BYTES] {
    let raw = (value as u32) & 0x00ff_ffff;
    [(raw >> 16) as u8, (raw >> 8) as u8, raw as u8]
}

/// 组一个 RTP 包（12 字节基础头 + L24 payload）。
fn packet_with(pt: u8, seq: u16, timestamp: u32, ssrc: u32, samples: &[i32]) -> Vec<u8> {
    let mut bytes = vec![0u8; 12 + samples.len() * L24_SAMPLE_BYTES];
    bytes[0] = 0x80; // v=2, P=0, X=0, CC=0
    bytes[1] = pt;
    bytes[2..4].copy_from_slice(&seq.to_be_bytes());
    bytes[4..8].copy_from_slice(&timestamp.to_be_bytes());
    bytes[8..12].copy_from_slice(&ssrc.to_be_bytes());
    for (index, &sample) in samples.iter().enumerate() {
        let encoded = l24(sample);
        let offset = 12 + index * L24_SAMPLE_BYTES;
        bytes[offset..offset + L24_SAMPLE_BYTES].copy_from_slice(&encoded);
    }
    bytes
}

/// 组一个本流（PT/SSRC 固定）RTP 包。
fn packet(seq: u16, timestamp: u32, samples: &[i32]) -> Vec<u8> {
    packet_with(PT, seq, timestamp, SSRC, samples)
}

/// 取一块；返回状态与整块输出。
fn fill(input: &mut Aes67Input) -> (BlockStatus, Vec<f32>) {
    let mut out = vec![0.0f32; input.samples_per_block()];
    let outcome = input.fill_block(&mut out);
    assert_eq!(
        outcome.frames,
        if outcome.status == BlockStatus::Filled {
            input.config().block_frames
        } else {
            0
        }
    );
    (outcome.status, out)
}

/// 取一块并断言已填满。
fn fill_ok(input: &mut Aes67Input) -> Vec<f32> {
    let (status, out) = fill(input);
    assert_eq!(status, BlockStatus::Filled, "期望 Filled，实际 {status:?}");
    out
}

#[test]
fn l24_decode_is_sample_exact() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    // 8 个 24-bit 值：+满量程边界、-满量程、+0.5、-1 LSB、+1 LSB、0、0x123456。
    let values = [
        0x7f_ffff, 0x80_0000, 0x40_0000, 0xff_ffff, 0x00_0001, 0x00_0000, 0x12_3456, 0x3f_ffff,
    ];
    let expected: Vec<f32> = values.iter().map(|&value| decoded(value)).collect();
    assert_eq!(
        input.feed_packet(&packet(0, 0, &values)).expect("feed"),
        FeedOutcome::Accepted { frames: 8 }
    );
    let first = fill_ok(&mut input);
    let second = fill_ok(&mut input);
    assert_eq!(first.as_slice(), &expected[..4]);
    assert_eq!(second.as_slice(), &expected[4..]);
    // 显式边界：0x7fffff ≈ +1、0x800000 = -1、0xffffff = -1 LSB、0x000001 = +1 LSB。
    assert_eq!(first[0], 8_388_607.0 / L24_SCALE);
    assert_eq!(first[1], -1.0);
    assert_eq!(first[3], -1.0 / L24_SCALE);
    assert_eq!(second[0], 1.0 / L24_SCALE);
    assert_eq!(second[1], 0.0);
}

#[test]
fn channel_map_places_rtp_channels_and_zero_fills() {
    let mut input_config = config(2, 4, 4);
    input_config.channels_map = vec![0, 2];
    let mut input = Aes67Input::new(input_config).expect("input");
    let waveforms = [0.5f32, -0.25, 0.75, -0.5, 0.125, 0.375, -0.75, -0.125];
    let payload = samples(&waveforms);
    assert_eq!(
        input.feed_packet(&packet(0, 0, &payload)).expect("feed"),
        FeedOutcome::Accepted { frames: 4 }
    );
    let out = fill_ok(&mut input);
    let mut expected = Vec::with_capacity(16);
    for frame in 0..4 {
        expected.push(waveforms[frame * 2]); // RTP ch0 -> 输出声道 0
        expected.push(0.0); // 未映射声道 1
        expected.push(waveforms[frame * 2 + 1]); // RTP ch1 -> 输出声道 2
        expected.push(0.0); // 未映射声道 3
    }
    assert_eq!(out, expected);
}

#[test]
fn reordered_packets_play_by_timestamp_and_loss_estimate_recovers() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    let block0 = packet(0, 0, &samples(&[0.0, 0.25, 0.5, 0.75]));
    let block2 = packet(2, 8, &samples(&[0.125, -0.125, 0.375, -0.375]));
    let block1 = packet(1, 4, &samples(&[-0.25, -0.5, -0.75, 0.875]));
    assert_eq!(
        input.feed_packet(&block0).expect("feed 0"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(input.stats().lost, 0);
    // seq 2 先到：序号缺口 1 个，估算 lost=1，但数据按时间戳排队。
    assert_eq!(
        input.feed_packet(&block2).expect("feed 2"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(input.stats().lost, 1);
    // seq 1 补到：乱序补齐，lost 估算回退。
    assert_eq!(
        input.feed_packet(&block1).expect("feed 1"),
        FeedOutcome::Accepted { frames: 4 }
    );
    let stats = input.stats();
    assert_eq!(stats.lost, 0, "乱序补齐后 lost 估算应回退");
    assert_eq!(stats.out_of_order, 1);
    assert_eq!(fill_ok(&mut input), vec![0.0, 0.25, 0.5, 0.75]);
    assert_eq!(fill_ok(&mut input), vec![-0.25, -0.5, -0.75, 0.875]);
    assert_eq!(fill_ok(&mut input), vec![0.125, -0.125, 0.375, -0.375]);
}

#[test]
fn sequence_wrap_does_not_break_ordering() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    let first = packet(0xff_ff, 0, &samples(&[0.25, 0.25, 0.25, 0.25]));
    let wrapped = packet(0, 4, &samples(&[0.5, 0.5, 0.5, 0.5]));
    assert_eq!(
        input.feed_packet(&first).expect("feed 65535"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(
        input.feed_packet(&wrapped).expect("feed 0"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(input.stats().lost, 0, "序号回绕不应算丢包");
    assert_eq!(fill_ok(&mut input), vec![0.25; 4]);
    assert_eq!(fill_ok(&mut input), vec![0.5; 4]);
}

#[test]
fn ssrc_change_resets_buffer_and_counts() {
    let new_ssrc = 0x1111_2222;
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    // 旧流先灌 8 帧（两块）。
    let old_a = packet(0, 0, &samples(&[0.5; 4]));
    let old_b = packet(1, 4, &samples(&[0.75; 4]));
    assert_eq!(
        input.feed_packet(&old_a).expect("feed old a"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(
        input.feed_packet(&old_b).expect("feed old b"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(input.jitter_fill(), 8);
    // 新 SSRC（发送端每 1-2 分钟换一次）：必须重置，旧流样本不能混进输出。
    let new_packet = packet_with(PT, 0, 1000, new_ssrc, &samples(&[0.25; 4]));
    assert_eq!(
        input.feed_packet(&new_packet).expect("feed new ssrc"),
        FeedOutcome::Restarted { frames: 4 }
    );
    let stats = input.stats();
    assert_eq!(stats.ssrc_changes, 1);
    assert_eq!(stats.ssrc, Some(new_ssrc));
    assert_eq!(stats.jitter_fill, 4, "换流后水位只含新流");
    assert_eq!(fill_ok(&mut input), vec![0.25; 4]);
}

#[test]
fn timestamp_jump_resets_stream_and_counts() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    assert_eq!(
        input
            .feed_packet(&packet(0, 0, &samples(&[0.5; 4])))
            .expect("feed"),
        FeedOutcome::Accepted { frames: 4 }
    );
    // 时间戳跳变 > 10 s：按新流重置（锚点 = 本包时间戳）。
    let jump = packet(1, 10 * 48_000 + 4, &samples(&[0.75; 4]));
    assert_eq!(
        input.feed_packet(&jump).expect("feed jump"),
        FeedOutcome::TimestampJump { frames: 4 }
    );
    assert_eq!(input.stats().timestamp_jumps, 1);
    assert_eq!(fill_ok(&mut input), vec![0.75; 4]);
}

#[test]
fn underrun_outputs_silence_and_counts() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    // 预热（没有任何包）：整块静音，不 panic，统计增加。
    let mut out = vec![9.0f32; 4];
    let warmup = input.fill_block(&mut out);
    assert_eq!(warmup.status, BlockStatus::Warmup);
    assert_eq!(warmup.frames, 0);
    assert!(out.iter().all(|&value| value == 0.0));
    assert_eq!(input.stats().underruns, 1);
    // 一块数据 → 填满。
    assert_eq!(
        input
            .feed_packet(&packet(0, 0, &samples(&[0.5, 0.25, -0.25, -0.5])))
            .expect("feed"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(fill_ok(&mut input), vec![0.5, 0.25, -0.25, -0.5]);
    // 数据耗尽 → 欠载：整块静音，统计 +1。
    out.iter_mut().for_each(|value| *value = 7.0);
    let underrun = input.fill_block(&mut out);
    assert_eq!(underrun.status, BlockStatus::Underrun);
    assert!(out.iter().all(|&value| value == 0.0));
    assert_eq!(input.stats().underruns, 2);
}

#[test]
fn jitter_target_prefill_holds_until_watermark() {
    let mut input_config = config(1, 1, 4);
    input_config.jitter_target_frames = 8;
    let mut input = Aes67Input::new(input_config).expect("input");
    assert_eq!(
        input
            .feed_packet(&packet(0, 0, &samples(&[0.125; 4])))
            .expect("feed a"),
        FeedOutcome::Accepted { frames: 4 }
    );
    // 水位 4 < 目标 8：首块预填未达，输出静音（即使已够一块）。
    assert_eq!(fill(&mut input).0, BlockStatus::Warmup);
    assert_eq!(
        input
            .feed_packet(&packet(1, 4, &samples(&[0.25; 4])))
            .expect("feed b"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(fill_ok(&mut input), vec![0.125; 4]);
    assert_eq!(input.jitter_fill(), 4);
}

#[test]
fn one_khz_sine_round_trips_sample_exact() {
    let block_frames = 128usize;
    let mut input = Aes67Input::new(config(1, 1, block_frames)).expect("input");
    let total_frames = 2 * block_frames;
    let mut expected = Vec::with_capacity(total_frames);
    let mut payload: Vec<i32> = Vec::with_capacity(64);
    let mut seq = 0u16;
    let mut timestamp = 0u32;
    // 1 kHz 正弦 @48 kHz，幅度 0.5；按 64 帧/包分 4 包喂入。
    for frame in 0..total_frames {
        let phase = 2.0 * std::f32::consts::PI * 1000.0 * frame as f32 / 48_000.0;
        let sample = 0.5 * phase.sin();
        let quantized = raw(sample);
        expected.push(decoded(quantized));
        payload.push(quantized);
        if payload.len() == 64 {
            assert_eq!(
                input
                    .feed_packet(&packet(seq, timestamp, &payload))
                    .expect("feed"),
                FeedOutcome::Accepted { frames: 64 }
            );
            seq += 1;
            timestamp += 64;
            payload.clear();
        }
    }
    assert!(payload.is_empty());
    let stats = input.stats();
    assert_eq!(stats.packets, 4);
    assert_eq!(stats.frames_received, total_frames as u64);
    let first = fill_ok(&mut input);
    let second = fill_ok(&mut input);
    assert_eq!(first.as_slice(), &expected[..block_frames]);
    assert_eq!(second.as_slice(), &expected[block_frames..]);
    // 与理想正弦之差只有 L24 量化误差（< 1 LSB = 2^-23 满量程）。
    for (index, &value) in first.iter().chain(second.iter()).enumerate() {
        let phase = 2.0 * std::f32::consts::PI * 1000.0 * index as f32 / 48_000.0;
        let ideal = 0.5 * phase.sin();
        assert!(
            (value - ideal).abs() < 1.0 / L24_SCALE,
            "frame {index}: value {value} vs ideal {ideal}"
        );
    }
}

#[test]
fn feed_and_fill_do_not_allocate_after_construction() {
    // 全部数据在计量窗口外构造。
    let mut input = Aes67Input::new(config(2, 2, 128)).expect("input");
    let mut out = vec![0.0f32; input.samples_per_block()];
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut seq = 0u16;
    let mut timestamp = 0u32;
    for iteration in 0..24 {
        for packet_index in 0..3 {
            let mut payload = Vec::with_capacity(96);
            for index in 0..96 {
                let value = (((iteration * 3 + packet_index) * 96 + index) as f32 * 0.001).sin();
                payload.push(raw(value * 0.5));
            }
            packets.push(packet(seq, timestamp, &payload));
            seq += 1;
            timestamp += 48;
        }
    }
    // 预热一次 fill（未锚定路径），让任何惰性初始化发生在计量窗口外。
    let _ = input.fill_block(&mut out);
    reset_allocation_count();
    // 每 3 个包（144 帧）取一块（128 帧）：净增 16 帧/轮，24 轮远小于 512 帧容量。
    for chunk in packets.chunks(3) {
        for pkt in chunk {
            let outcome = input.feed_packet(pkt).expect("feed");
            assert_eq!(outcome, FeedOutcome::Accepted { frames: 48 });
        }
        let outcome = input.fill_block(&mut out);
        assert_eq!(outcome.status, BlockStatus::Filled);
    }
    let allocated = allocation_count();
    assert_eq!(allocated, 0, "实时路径在构造后分配了 {allocated} 次堆内存");
    assert!(input.stats().frames_filled > 0);
}

#[test]
fn wrong_output_length_is_reported_without_panic() {
    let mut input = Aes67Input::new(config(2, 4, 8)).expect("input");
    assert_eq!(input.samples_per_block(), 32);
    let mut out = vec![1.0f32; 31];
    let outcome = input.fill_block(&mut out);
    assert_eq!(
        outcome.status,
        BlockStatus::WrongOutputLength {
            expected: 32,
            actual: 31
        }
    );
    assert_eq!(outcome.frames, 0);
    assert!(out.iter().all(|&value| value == 0.0));
}

#[test]
fn config_validation_works_fail_closed() {
    fn assert_invalid(mutate: impl FnOnce(&mut Aes67InputConfig)) {
        let mut input_config = config(2, 2, 128);
        mutate(&mut input_config);
        assert!(
            matches!(input_config.validate(), Err(InputError::BadConfig(_))),
            "配置应被拒绝: {input_config:?}"
        );
        assert!(Aes67Input::new(input_config).is_err());
    }
    assert!(config(2, 2, 128).validate().is_ok());
    // IO I2b：单播目的地址是合法配置（接收器会 bind 本端地址，不 join 组播）。
    let mut unicast = config(2, 2, 128);
    unicast.multicast_addr = Ipv4Addr::new(192, 168, 1, 10);
    assert!(unicast.validate().is_ok());
    assert_invalid(|input_config| input_config.multicast_addr = Ipv4Addr::UNSPECIFIED);
    assert_invalid(|input_config| input_config.port = 0);
    assert_invalid(|input_config| input_config.payload_type = 95);
    assert_invalid(|input_config| input_config.payload_type = 128);
    assert_invalid(|input_config| input_config.sample_rate = 0);
    assert_invalid(|input_config| input_config.channels = 0);
    assert_invalid(|input_config| input_config.channels = 9);
    assert_invalid(|input_config| {
        input_config.channels_map.pop();
    });
    assert_invalid(|input_config| input_config.channels_map = vec![0, 0]);
    assert_invalid(|input_config| input_config.channels_map = vec![0, 4]);
    assert_invalid(|input_config| input_config.output_channels = 0);
    assert_invalid(|input_config| input_config.output_channels = 65);
    assert_invalid(|input_config| input_config.block_frames = 0);
    assert_invalid(|input_config| input_config.block_frames = 8193);
    assert_invalid(|input_config| input_config.jitter_target_frames = 65_537);
}

#[test]
fn rtp_header_extras_are_skipped_and_malformed_packets_rejected() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    let values = samples(&[0.5, 0.25, -0.25, -0.5]);
    // 手工构造带 CSRC=1 + 扩展 2 words + padding 2 字节的合法包。
    let mut bytes = Vec::new();
    bytes.push(0x80 | 0x01 | 0x10 | 0x20); // v=2, CC=1, X, P
    bytes.push(PT);
    bytes.extend_from_slice(&7u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&SSRC.to_be_bytes());
    bytes.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]); // CSRC
    bytes.extend_from_slice(&[0xbe, 0xde, 0x00, 0x02]); // 扩展头：2 words
    bytes.extend_from_slice(&[0u8; 8]); // 扩展数据
    for &value in &values {
        bytes.extend_from_slice(&l24(value));
    }
    bytes.extend_from_slice(&[0x00, 0x02]); // padding：末字节 = 2
    assert_eq!(
        input.feed_packet(&bytes).expect("feed extras"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(fill_ok(&mut input), vec![0.5, 0.25, -0.25, -0.5]);

    // 空包/短包。
    assert_eq!(
        input.feed_packet(&[]).expect_err("short"),
        InputError::PacketTooShort
    );
    // 版本不是 2。
    let mut bad_version = packet(1, 0, &values);
    bad_version[0] = 0x40;
    assert_eq!(
        input.feed_packet(&bad_version).expect_err("version"),
        InputError::BadVersion
    );
    // CSRC=15 但没有 CSRC 数据。
    let mut csrc = packet(1, 0, &values);
    csrc[0] |= 0x0f;
    assert_eq!(
        input.feed_packet(&csrc).expect_err("csrc"),
        InputError::CsrcOverflow
    );
    // 扩展标志但没有扩展头。
    let mut extension = packet(1, 0, &values);
    extension[0] |= 0x10;
    assert_eq!(
        input.feed_packet(&extension).expect_err("extension"),
        InputError::ExtensionOverflow
    );
    // padding 长度非法（末字节 0）。
    let mut padding = packet(1, 0, &values);
    padding[0] |= 0x20;
    let last = padding.len() - 1;
    padding[last] = 0;
    assert_eq!(
        input.feed_packet(&padding).expect_err("padding"),
        InputError::BadPadding
    );
    // payload 不是整帧（2 字节，channels=1 时帧长 3 字节）。
    let mut bad_payload = vec![0u8; 14];
    bad_payload[0] = 0x80;
    bad_payload[1] = PT;
    assert_eq!(
        input.feed_packet(&bad_payload).expect_err("payload"),
        InputError::BadPayload
    );
    // 其他 payload type：忽略但计数。
    let foreign = packet_with(PT + 1, 2, 8, SSRC, &values);
    assert_eq!(
        input.feed_packet(&foreign).expect("foreign"),
        FeedOutcome::ForeignPayload
    );
    let stats = input.stats();
    assert_eq!(stats.foreign_payloads, 1);
    assert_eq!(stats.malformed, 6);
    assert_eq!(stats.packets, 3, "只有通过 RTP 头解析的包计入 packets");
}

#[test]
fn late_duplicate_and_overflow_are_counted_not_fatal() {
    let mut input = Aes67Input::new(config(1, 1, 4)).expect("input");
    let first = packet(0, 0, &samples(&[0.5; 4]));
    assert_eq!(
        input.feed_packet(&first).expect("feed"),
        FeedOutcome::Accepted { frames: 4 }
    );
    assert_eq!(fill_ok(&mut input), vec![0.5; 4]);
    // 重复序号：丢弃并计数。
    assert_eq!(
        input.feed_packet(&first).expect("dup"),
        FeedOutcome::Duplicate
    );
    assert_eq!(input.stats().duplicates, 1);
    // 时间戳已越过播放游标（读游标 = 4）：整包 late。
    let late = packet(1, 0, &samples(&[0.25; 4]));
    assert_eq!(
        input.feed_packet(&late).expect("late"),
        FeedOutcome::Late { frames: 4 }
    );
    // 帧号超出环窗口（读游标 4 + 容量 512）：overflow。
    let overflow = packet(2, 4 + 512, &samples(&[0.75; 4]));
    assert_eq!(
        input.feed_packet(&overflow).expect("overflow"),
        FeedOutcome::Overflow { frames: 4 }
    );
    let stats = input.stats();
    assert_eq!(stats.late, 1);
    assert_eq!(stats.overruns, 1);
    assert_eq!(stats.packets, 4);
}

#[test]
fn default_config_and_names_are_stable() {
    let default_config = Aes67InputConfig::default();
    assert!(default_config.validate().is_ok());
    let input = Aes67Input::new(default_config).expect("input");
    assert_eq!(input.samples_per_block(), 128 * 2);
    assert_eq!(input.capacity_frames(), 512);
    assert_eq!(BlockStatus::Filled.name(), "filled");
    assert_eq!(BlockStatus::Underrun.name(), "underrun");
    assert_eq!(FeedOutcome::ForeignPayload.name(), "foreign_payload");
    assert_eq!(
        InputError::BadPayload.name(),
        "ERR_AES67_INPUT_RTP_BAD_PAYLOAD"
    );
    assert_eq!(
        InputError::PacketTooShort.to_string(),
        "ERR_AES67_INPUT_RTP_TOO_SHORT"
    );
    assert_eq!(InputError::BadConfig("x").to_string(), "bad config: x");
}
