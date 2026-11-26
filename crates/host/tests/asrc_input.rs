//! AES67 输入 ASRC 集成测试（T41）：真实 [Aes67Input] 路径 + 合成发送端时钟偏差。
//!
//! 覆盖：
//! - 发送端 ±200 ppm 漂移下 ASRC 路径无欠载、fill 稳定在目标、`ppm_estimate` 收敛 ±5 ppm、
//!   `stats().asrc` 状态可用；
//! - 关闭 ASRC 的对照：同样漂移下出现欠载/抖动环溢出（回归证据）；
//! - `feed_packet` + `fill_block` 的 ASRC 路径零堆分配（复用 host 的计数分配器门禁模式）；
//! - 换流（SSRC 变化）后 ASRC 重置并继续工作。
#![allow(clippy::missing_const_for_thread_local)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;

use khaen_host::aes67_input::{
    Aes67Input, Aes67InputConfig, BlockStatus, FeedOutcome, Interpolation, L24_SAMPLE_BYTES,
};

/// 统计分配次数的全局分配器：只累加当前线程的分配计数（thread_local，无锁无分配）。
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

fn reset_allocation_count() {
    ALLOCATIONS.with(|count| count.set(0));
}

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;
const CHANNELS: usize = 2;
const PACKET_FRAMES: usize = 48;
const TONE_HZ: f64 = 1_000.0;
const TONE_AMPLITUDE: f32 = 0.5;
const WARMUP_SECONDS: f64 = 20.0;
const RUN_SECONDS: f64 = 36.0;
const TARGET_FILL_TOLERANCE: usize = 64;
const TARGET_FILL: usize = 2 * BLOCK_FRAMES;
const PACKET_BYTES: usize = 12 + PACKET_FRAMES * CHANNELS * L24_SAMPLE_BYTES;

fn base_config() -> Aes67InputConfig {
    Aes67InputConfig {
        multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
        port: 5004,
        payload_type: 96,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        channels_map: (0..CHANNELS as u16).collect(),
        output_channels: CHANNELS,
        block_frames: BLOCK_FRAMES,
        jitter_target_frames: 0,
    }
}

/// 发送端时钟：按 (1 + drift_ppm·1e-6) 速率产生 1 kHz 正弦媒体帧，按整包突发写出 RTP。
struct SenderClock {
    drift_ppm: f64,
    local_frames: u64,
    media_total: u64,
    emitted: u64,
    sequence: u16,
}

impl SenderClock {
    fn new(drift_ppm: f64) -> Self {
        Self {
            drift_ppm,
            local_frames: 0,
            media_total: 0,
            emitted: 0,
            sequence: 0,
        }
    }

    /// 本地时钟前进 `local` 帧，把新到齐的整包写入 `packets`（会分配，测量窗口外调用）。
    fn advance(&mut self, local: usize, packets: &mut VecDeque<Vec<u8>>) {
        self.local_frames += local as u64;
        self.media_total =
            (self.local_frames as f64 * (1.0 + self.drift_ppm * 1e-6)).floor() as u64;
        while self.emitted + PACKET_FRAMES as u64 <= self.media_total {
            let mut packet = vec![0u8; PACKET_BYTES];
            self.fill_packet(&mut packet, true);
            packets.push_back(packet);
        }
    }

    /// 只在测量窗口外：生成一包（含正弦 payload，分配）。
    fn make_packet(&mut self) -> Vec<u8> {
        let mut packet = vec![0u8; PACKET_BYTES];
        self.fill_packet(&mut packet, true);
        packet
    }

    /// 原地改写 RTP 头并推进序号/媒体帧游标（不分配；测量窗口内用）。
    fn rewrite_header(&mut self, packet: &mut [u8]) {
        self.fill_packet(packet, false);
    }

