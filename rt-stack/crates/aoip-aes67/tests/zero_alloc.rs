//! 热路径**零分配**证明（计数 GlobalAlloc；task-235 RT 约束验收）。
//!
//! 单独一个 test 二进制、单个测试：Rust test harness 的其它线程不参与测量，计数
//! 全局分配器只统计本测试线程在测量区内的分配。测量区只包含 RT/IO 热路径函数：
//! - \`JitterWriter::push_packet\`（RTP 头已知 + 预编码 L24 payload，直接写环）；
//! - \`JitterReader::read_block\` / \`CaptureBlockSource::read_block\`（整块输出）。
//!
//! 运行：\`cargo test -p tspi-aoip-aes67 --test zero_alloc -- --test-threads=1\`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use tspi_aoip_aes67::capture::CaptureBlockSource;
use tspi_aoip_aes67::clock::{frames_to_ns_ceil, MediaClock, MockClock};
use tspi_aoip_aes67::jitter::{self, JitterConfig};
use tspi_aoip_aes67::rtp::{self, Header};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: 转发到系统分配器，layout 由调用方保证合法。
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 与 alloc 配对转发；ptr/layout 由调用方保证合法。
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

fn packet_header(sequence: u16, timestamp: u32) -> Header {
    Header {
        version: 2,
        padding: false,
        extension: false,
        csrc_count: 0,
        marker: false,
        payload_type: 96,
        sequence,
        timestamp,
        ssrc: 0xabcd_1234,
    }
}

/// 预编码一包 L24（48 帧 × 2ch × 3B）正弦样本。
fn encode_packet(frames: usize, channels: usize) -> Vec<u8> {
    let mut payload = vec![0u8; frames * channels * rtp::L24_SAMPLE_BYTES];
    for frame in 0..frames {
        for channel in 0..channels {
            let phase = std::f64::consts::TAU * 1_000.0 * (frame as f64) / 48_000.0;
            let sample = (phase.sin() * 0.25) as f32;
            let full = rtp::f32_to_i32_full_scale(sample);
            let offset = (frame * channels + channel) * rtp::L24_SAMPLE_BYTES;
            let mut encoded = [0u8; rtp::L24_SAMPLE_BYTES];
            rtp::pack_l24_sample(full, &mut encoded);
            payload[offset..offset + rtp::L24_SAMPLE_BYTES].copy_from_slice(&encoded);
        }
    }
    payload
}

#[test]
fn hot_path_push_and_read_are_allocation_free() {
    let config = JitterConfig::new(2, 48_000, 128, 8_192, 4_000).expect("config");
    let payload = encode_packet(48, 2);
    let mut decode_buffer = [0i32; 128 * 2];
    let origin = 1_000u64;

    // 预热：用一次性的独立通道跑首包/首块，把任何惰性初始化移出测量区。
    {
        let (warm_writer, warm_reader) = jitter::channel(config).expect("warmup channel");
        let mut warm_source = CaptureBlockSource::new(warm_reader).expect("warmup source");
        warm_writer.push_packet(origin, &packet_header(0, 0), &payload);
        let now = origin + frames_to_ns_ceil(256, 48_000) + 1_000;
        let _ = warm_source.read_block(now, &mut decode_buffer);
    }

    // 正式测量通道（构造在测量区外）。
    let (writer, reader) = jitter::channel(config).expect("channel");
    let mut source = CaptureBlockSource::new(reader).expect("capture source");

    // 测量区：200 次 push（序号/时间戳随包递增）+ 64 次整块读取（时钟按块推进）。
    let before = allocations();
    for index in 0..200u16 {
        let header = packet_header(index, index as u32 * 48);
        let result = writer.push_packet(origin, &header, &payload);
        // 200 包 = 9600 帧 > 环容量 8192；超出窗口的包按策略 Overflow 丢弃（计数，不阻塞）。
        assert!(matches!(
            result,
            jitter::PushResult::Accepted { .. }
                | jitter::PushResult::Restarted { .. }
                | jitter::PushResult::Overflow { .. }
        ));
    }
    let latency = 256u64;
    let mut block_frames_played = 0u64;
    for index in 0..64u64 {
        let now = origin + frames_to_ns_ceil(latency + block_frames_played, 48_000);
        let read = source
            .read_block(now, &mut decode_buffer)
            .expect("read block");
        assert!(
            read.status == jitter::BlockStatus::Ok || read.status == jitter::BlockStatus::Underrun
        );
        block_frames_played += 128;
        let _ = index;
    }
    let delta = allocations() - before;

    // 计数器自检（证明热路径确实跑了）与零分配断言。
    let jitter_stats = source.jitter_stats();
    assert!(jitter_stats.packets >= 200, "{jitter_stats:?}");
    // 200 包 = 9600 帧 > 环容量 8192：超出窗口的包按策略丢弃（Overflow），已写帧 = 8192。
    assert!(jitter_stats.frames_received >= 8_192, "{jitter_stats:?}");
    assert_eq!(delta, 0, "热路径发生 {delta} 次堆分配（应为 0）");

    // 用 MockClock 的其它 API 也保持零分配（时钟读数本身）。
    let clock = MockClock::new(48_000);
    let before = allocations();
    for _ in 0..1_000 {
        clock.advance_frames(128);
        let _ = clock.now_ns();
    }
    assert_eq!(allocations() - before, 0, "MockClock 读数不应分配");
}
