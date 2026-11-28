//! 多路 AES67 输入总线测试（IO I3a）：合成 RTP/L24，不碰真实 socket/组播。
//!
//! 覆盖：两路 disjoint 映射到同一输入总线、重叠槽位按样本相加、断流/欠载贡献 0 不 gate
//! 其它流、超量程计数但未缩放、配置/下标/长度 fail-closed、构造后 feed/fill 零分配。

#![allow(clippy::missing_const_for_thread_local)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::Ipv4Addr;

use khaen_host::aes67_bus::{Aes67InputBus, BusError, MAX_BUS_STREAMS};
use khaen_host::aes67_input::{Aes67InputConfig, L24_SAMPLE_BYTES};

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

fn allocation_count() -> usize {
    ALLOCATIONS.with(Cell::get)
}

const L24_SCALE: f32 = 8_388_608.0;
const PT_A: u8 = 96;
const PT_B: u8 = 97;

/// f32 -> 24-bit 满量程整数（测试值都选可精确表示的二进制小数）。
fn raw(value: f32) -> i32 {
    (value * L24_SCALE) as i32
}

fn decoded(value: i32) -> f32 {
    let raw = (value as u32) & 0x00ff_ffff;
    let signed = if raw >= 0x0080_0000 {
        raw as i32 - 0x0100_0000
    } else {
        raw as i32
    };
    signed as f32 / L24_SCALE
}

fn samples(values: &[f32]) -> Vec<i32> {
    values.iter().map(|&value| raw(value)).collect()
}

fn packet(pt: u8, seq: u16, timestamp: u32, ssrc: u32, values: &[f32]) -> Vec<u8> {
    let samples = samples(values);
    let mut bytes = vec![0u8; 12 + samples.len() * L24_SAMPLE_BYTES];
    bytes[0] = 0x80;
    bytes[1] = pt;
    bytes[2..4].copy_from_slice(&seq.to_be_bytes());
    bytes[4..8].copy_from_slice(&timestamp.to_be_bytes());
    bytes[8..12].copy_from_slice(&ssrc.to_be_bytes());
    for (index, &sample) in samples.iter().enumerate() {
        let encoded = (sample as u32) & 0x00ff_ffff;
        let offset = 12 + index * L24_SAMPLE_BYTES;
        bytes[offset] = (encoded >> 16) as u8;
        bytes[offset + 1] = (encoded >> 8) as u8;
        bytes[offset + 2] = encoded as u8;
    }
    bytes
}

fn stream(
    channels: usize,
    output_channels: usize,
    block_frames: usize,
    map: &[u16],
    pt: u8,
    port: u16,
) -> Aes67InputConfig {
    Aes67InputConfig {
        multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
        port,
        payload_type: pt,
        sample_rate: 48_000,
        channels,
        channels_map: map.to_vec(),
        output_channels,
        block_frames,
        jitter_target_frames: 0,
    }
}

fn make_bus(streams: Vec<Aes67InputConfig>) -> Aes67InputBus {
    Aes67InputBus::new_with_asrc(streams, None).expect("bus")
}

fn fill(bus: &mut Aes67InputBus) -> (usize, Vec<f32>) {
    let mut out = vec![0.0f32; bus.samples_per_block()];
    let outcome = bus.fill_block(&mut out).expect("fill");
    (outcome.filled_streams, out)
}

fn expected(values: &[f32]) -> Vec<f32> {
    values.iter().map(|&value| decoded(raw(value))).collect()
}

#[test]
fn two_streams_map_into_disjoint_bus_slots() {
    let mut bus = make_bus(vec![
        stream(2, 4, 2, &[0, 1], PT_A, 5004),
        stream(2, 4, 2, &[2, 3], PT_B, 5006),
    ]);
    bus.feed_packet(
        0,
        &packet(PT_A, 1, 0, 0x1111_1111, &[0.25, -0.5, 0.75, -1.0]),
    )
    .expect("stream 0");
    bus.feed_packet(
        1,
        &packet(PT_B, 1, 0, 0x2222_2222, &[0.5, 0.25, 0.125, 0.0]),
    )
    .expect("stream 1");

    let (filled, out) = fill(&mut bus);
    assert_eq!(filled, 2);
    assert_eq!(
        out,
        expected(&[0.25, -0.5, 0.5, 0.25, 0.75, -1.0, 0.125, 0.0])
    );

    // 第二块两路都没有新数据：整块静音，但 fill 照常返回、不 gate。
    let (filled, out) = fill(&mut bus);
    assert_eq!(filled, 0);
    assert!(out.iter().all(|&value| value == 0.0));
    let stats = bus.stats();
    assert_eq!(stats.blocks, 2);
    assert_eq!(stats.filled_blocks, 1);
    assert_eq!(stats.silent_blocks, 1);
    assert_eq!(stats.streams.len(), 2);
}

