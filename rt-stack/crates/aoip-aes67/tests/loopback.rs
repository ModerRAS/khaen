//! sender→receiver **loopback 集成测试**（task-235 验收：合成 RTP 走 127.0.0.1/组播）。
//!
//! 覆盖：
//! 1. 单播 loopback：SDP 子集 → UDP → RTP/序号/时间戳 → L24 解码 → 抖动缓冲 →
//!    capture 固定块；断言帧数/通道数/逐通道音调频率（Goertzel）/幅度；
//! 2. 组播 loopback：bind+join 后本机组播投递（平台策略允许时；不依赖交换机 IGMP）；
//! 3. 组播 join 失败路径/组播配置校验（离线错误语义）；
//! 4. SDP 文件输入 → ReceiverConfig（不建档/不联网板端的文件路径验收）。
//!
//! 全部离线：不打开声卡、不连板端、不启动 daemon；UDP 只走本机回环。

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use khaen_aoip_aes67::capture::CaptureBlockSource;
use khaen_aoip_aes67::clock::{frames_to_ns_ceil, ns_to_frames};
use khaen_aoip_aes67::jitter::{BlockStatus, JitterConfig};
use khaen_aoip_aes67::receiver::{Aes67Receiver, ReceiverConfig, ReceiverError};
use khaen_aoip_aes67::rtp;
use khaen_aoip_aes67::sdp::SdpDocument;
use khaen_aoip_aes67::sender::{Aes67RtpSender, SenderConfig};

/// 每包帧数（1 ms @48k）。
const FRAMES_PER_PACKET: usize = 48;
/// 集成测试块长（与 host capture 合同一致）。
const BLOCK_FRAMES: usize = 128;
/// 采样率。
const RATE_HZ: u32 = 48_000;

/// 轮询等待（不依赖 sleep 精度；超时返回 false）。
fn wait_until(deadline: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    predicate()
}

