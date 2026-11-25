//! Real-time RTP send loop: QPC deadline pacing, RTP/L24 packetization,
//! underrun/deadline statistics.
//!
//! Clock note: no PTP/PHC available on this Windows host; RTP timestamps are
//! derived from the local monotonic clock (QPC). See CLOCK-AND-SYNC.md.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use anyhow::{bail, Context, Result};
use socket2::{Domain, Protocol, Socket, Type};

use crate::clock::{
    frames_to_ns, LatenessStats, MonoClock, ThreadPriorityGuard, TimerResolutionGuard,
};
use crate::rtp::Packetizer;
use crate::source::Source;
use crate::AES67_RATE;

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub dest: SocketAddrV4,
    pub multicast: bool,
    pub iface_ip: Option<Ipv4Addr>,
    pub ttl: u32,
    pub mcast_loop: bool,
    pub ptime_ms: f64,
    pub duration_s: Option<f64>,
    pub payload_type: u8,
    pub ssrc: u32,
    pub initial_seq: u16,
    pub initial_ts: u32,
    pub warmup_ms: f64,
    /// Max time to wait for the first captured audio (WASAPI loopback sources may
    /// need a few buffers before the first packet arrives).
    pub warmup_timeout_ms: f64,
    pub verbose: bool,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            dest: SocketAddrV4::new(Ipv4Addr::new(239, 69, 63, 10), 5004),
            multicast: true,
            iface_ip: None,
            ttl: 1,
            mcast_loop: true,
            ptime_ms: crate::DEFAULT_PTIME_MS,
            duration_s: None,
            payload_type: crate::DEFAULT_PAYLOAD_TYPE,
            ssrc: 0x5453_5049, // "TSPI"
            initial_seq: 0,
            initial_ts: 0,
            warmup_ms: 50.0,
            warmup_timeout_ms: 2000.0,
            verbose: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SenderStats {
    pub packets_sent: u64,
    pub frames_sent: u64,
    pub payload_bytes: u64,
    pub datagram_bytes: u64,
    pub send_errors: u64,
    pub underrun_packets: u64,
    pub underrun_frames: u64,
    pub eof_early: bool,
    pub first_seq: u16,
    pub last_seq: u16,
    pub first_ts: u32,
    pub last_ts: u32,
    pub ssrc: u32,
    pub payload_type: u8,
    pub ptime_ms: f64,
    pub samples_per_packet: u32,
    pub channels: usize,
    pub dest: String,
    pub iface: String,
    pub ttl: u32,
    pub multicast: bool,
    pub clock_source: String,
    pub warmup_frames: u64,
    pub run_time_s: f64,
    pub effective_packet_rate_hz: f64,
    pub effective_bitrate_kbps: f64,
    pub late: LatenessStats,
}

pub struct SenderRun {
    pub stats: SenderStats,
    /// Hex dump of the first datagrams (kept for evidence; max 4, capped bytes).
    pub first_packets_hex: Vec<String>,
}

fn build_socket(cfg: &SenderConfig) -> Result<Socket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .context("failed to create UDP socket")?;
    let _ = sock.set_reuse_address(true);
    match cfg.iface_ip {
        Some(ip) => {
            sock.bind(&SocketAddrV4::new(ip, 0).into())
                .with_context(|| format!("failed to bind sender socket to {ip}"))?;
        }
        None => {
            sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into())
                .context("failed to bind sender socket")?;
        }
    }
    if cfg.multicast {
        if let Some(ip) = cfg.iface_ip {
            sock.set_multicast_if_v4(&ip)
                .with_context(|| format!("failed to set IP_MULTICAST_IF to {ip}"))?;
        }
        sock.set_multicast_ttl_v4(cfg.ttl)
            .context("failed to set IP_MULTICAST_TTL")?;
        sock.set_multicast_loop_v4(cfg.mcast_loop)
            .context("failed to set IP_MULTICAST_LOOP")?;
    }
    Ok(sock)
}