#[test]
fn overlapping_slots_are_summed_in_f64_and_underrun_is_zero() {
    let mut bus = make_bus(vec![
        stream(2, 2, 2, &[0, 1], PT_A, 5004),
        stream(2, 2, 2, &[0, 1], PT_B, 5006),
    ]);
    bus.feed_packet(
        0,
        &packet(PT_A, 1, 0, 0x1111_1111, &[0.75, 0.75, 0.75, 0.75]),
    )
    .expect("stream 0");
    bus.feed_packet(
        1,
        &packet(PT_B, 1, 0, 0x2222_2222, &[0.75, 0.75, 0.75, 0.75]),
    )
    .expect("stream 1");
    let (filled, out) = fill(&mut bus);
    assert_eq!(filled, 2);
    assert_eq!(out, vec![1.5f32; 4]);
    let stats = bus.stats();
    assert_eq!(
        stats.over_range_samples, 4,
        "|sum|>1 is counted, not scaled"
    );
    assert_eq!(stats.streams[0].packets, 1);

    // 只有一路有数据：另一路按 0 贡献，输出不被 gate。
    let mut bus = make_bus(vec![
        stream(2, 2, 2, &[0, 1], PT_A, 5004),
        stream(2, 2, 2, &[0, 1], PT_B, 5006),
    ]);
    bus.feed_packet(
        0,
        &packet(PT_A, 1, 0, 0x1111_1111, &[0.5, -0.25, 0.125, -0.5]),
    )
    .expect("stream 0");
    let (filled, out) = fill(&mut bus);
    assert_eq!(filled, 1);
    assert_eq!(out, expected(&[0.5, -0.25, 0.125, -0.5]));
    let stats = bus.stats();
    assert_eq!(stats.filled_blocks, 1);
    assert_eq!(stats.silent_blocks, 0);
    assert_eq!(stats.streams[1].underruns, 1);
}

#[test]
fn bus_rejects_bad_config_and_bounds_fail_closed() {
    assert_eq!(
        Aes67InputBus::new_with_asrc(Vec::new(), None).expect_err("empty"),
        BusError::BadConfig("input bus needs at least one stream")
    );

    assert_eq!(
        Aes67InputBus::new_with_asrc(
            vec![
                stream(2, 2, 2, &[0, 1], PT_A, 5004),
                stream(2, 4, 2, &[0, 1], PT_B, 5006),
            ],
            None,
        )
        .expect_err("width mismatch"),
        BusError::BadConfig("all streams must use the same output_channels (input bus width)")
    );

    assert_eq!(
        Aes67InputBus::new_with_asrc(
            vec![
                stream(2, 2, 2, &[0, 1], PT_A, 5004),
                stream(2, 2, 4, &[0, 1], PT_B, 5006),
            ],
            None,
        )
        .expect_err("block mismatch"),
        BusError::BadConfig("all streams must use the same block_frames")
    );

    let too_many: Vec<_> = (0..=MAX_BUS_STREAMS)
        .map(|index| stream(2, 2, 2, &[0, 1], PT_A, 5004 + index as u16))
        .collect();
    assert_eq!(
        Aes67InputBus::new_with_asrc(too_many, None).expect_err("too many"),
        BusError::BadConfig("input bus exceeds MAX_BUS_STREAMS")
    );

    let mut bus = make_bus(vec![stream(2, 2, 2, &[0, 1], PT_A, 5004)]);
    assert_eq!(
        bus.feed_packet(1, &packet(PT_A, 1, 0, 1, &[0.0, 0.0]))
            .expect_err("index"),
        BusError::StreamOutOfRange {
            index: 1,
            streams: 1
        }
    );
    let mut short = [0.0f32; 1];
    assert_eq!(
        bus.fill_block(&mut short).expect_err("short out"),
        BusError::BadConfig("output buffer length must equal block_frames * output_channels")
    );

    // 错 PT 是忽略而不是错误（ForeignPayload），与单路输入一致。
    assert_eq!(
        bus.feed_packet(0, &packet(PT_B, 1, 0, 1, &[0.0, 0.0]))
            .expect("foreign"),
        khaen_host::aes67_input::FeedOutcome::ForeignPayload
    );
}

#[test]
fn bus_feed_and_fill_do_not_allocate_after_construction() {
    let mut bus = make_bus(vec![
        stream(2, 4, 128, &[0, 1], PT_A, 5004),
        stream(2, 4, 128, &[2, 3], PT_B, 5006),
    ]);
    let packet_a = packet(PT_A, 1, 0, 0x1111_1111, &[0.25; 256]);
    let packet_b = packet(PT_B, 1, 0, 0x2222_2222, &[0.5; 256]);
    let mut out = vec![0.0f32; bus.samples_per_block()];
    // 预热/首次调用会因 ASRC 关闭走原路径；喂满 128 帧后 fill。
    bus.feed_packet(0, &packet_a).expect("a");
    bus.feed_packet(1, &packet_b).expect("b");

    let before = allocation_count();
    bus.feed_packet(0, &packet_a).expect("a2");
    bus.fill_block(&mut out).expect("fill");
    assert_eq!(allocation_count(), before, "RT feed/fill must not allocate");
}