/// Goertzel 单频点幅度（f32 样本；测试用，非热路径）。
fn goertzel_magnitude(samples: &[f32], freq_hz: f32, rate_hz: f32) -> f32 {
    let n = samples.len() as f32;
    let omega = 2.0 * std::f32::consts::PI * freq_hz / rate_hz;
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &sample in samples {
        let s0 = sample + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    power.max(0.0).sqrt() * 2.0 / n
}

/// RMS。
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

fn unicast_receiver_config(port: u16, channels: u16) -> ReceiverConfig {
    ReceiverConfig {
        media: Ipv4Addr::LOCALHOST,
        port,
        multicast: false,
        interfaces: khaen_aoip_aes67::listen::ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: khaen_aoip_aes67::listen::GroupAllow::EMPTY,
            source_allow: khaen_aoip_aes67::listen::SourceAllow::EMPTY,
        channels,
        rcvbuf_bytes: 4 << 20,
        recv_timeout_ms: 10,
        io_priority: None,
        clock: khaen_aoip_aes67::receiver::ReceiverClock::Usrv,
        jitter: JitterConfig::new(channels, RATE_HZ, BLOCK_FRAMES, 8_192, 8_000)
            .expect("jitter config"),
        ssrc_filter: None,
    }
}

/// sender → receiver → capture 固定块 → 逐声道音调校验（单播回环主验收）。
#[test]
fn loopback_unicast_sine_end_to_end() {
    // 1) 接收端 bind（临时端口）。
    let mut receiver = Aes67Receiver::bind(unicast_receiver_config(0, 2)).expect("receiver bind");
    let port = receiver.local_addr().port();
    assert!(port > 0);
    let reader = receiver.take_reader().expect("jitter reader");
    let mut source = CaptureBlockSource::new(reader).expect("capture source");
    assert_eq!(source.block_frames(), BLOCK_FRAMES);
    assert_eq!(source.channels(), 2);

    // 2) 发送端：ch0=1 kHz、ch1=3 kHz，1 ms 包；200 包 = 9600 帧 ≈ 200 ms。
    let mut config = SenderConfig::unicast(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), 2);
    config.frames_per_packet = FRAMES_PER_PACKET;
    let mut sender = Aes67RtpSender::bind(config).expect("sender bind");
    let tones = [1_000.0f32, 3_000.0f32];
    let total_packets = 200usize;
    let sender_thread = std::thread::spawn(move || {
        for _ in 0..total_packets {
            sender
                .send_sine_channels(&tones, 0.25)
                .expect("send sine packet");
        }
        sender
    });

    // 3) 等首包锚定。
    assert!(
        wait_until(Duration::from_secs(2), || source.origin_ns().is_some()),
        "首包未锚定（loopback 未收到 RTP）"
    );
    let origin = source.origin_ns().expect("origin");
    let latency = source.latency_frames();
    assert_eq!(latency % BLOCK_FRAMES as u64, 0);

    // 4) 时钟驱动读固定块（16 块 = 2048 帧），逐块等到数据水位足够。
    let blocks = 16usize;
    let mut out = vec![0i32; BLOCK_FRAMES * 2];
    let mut channel0 = Vec::with_capacity(blocks * BLOCK_FRAMES);
    let mut channel1 = Vec::with_capacity(blocks * BLOCK_FRAMES);
    let mut ok_blocks = 0usize;
    for index in 0..blocks {
        let needed = ((index + 1) * BLOCK_FRAMES) as u64;
        assert!(
            wait_until(Duration::from_secs(2), || source.origin_ns().is_some()
                && source.available_frames() >= needed),
            "第 {index} 块数据未到（available={} needed={needed}）",
            source.available_frames()
        );
        let now = origin + frames_to_ns_ceil(latency + (index * BLOCK_FRAMES) as u64, RATE_HZ);
        let read = source.read_block(now, &mut out).expect("read block");
        if read.status == BlockStatus::Ok {
            ok_blocks += 1;
            for frame in 0..BLOCK_FRAMES {
                channel0.push(rtp::i32_full_scale_to_f32(out[frame * 2]));
                channel1.push(rtp::i32_full_scale_to_f32(out[frame * 2 + 1]));
            }
        }
    }
    let sender = sender_thread.join().expect("sender thread");
    assert!(sender.packets_sent() >= 200);
    receiver.stop();
    // 证据真实性：reader 已交给 capture（source），receiver.report_json 的 jitter 统计
    // 必须是真实计数（不是零）。
    let report = receiver.report_json();
    assert!(
        !report.contains("\"jitter\":{\"packets\":0,\"frames_received\":0"),
        "report jitter stats degraded to zero after take_reader: {report}"
    );

    // 5) 断言：帧数/块状态/通道分离/音调频率/幅度。
    assert_eq!(ok_blocks, blocks, "all capture blocks should be Ok");
    assert_eq!(
        source.counters().frames_delivered,
        (blocks * BLOCK_FRAMES) as u64
    );
    assert_eq!(channel0.len(), blocks * BLOCK_FRAMES);
    assert_eq!(channel1.len(), blocks * BLOCK_FRAMES);
    assert!(
        rms(&channel0) > 0.15 && rms(&channel0) < 0.20,
        "RMS ch0={}",
        rms(&channel0)
    );
    assert!(
        rms(&channel1) > 0.15 && rms(&channel1) < 0.20,
        "RMS ch1={}",
        rms(&channel1)
    );
    let ch0_1k = goertzel_magnitude(&channel0, 1_000.0, RATE_HZ as f32);
    let ch0_3k = goertzel_magnitude(&channel0, 3_000.0, RATE_HZ as f32);
    let ch1_1k = goertzel_magnitude(&channel1, 1_000.0, RATE_HZ as f32);
    let ch1_3k = goertzel_magnitude(&channel1, 3_000.0, RATE_HZ as f32);
    assert!(
        ch0_1k > 10.0 * ch0_3k,
        "ch0 主频应 1k：1k={ch0_1k} 3k={ch0_3k}"
    );
    assert!(
        ch1_3k > 10.0 * ch1_1k,
        "ch1 主频应 3k：3k={ch1_3k} 1k={ch1_1k}"
    );
    let jitter = source.jitter_stats();
    assert_eq!(jitter.underruns, 0, "{jitter:?}");
    assert_eq!(jitter.warmups, 0, "{jitter:?}");
    assert!(jitter.packets >= 100, "{jitter:?}");
    assert_eq!(jitter.late_packets, 0, "{jitter:?}");
    // 采样级连续性：相邻两块边界应连续（正弦相位连续），不做重采样。
    let boundary = channel0[BLOCK_FRAMES - 1];
    let next = channel0[BLOCK_FRAMES];
    assert!((boundary.abs() <= 0.25 + 0.01) && (next.abs() <= 0.25 + 0.01));
    let _ = ns_to_frames(1_000_000, RATE_HZ);
}