/// Run the sender loop until duration/EOF. Returns stats + first datagrams.
pub fn run_sender(cfg: &SenderConfig, source: &mut dyn Source) -> Result<SenderRun> {
    let channels = source.channels();
    anyhow::ensure!((1..=8).contains(&channels), "channels must be 1..=8");

    let samples_f = AES67_RATE as f64 * cfg.ptime_ms / 1000.0;
    let samples_per_packet = samples_f.round() as u32;
    if (samples_f - samples_per_packet as f64).abs() > 1e-6 {
        bail!(
            "ptime {} ms at 48 kHz is not an integral number of samples (got {samples_f})",
            cfg.ptime_ms
        );
    }
    anyhow::ensure!(samples_per_packet >= 1, "ptime too small");
    if let Some(d) = cfg.duration_s {
        anyhow::ensure!(d > 0.0, "duration must be > 0");
    }
    let period_ns = frames_to_ns(samples_per_packet as u64);
    let dest = cfg.dest;
    let sock = build_socket(cfg)?;

    let clock = MonoClock::new()?;
    let timer_guard = TimerResolutionGuard::request_1ms();
    let _prio_guard = ThreadPriorityGuard::boost_highest();
    if cfg.verbose {
        eprintln!(
            "sender: dest={} multicast={} iface={} ptime={}ms ({} frames/pkt) channels={} pt={} ssrc=0x{:08x} timer1ms={}",
            dest,
            cfg.multicast,
            cfg.iface_ip
                .map(|i| i.to_string())
                .unwrap_or_else(|| "<default>".into()),
            cfg.ptime_ms,
            samples_per_packet,
            channels,
            cfg.payload_type,
            cfg.ssrc,
            timer_guard.is_active()
        );
        eprintln!("sender: source = {}", source.description());
    }

    let mut stats = SenderStats {
        ssrc: cfg.ssrc,
        payload_type: cfg.payload_type,
        ptime_ms: cfg.ptime_ms,
        samples_per_packet,
        channels,
        dest: dest.to_string(),
        iface: cfg
            .iface_ip
            .map(|i| i.to_string())
            .unwrap_or_else(|| "<default>".into()),
        ttl: cfg.ttl,
        multicast: cfg.multicast,
        clock_source: "local-monotonic-qpc".to_string(),
        ..Default::default()
    };

    // Warm-up: pre-read a small number of frames so WASAPI capture starts with a
    // filled buffer (and file sources start with a consistent lead).
    let warmup_frames = ((AES67_RATE as f64 * cfg.warmup_ms / 1000.0).round() as u64).max(0);
    let mut prebuffer: Vec<f32> = Vec::new();
    if warmup_frames > 0 {
        let pkt_frames = samples_per_packet as usize;
        let target_frames = warmup_frames as usize;
        let mut got = 0usize;
        let warm_deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(cfg.warmup_timeout_ms.max(1.0) as u64);
        while got < target_frames && std::time::Instant::now() < warm_deadline {
            let want = pkt_frames.min(target_frames - got);
            let mut buf = vec![0.0f32; want * channels];
            let st = source.read_into(&mut buf, want)?;
            if st.produced_frames == 0 {
                if st.eof {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            prebuffer.extend_from_slice(&buf[..st.produced_frames * channels]);
            got += st.produced_frames;
            if st.eof {
                break;
            }
        }
    }
    stats.warmup_frames = (prebuffer.len() / channels) as u64;

    let total_frames_limit: Option<u64> = cfg
        .duration_s
        .map(|d| (d * AES67_RATE as f64).round() as u64);

    let mut packetizer =
        Packetizer::new(cfg.payload_type, cfg.initial_seq, cfg.initial_ts, cfg.ssrc);
    stats.first_seq = cfg.initial_seq;
    stats.first_ts = cfg.initial_ts;

    let mut pkt = Vec::with_capacity(12 + samples_per_packet as usize * channels * 3);
    let mut samples = vec![0.0f32; samples_per_packet as usize * channels];
    let mut pre_off = 0usize; // in f32 samples
    let t0 = clock.now_ns();
    let mut late = LatenessStats::new();
    let mut frames_sent: u64 = 0;
    let mut eof_early = false;
    let mut send_errors = 0u64;
    let mut first_datagrams: Vec<String> = Vec::new();

    loop {
        if let Some(limit) = total_frames_limit {
            if frames_sent >= limit {
                break;
            }
        }

        // Fill one packet of audio: use prebuffer first, then the live source.
        let need = samples.len();
        let mut filled = 0usize;
        if pre_off < prebuffer.len() {
            let take = (prebuffer.len() - pre_off).min(need);
            samples[..take].copy_from_slice(&prebuffer[pre_off..pre_off + take]);
            pre_off += take;
            filled = take;
        }
        let mut underrun_pkt = false;
        let mut source_eof = false;
        if filled < need {
            let frames_left = (need - filled) / channels;
            let st = source.read_into(&mut samples[filled..], frames_left)?;
            filled += st.produced_frames * channels;
            if st.underrun_frames > 0 {
                stats.underrun_frames += st.underrun_frames as u64;
                underrun_pkt = true;
            }
            source_eof = st.eof;
        }
        if filled == 0 {
            if source_eof {
                eof_early = true;
                break;
            }
            // Live capture temporarily empty (WASAPI warm-up / scheduling gap):
            // keep the real-time stream alive by sending silence instead of exiting.
            stats.underrun_packets += 1;
            stats.underrun_frames += samples_per_packet as u64;
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        for v in samples[filled..].iter_mut() {
            *v = 0.0;
        }
        let frames_this = filled / channels;
        if underrun_pkt {
            stats.underrun_packets += 1;
        }

        // Encode RTP packet.
        packetizer.next_packet(&samples[..filled], false, &mut pkt);
        packetizer.advance(frames_this as u32);

        // Deadline for this packet (absolute, from t0 + n * period).
        let deadline =
            t0 + ((frames_sent as u128 * period_ns as u128) / samples_per_packet as u128) as u64;
        if frames_sent > 0 {
            clock.sleep_until(deadline);
        }

        let sa: SocketAddr = SocketAddr::V4(dest);
        match sock.send_to(&pkt, &sa.into()) {
            Ok(sent) => {
                stats.packets_sent += 1;
                stats.datagram_bytes += sent as u64;
                stats.payload_bytes += (pkt.len() - 12) as u64;
                if first_datagrams.len() < 4 {
                    first_datagrams.push(hex_dump(&pkt, 96));
                }
            }
            Err(e) => {
                send_errors += 1;
                stats.send_errors += 1;
                if cfg.verbose && send_errors <= 3 {
                    eprintln!("sender: send_to {dest} failed: {e}");
                }
            }
        }

        let now = clock.now_ns();
        late.observe(now.saturating_sub(deadline), period_ns);

        stats.frames_sent += frames_this as u64;
        frames_sent += frames_this as u64;
        if cfg.verbose && stats.packets_sent % 1000 == 0 && stats.packets_sent > 0 {
            eprintln!(
                "sender: {} packets, {} frames, underruns={}, p99_late={}us",
                stats.packets_sent,
                stats.frames_sent,
                stats.underrun_packets,
                late.percentile_ns(0.99) / 1000
            );
        }
        if eof_early && filled < need {
            break;
        }
    }

    let elapsed_ns = clock.now_ns().saturating_sub(t0).max(1);
    stats.run_time_s = elapsed_ns as f64 / 1e9;
    stats.effective_packet_rate_hz = stats.packets_sent as f64 / stats.run_time_s;
    stats.effective_bitrate_kbps = stats.datagram_bytes as f64 * 8.0 / stats.run_time_s / 1000.0;
    stats.eof_early = eof_early;
    stats.last_seq = cfg
        .initial_seq
        .wrapping_add(stats.packets_sent.saturating_sub(1) as u16);
    stats.last_ts = cfg.initial_ts.wrapping_add(stats.frames_sent as u32);
    stats.late = late;

    Ok(SenderRun {
        stats,
        first_packets_hex: first_datagrams,
    })
}

/// Parse an IPv4 interface address argument.
pub fn parse_iface(s: &str) -> Result<Ipv4Addr> {
    s.parse::<Ipv4Addr>()
        .with_context(|| format!("invalid IPv4 address {s:?}"))
}

/// Format a datagram as uppercase hex (evidence helper).
pub fn hex_dump(bytes: &[u8], max: usize) -> String {
    let n = bytes.len().min(max);
    let mut s = String::with_capacity(n * 3);
    for (i, b) in bytes.iter().take(n).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    if bytes.len() > n {
        s.push_str(" ...");
    }
    s
}
