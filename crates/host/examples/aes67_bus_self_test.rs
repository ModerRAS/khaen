//! 多路 AES67 输入总线自测（IO I3a）：真实 UDP socket + 合成 RTP/L24，不碰 ALSA/MOTU，
//! 不出声。用于本机或设备上验证“两路流 → 一条输入总线 → 稀疏混音”的真实收包路径。
//!
//! 运行：
//!
//! ~~~text
//! cargo run -p khaen-host --example aes67_bus_self_test
//! ~~~
//!
//! 进程内起两个发送线程（127.0.0.1:15004 / 15006，各 2ch L24 48 kHz，1 ms ptime）和两个
//! 收包线程，总线宽度 4：stream0 映射槽 0/1，stream1 映射槽 2/3。跑 1 秒后打印块统计；
//! 如果两路都收到、输出非静音、无超量程，退出码 0；否则退出码 1。

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use khaen_host::aes67_bus::Aes67InputBus;
use khaen_host::aes67_input::{Aes67InputConfig, L24_SAMPLE_BYTES};
use khaen_host::runtime::{PacketSource, ReceiverConfig, UdpPacketSource};

const PT_A: u8 = 96;
const PT_B: u8 = 97;
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const FRAMES_PER_PACKET: usize = 48;
const BLOCK_FRAMES: usize = 128;
const BUS_WIDTH: usize = 4;
const DURATION: Duration = Duration::from_secs(1);

fn stream(port: u16, pt: u8, map: &[u16]) -> Aes67InputConfig {
    Aes67InputConfig {
        multicast_addr: Ipv4Addr::new(127, 0, 0, 1),
        port,
        payload_type: pt,
        sample_rate: RATE,
        channels: CHANNELS,
        channels_map: map.to_vec(),
        output_channels: BUS_WIDTH,
        block_frames: BLOCK_FRAMES,
        // 实时自测留 ~85 ms 抖动余量，避免调度抖动导致突发溢出（真实接线按网络预算设置）。
        jitter_target_frames: 4096,
    }
}

fn packet(pt: u8, seq: u16, timestamp: u32, ssrc: u32, phase: usize, freq: f32) -> Vec<u8> {
    let mut bytes = vec![0u8; 12 + FRAMES_PER_PACKET * CHANNELS * L24_SAMPLE_BYTES];
    bytes[0] = 0x80;
    bytes[1] = pt;
    bytes[2..4].copy_from_slice(&seq.to_be_bytes());
    bytes[4..8].copy_from_slice(&timestamp.to_be_bytes());
    bytes[8..12].copy_from_slice(&ssrc.to_be_bytes());
    for frame in 0..FRAMES_PER_PACKET {
        for channel in 0..CHANNELS {
            let n = (phase + frame) as f32;
            let value = (0.25 * (2.0 * std::f32::consts::PI * freq * n / RATE as f32).sin()) as f64;
            let encoded = ((value * 8_388_608.0) as i32 as u32) & 0x00ff_ffff;
            let offset = 12 + (frame * CHANNELS + channel) * L24_SAMPLE_BYTES;
            bytes[offset] = (encoded >> 16) as u8;
            bytes[offset + 1] = (encoded >> 8) as u8;
            bytes[offset + 2] = encoded as u8;
        }
    }
    bytes
}

fn sender(port: u16, pt: u8, ssrc: u32, freq: f32, stop: Arc<AtomicBool>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("sender socket");
    let target = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut sequence = 0u16;
    let mut timestamp = 0u32;
    let mut phase = 0usize;
    let start = Instant::now();
    let mut next_packet = start;
    while !stop.load(Ordering::Relaxed) && start.elapsed() < DURATION {
        let now = Instant::now();
        if now >= next_packet {
            let bytes = packet(pt, sequence, timestamp, ssrc, phase, freq);
            let _ = socket.send_to(&bytes, target);
            sequence = sequence.wrapping_add(1);
            timestamp = timestamp.wrapping_add(FRAMES_PER_PACKET as u32);
            phase += FRAMES_PER_PACKET;
            // 按墙上时钟补发（sleep 精度不足时一次循环可发多包），保证媒体时间轴跟住 1 s。
            next_packet += Duration::from_millis(1);
        } else {
            thread::sleep(Duration::from_micros(200));
        }
    }
}