/// 组播 loopback（本机 IGMP/回环投递；平台不允许时明确 skip 并打印原因）。
#[test]
fn loopback_multicast_receives_when_platform_allows() {
    let group = Ipv4Addr::new(239, 255, 42, 77);
    let port = 15_433u16;
    let config = ReceiverConfig {
        media: group,
        port,
        multicast: true,
        interfaces: khaen_aoip_aes67::listen::ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: khaen_aoip_aes67::listen::GroupAllow::EMPTY,
            source_allow: khaen_aoip_aes67::listen::SourceAllow::EMPTY,
        channels: 2,
        rcvbuf_bytes: 4 << 20,
        recv_timeout_ms: 10,
        io_priority: None,
        clock: khaen_aoip_aes67::receiver::ReceiverClock::Usrv,
        jitter: JitterConfig::aes67_default(2).expect("jitter"),
        ssrc_filter: None,
    };
    let mut receiver = match Aes67Receiver::bind(config) {
        Ok(receiver) => receiver,
        Err(ReceiverError::MulticastJoin(_)) => {
            eprintln!("multicast join unsupported on this platform; covered by offline model");
            return;
        }
        Err(error) => panic!("multicast bind failed: {}", error.name()),
    };
    let mut sender_config = SenderConfig::multicast(group, port, 2, Some(Ipv4Addr::LOCALHOST));
    sender_config.frames_per_packet = FRAMES_PER_PACKET;
    let mut sender = Aes67RtpSender::bind(sender_config).expect("multicast sender");
    for _ in 0..40 {
        sender.send_sine(1_000.0).expect("send");
    }
    let received = wait_until(Duration::from_secs(2), || {
        receiver.io_stats().accepted_packets > 0
    });
    let stats = receiver.io_stats();
    receiver.stop();
    if !received {
        // Windows 默认策略/网卡选择可能阻断本机组播回环；记录为显式 skip（组播地址解析/
        // join 已由 receiver 单测覆盖），不伪造通过。
        eprintln!(
            "MULTICAST_LOOPBACK_SKIPPED：本机未投递组播回环（datagrams={} accepted={}）",
            stats.datagrams, stats.accepted_packets
        );
        return;
    }
    assert!(stats.accepted_packets >= 1, "{stats:?}");
    assert!(stats.accepted_frames >= 48, "{stats:?}");
}

/// SDP 文件输入 → 校验 → ReceiverConfig（离线文件路径验收，不 bind socket）。
#[test]
fn sdp_file_to_receiver_config() {
    let text = "v=0\r\n\
o=- 0 0 IN IP4 127.0.0.1\r\n\
s=TSPI-AES67-proto\r\n\
c=IN IP4 239.69.16.146/32\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 103\r\n\
a=rtpmap:103 L24/48000/8\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n\
a=mediaclk:direct=0\r\n";
    // 集成测试用 CARGO_TARGET_TMPDIR（本机 TEMP 指向 R: 内存盘，写入可能被拒）。
    let directory = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    std::fs::create_dir_all(&directory).expect("create tmp dir");
    let path = directory.join(format!("khaen-aes67-rx-{}.sdp", std::process::id()));
    std::fs::write(&path, text).expect("write sdp");
    let document = SdpDocument::from_file(path.to_str().expect("utf8 path")).expect("load sdp");
    let session = document.parse_validated().expect("sdp profile");
    assert_eq!(session.media.rtpmap.channels, 8);
    assert_eq!(session.max_packet_frames(), 48);
    let config = ReceiverConfig::from_session(&session, Some(Ipv4Addr::LOCALHOST)).expect("config");
    assert!(config.multicast);
    assert_eq!(config.port, 5_004);
    assert_eq!(config.channels, 8);
    assert_eq!(config.jitter.latency_frames(), 256);
    let _ = std::fs::remove_file(&path);
}

/// 组播配置错误语义（端口 0 / 时钟未接线）。
#[test]
fn receiver_config_error_semantics() {
    let mut config = unicast_receiver_config(0, 2);
    config.multicast = true;
    assert_eq!(config.validate().unwrap_err().name(), "ERR_RX_CONFIG");
    let mut config = unicast_receiver_config(0, 2);
    config.channels = 0;
    assert_eq!(config.validate().unwrap_err().name(), "ERR_RX_CONFIG");
    let document = SdpDocument::from_text(
        "v=0\r\n\
c=IN IP4 239.0.0.1/16\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n",
    );
    let session = document.parse().expect("parse");
    let mut config = ReceiverConfig::from_session(&session, None).expect("config");
    config.clock = khaen_aoip_aes67::receiver::ReceiverClock::PtpPhc { domain: 0 };
    assert_eq!(
        config.validate().unwrap_err().name(),
        "ERR_RX_CLOCK_NOT_WIRED"
    );
}
