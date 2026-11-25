// AES67 用户态接收端 + 自带最小 RTP 发送端：单进程 loopback 演示（task-235 证据用）。
//
// 运行（离线、仅本机回环 UDP，不打开声卡/不连板端）：
//   cargo run -q -p tspi-aoip-aes67 --example aes67_loopback
// 输出一行 JSON：块数/帧数/通道/逐通道 RMS/主频（Goertzel）/抖动缓冲计数/接收端报告。
// 生产发送端归 task-236（rt-stack/tools/aes67-sender）；本示例只为 receiver 验收。

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use tspi_aoip_aes67::capture::CaptureBlockSource;
use tspi_aoip_aes67::clock::frames_to_ns_ceil;
use tspi_aoip_aes67::jitter::{BlockStatus, JitterConfig};
use tspi_aoip_aes67::receiver::{Aes67Receiver, ReceiverClock, ReceiverConfig};
use tspi_aoip_aes67::rtp;
use tspi_aoip_aes67::sender::{Aes67RtpSender, SenderConfig};

const BLOCK_FRAMES: usize = 128;
const RATE_HZ: u32 = 48_000;
const BLOCKS: usize = 16;
const TONES: [f32; 2] = [1_000.0, 3_000.0];

fn goertzel_magnitude(samples: &[f32], freq_hz: f32) -> f32 {
    let n = samples.len() as f32;
    let omega = 2.0 * std::f32::consts::PI * freq_hz / RATE_HZ as f32;
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &sample in samples {
        let s0 = sample + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() * 2.0 / n
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

fn main() {
    let mut receiver = Aes67Receiver::bind(ReceiverConfig {
        media: Ipv4Addr::LOCALHOST,
        port: 0,
        multicast: false,
        interfaces: tspi_aoip_aes67::listen::ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: tspi_aoip_aes67::listen::GroupAllow::EMPTY,
            source_allow: tspi_aoip_aes67::listen::SourceAllow::EMPTY,
        channels: 2,
        rcvbuf_bytes: 4 << 20,
        recv_timeout_ms: 10,
        io_priority: None,
        clock: ReceiverClock::Usrv,
        jitter: JitterConfig::new(2, RATE_HZ, BLOCK_FRAMES, 8_192, 8_000).expect("jitter"),
        ssrc_filter: None,
    })
    .expect("receiver bind");
    let port = receiver.local_addr().port();
    let reader = receiver.take_reader().expect("jitter reader");
    let mut source = CaptureBlockSource::new(reader).expect("capture source");

    let mut config = SenderConfig::unicast(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), 2);
    config.frames_per_packet = 48;
    let mut sender = Aes67RtpSender::bind(config).expect("sender bind");
    let sender_thread = std::thread::spawn(move || {
        for _ in 0..240 {
            sender.send_sine_channels(&TONES, 0.25).expect("send");
        }
        sender
    });

    let deadline = Instant::now() + Duration::from_secs(3);
    while source.origin_ns().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let origin = source.origin_ns().expect("first RTP packet anchored");
    let latency = source.latency_frames();
    let mut planar = vec![0i32; BLOCK_FRAMES * 2];
    let mut channel_samples: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
    let mut ok_blocks = 0usize;
    for index in 0..BLOCKS {
        let needed = ((index + 1) * BLOCK_FRAMES) as u64;
        let deadline = Instant::now() + Duration::from_secs(3);
        while source.available_frames() < needed && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let now = origin + frames_to_ns_ceil(latency + (index * BLOCK_FRAMES) as u64, RATE_HZ);
        let read = source.read_block(now, &mut planar).expect("read block");
        if read.status == BlockStatus::Ok {
            ok_blocks += 1;
            for frame in 0..BLOCK_FRAMES {
                channel_samples[0].push(rtp::i32_full_scale_to_f32(planar[frame * 2]));
                channel_samples[1].push(rtp::i32_full_scale_to_f32(planar[frame * 2 + 1]));
            }
        }
    }
    let sender = sender_thread.join().expect("sender thread");
    let stats = source.jitter_stats();
    receiver.stop();

    let report = receiver.report_json();
    println!(
        "{{\"proto\":\"aes67-rx-proto\",\"mode\":\"loopback-example\",\"source\":\"aes67_loopback\",\
\"blocks_requested\":{BLOCKS},\"ok_blocks\":{ok_blocks},\"frames\":{},\"channels\":2,\
\"frames_per_packet\":48,\"packets_sent\":{},\
\"rms\":[{:.6},{:.6}],\"tone_hz\":[{:.0},{:.0}],\
\"magnitude_1k\":[{:.6},{:.6}],\"magnitude_3k\":[{:.6},{:.6}],\
\"jitter\":{{\"packets\":{},\"frames_received\":{},\"frames_played\":{},\
\"underruns\":{},\"warmups\":{},\"late_packets\":{},\"overruns\":{}}},\
\"receiver\":{}}}",
        channel_samples[0].len(),
        sender.packets_sent(),
        rms(&channel_samples[0]),
        rms(&channel_samples[1]),
        TONES[0],
        TONES[1],
        goertzel_magnitude(&channel_samples[0], 1_000.0),
        goertzel_magnitude(&channel_samples[1], 1_000.0),
        goertzel_magnitude(&channel_samples[0], 3_000.0),
        goertzel_magnitude(&channel_samples[1], 3_000.0),
        stats.packets,
        stats.frames_received,
        stats.frames_played,
        stats.underruns,
        stats.warmups,
        stats.late_packets,
        stats.overruns,
        report.trim(),
    );
}