fn receiver(
    index: usize,
    config: Aes67InputConfig,
    tx: mpsc::Sender<(usize, Vec<u8>)>,
    stop: Arc<AtomicBool>,
) {
    let mut source = match UdpPacketSource::bind(&config, &ReceiverConfig::default()) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("receiver {index}: bind failed: {error}");
            return;
        }
    };
    let mut buffer = vec![0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match source.recv(&mut buffer) {
            Ok(0) => {}
            Ok(len) => {
                let _ = tx.send((index, buffer[..len].to_vec()));
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                eprintln!("receiver {index}: recv failed: {error}");
                break;
            }
        }
    }
}

fn main() {
    let stop = Arc::new(AtomicBool::new(false));
    let configs = vec![stream(15004, PT_A, &[0, 1]), stream(15006, PT_B, &[2, 3])];
    let mut bus = Aes67InputBus::new_with_asrc(configs.clone(), None).expect("bus");
    let (tx, rx) = mpsc::channel::<(usize, Vec<u8>)>();
    let mut receivers = Vec::new();
    for (index, config) in configs.into_iter().enumerate() {
        let tx = tx.clone();
        let stop = Arc::clone(&stop);
        receivers.push(thread::spawn(move || receiver(index, config, tx, stop)));
    }
    drop(tx);
    let senders = vec![
        thread::spawn({
            let stop = Arc::clone(&stop);
            move || sender(15004, PT_A, 0x1111_1111, 440.0, stop)
        }),
        thread::spawn({
            let stop = Arc::clone(&stop);
            move || sender(15006, PT_B, 0x2222_2222, 660.0, stop)
        }),
    ];

    let block_period = Duration::from_secs_f64(BLOCK_FRAMES as f64 / RATE as f64);
    let start = Instant::now();
    let mut next_block = start + block_period;
    let mut out = vec![0.0f32; bus.samples_per_block()];
    let mut peak = 0.0f32;
    let mut blocks = 0u64;
    let mut last_outcome = None;
    while start.elapsed() < DURATION {
        while let Ok((index, bytes)) = rx.try_recv() {
            if let Err(error) = bus.feed_packet(index, &bytes) {
                eprintln!("feed stream {index} failed: {error:?}");
            }
        }
        let now = Instant::now();
        if now >= next_block {
            let outcome = bus.fill_block(&mut out).expect("fill");
            last_outcome = Some(outcome);
            blocks += 1;
            for &value in &out {
                peak = peak.max(value.abs());
            }
            next_block += block_period;
        }
        thread::sleep(Duration::from_micros(200));
    }
    stop.store(true, Ordering::Relaxed);
    for sender in senders {
        let _ = sender.join();
    }
    for receiver in receivers {
        let _ = receiver.join();
    }

    let stats = bus.stats();
    let outcome = last_outcome.expect("at least one block");
    println!(
        "blocks={blocks} filled_blocks={} silent_blocks={} over_range={} peak={peak:.4} last={{filled:{},warmup:{},underrun:{}}} streams={}",
        stats.filled_blocks,
        stats.silent_blocks,
        stats.over_range_samples,
        outcome.filled_streams,
        outcome.warmup_streams,
        outcome.underrun_streams,
        stats.streams.len()
    );
    for (index, stream) in stats.streams.iter().enumerate() {
        println!(
            "stream{index}: packets={} frames_received={} frames_filled={} underruns={} malformed={} foreign={}",
            stream.packets,
            stream.frames_received,
            stream.frames_filled,
            stream.underruns,
            stream.malformed,
            stream.foreign_payloads
        );
    }
    let ok = stats.filled_blocks > blocks / 2
        && stats.streams.iter().all(|stream| stream.packets > 0)
        && stats.streams[0].frames_filled > 0
        && stats.streams[1].frames_filled > 0
        && peak > 0.05
        && stats.over_range_samples == 0;
    if ok {
        println!("self-test OK: two real UDP streams mixed into one input bus");
    } else {
        eprintln!("self-test FAILED: see counters above");
        std::process::exit(1);
    }
}