    /// 写头（+ 可选的 payload），并推进序号/媒体帧游标。
    fn fill_packet(&mut self, packet: &mut [u8], payload: bool) {
        packet[0] = 0x80;
        packet[1] = 96;
        packet[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        packet[4..8].copy_from_slice(&(self.emitted as u32).to_be_bytes());
        packet[8..12].copy_from_slice(&0x0bad_cafeu32.to_be_bytes());
        if payload {
            let omega = std::f64::consts::TAU * TONE_HZ / f64::from(SAMPLE_RATE);
            for frame in 0..PACKET_FRAMES {
                let media_frame = self.emitted + frame as u64;
                let value = (omega * media_frame as f64).sin() as f32 * TONE_AMPLITUDE;
                let raw = ((value * 8_388_608.0) as i32 as u32) & 0x00ff_ffff;
                for channel in 0..CHANNELS {
                    let offset = 12 + (frame * CHANNELS + channel) * L24_SAMPLE_BYTES;
                    packet[offset] = (raw >> 16) as u8;
                    packet[offset + 1] = (raw >> 8) as u8;
                    packet[offset + 2] = raw as u8;
                }
            }
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.emitted += PACKET_FRAMES as u64;
    }
}

/// 一次仿真的统计。
#[derive(Clone, Copy, Debug)]
struct SimOutcome {
    underruns: u64,
    overruns: u64,
    filled_blocks: usize,
    fill_min: usize,
    fill_max: usize,
    mean_square: f64,
    ppm_estimate: f32,
    ratio_ppm: f32,
    asrc_enabled: bool,
}

/// 用给定的插值方式（None = off）跑一次带发送端时钟偏差的完整输入路径仿真。
fn simulate(interp: Option<Interpolation>, drift_ppm: f64) -> SimOutcome {
    let mut input = Aes67Input::new_with_asrc(base_config(), interp).expect("input");
    let mut packets: VecDeque<Vec<u8>> = VecDeque::new();
    let mut clock = SenderClock::new(drift_ppm);
    // 预填：按包粒度喂到目标水位（ASRC 恒为 2 块；off 也用同一水位便于对照）。
    while input.jitter_fill() < TARGET_FILL {
        clock.advance(PACKET_FRAMES, &mut packets);
        while let Some(packet) = packets.pop_front() {
            input.feed_packet(&packet).expect("feed");
        }
    }
    let total_blocks =
        (RUN_SECONDS * f64::from(SAMPLE_RATE) / BLOCK_FRAMES as f64).round() as usize;
    let warmup_blocks =
        (WARMUP_SECONDS * f64::from(SAMPLE_RATE) / BLOCK_FRAMES as f64).round() as usize;
    let mut out = vec![0.0f32; input.samples_per_block()];
    let mut underruns_before = 0u64;
    let mut filled_blocks = 0usize;
    let mut fill_min = usize::MAX;
    let mut fill_max = 0usize;
    let mut energy = 0.0f64;
    let mut energy_samples = 0usize;
    for block in 0..total_blocks {
        clock.advance(BLOCK_FRAMES, &mut packets);
        while let Some(packet) = packets.pop_front() {
            input.feed_packet(&packet).expect("feed packet");
        }
        let block_outcome = input.fill_block(&mut out);
        if block == warmup_blocks {
            underruns_before = input.stats().underruns;
        }
        if block >= warmup_blocks && block_outcome.status == BlockStatus::Filled {
            filled_blocks += 1;
            let fill = input.jitter_fill();
            fill_min = fill_min.min(fill);
            fill_max = fill_max.max(fill);
            if block >= total_blocks - 375 {
                for sample in &out {
                    energy += f64::from(*sample) * f64::from(*sample);
                }
                energy_samples += out.len();
            }
        }
    }
    let stats = input.stats();
    let report = stats.asrc;
    SimOutcome {
        underruns: stats.underruns.saturating_sub(underruns_before),
        overruns: stats.overruns,
        filled_blocks,
        fill_min,
        fill_max,
        mean_square: if energy_samples == 0 {
            0.0
        } else {
            energy / energy_samples as f64
        },
        ppm_estimate: report.map_or(0.0, |value| value.ppm_estimate as f32),
        ratio_ppm: report.map_or(0.0, |value| value.ratio_ppm as f32),
        asrc_enabled: report.is_some_and(|value| value.enabled),
    }
}

#[test]
fn asrc_tracks_sender_clock_drift_without_underrun() {
    for drift_ppm in [200.0f64, -200.0] {
        for interp in [Interpolation::Poly, Interpolation::Sinc] {
            let outcome = simulate(Some(interp), drift_ppm);
            assert_eq!(
                outcome.underruns, 0,
                "{interp:?} drift={drift_ppm}: 出现欠载"
            );
            assert!(
                outcome.filled_blocks > 0,
                "{interp:?}: 统计窗口内没有任何 Filled 块"
            );
            assert!(
                outcome.fill_min >= TARGET_FILL - TARGET_FILL_TOLERANCE,
                "{interp:?} drift={drift_ppm}: fill_min={} 低于目标 {}±{}",
                outcome.fill_min,
                TARGET_FILL,
                TARGET_FILL_TOLERANCE
            );
            assert!(
                outcome.fill_max <= TARGET_FILL + TARGET_FILL_TOLERANCE,
                "{interp:?} drift={drift_ppm}: fill_max={} 高于目标 {}±{}",
                outcome.fill_max,
                TARGET_FILL,
                TARGET_FILL_TOLERANCE
            );
            assert!(
                (f64::from(outcome.ppm_estimate) + drift_ppm).abs() <= 5.0,
                "{interp:?} drift={drift_ppm}: ppm 估计 {:.2} 未收敛到 {:.2}±5",
                outcome.ppm_estimate,
                -drift_ppm
            );
            assert!(
                (f64::from(outcome.ratio_ppm) + drift_ppm).abs() <= 80.0,
                "{interp:?} drift={drift_ppm}: 施加比率 {:.2} 离 {:.2} 过远",
                outcome.ratio_ppm,
                -drift_ppm
            );
            assert!(
                outcome.asrc_enabled,
                "{interp:?}: stats.asrc.enabled 应为 true"
            );
            assert!(
                outcome.mean_square > 0.01,
                "{interp:?}: 输出能量异常（{:.4}）",
                outcome.mean_square
            );
        }
    }
}

#[test]
fn disabled_asrc_same_drift_underruns_or_overruns() {
    // 对照：关闭 ASRC（原路径）在同样漂移下必然欠载或抖动环溢出。
    let slow = simulate(None, -500.0);
    assert!(
        slow.underruns > 0,
        "关闭 ASRC 且发送端慢 500 ppm 时应欠载，实际 underruns={}",
        slow.underruns
    );
    let fast = simulate(None, 200.0);
    assert!(
        fast.overruns > 0,
        "关闭 ASRC 且发送端快 200 ppm 时抖动环应溢出丢包，实际 overruns={}",
        fast.overruns
    );
}

#[test]
fn asrc_path_is_allocation_free() {
    // 每 3 块喂 [3,3,2] 包 ≈ 128 帧/块（与本地消费速率匹配，不靠欠载/溢出兜底）。
    const PACKETS_PER_BLOCK: [usize; 3] = [3, 3, 2];
    for interp in [Interpolation::Poly, Interpolation::Sinc] {
        let mut input = Aes67Input::new_with_asrc(base_config(), Some(interp)).expect("input");
        let mut clock = SenderClock::new(200.0);
        let mut out = vec![0.0f32; input.samples_per_block()];
        while input.jitter_fill() < TARGET_FILL {
            let packet = clock.make_packet();
            input.feed_packet(&packet).expect("feed");
        }
        for _ in 0..64 {
            let packet = clock.make_packet();
            input.feed_packet(&packet).expect("feed");
            let _ = input.fill_block(&mut out);
        }
        // 预分配 4 个包缓冲；测量窗口内只原地改写 RTP 头（无分配）。
        let mut packets: Vec<Vec<u8>> = (0..4).map(|_| clock.make_packet()).collect();
        let mut next = 0usize;
        reset_allocation_count();
        let pool_len = packets.len();
        for block in 0..120 {
            let produced = PACKETS_PER_BLOCK[block % PACKETS_PER_BLOCK.len()];
            for _ in 0..produced {
                let index = next % pool_len;
                let packet = &mut packets[index];
                clock.rewrite_header(packet);
                input.feed_packet(packet).expect("feed");
                next += 1;
            }
            let _ = input.fill_block(&mut out);
            let _ = input.stats();
        }
        assert_eq!(
            allocation_count(),
            0,
            "{interp:?}: feed_packet/fill_block(ASRC) 路径发生堆分配"
        );
    }
}

#[test]
fn ssrc_change_resets_and_keeps_asrc_working() {
    let mut input =
        Aes67Input::new_with_asrc(base_config(), Some(Interpolation::Poly)).expect("input");
    let mut clock = SenderClock::new(200.0);
    while input.jitter_fill() < TARGET_FILL {
        let packet = clock.make_packet();
        input.feed_packet(&packet).expect("feed");
    }
    let mut out = vec![0.0f32; input.samples_per_block()];
    assert_eq!(input.fill_block(&mut out).status, BlockStatus::Filled);
    // 换流：篡改 SSRC → Restarted，ASRC 复位后继续输出。
    let mut packet = clock.make_packet();
    packet[8..12].copy_from_slice(&0x1234_5678u32.to_be_bytes());
    let outcome = input.feed_packet(&packet).expect("feed");
    assert!(matches!(outcome, FeedOutcome::Restarted { .. }));
    assert!(
        input.asrc_enabled(),
        "换流后 ASRC 应重新启用（reset 清 bypass）"
    );
    // 换流后抖动环清空，重新预填到目标水位。
    while input.jitter_fill() < TARGET_FILL {
        let packet = clock.make_packet();
        input.feed_packet(&packet).expect("feed");
    }
    for _ in 0..16 {
        let packet = clock.make_packet();
        input.feed_packet(&packet).expect("feed");
        let _ = input.fill_block(&mut out);
    }
    assert_eq!(
        input.stats().asrc_bypasses,
        0,
        "正常换流不应触发 ASRC bypass"
    );
}
