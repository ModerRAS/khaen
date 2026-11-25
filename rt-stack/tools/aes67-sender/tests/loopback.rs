//! In-process sender -> receiver loopback integration tests.
//!
//! These do not touch WASAPI; they prove RTP/L24 framing, pacing, sequence and
//! timestamp monotonicity, channel mapping and tone content over real UDP.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use aes67_sender::analysis;
use aes67_sender::pacer::{run_sender, SenderConfig};
use aes67_sender::pcm;
use aes67_sender::rtp::{RtpHeader, RTP_HEADER_LEN};
use aes67_sender::source::{SineSource, Source, WavFileSource};
use aes67_sender::wav::{write_wav_file, WavWriteSpec};

struct RxResult {
    packets: u64,
    frames: u64,
    samples: Vec<f32>,
    seq_first: u16,
    seq_last: u16,
    ts_first: u32,
    ts_last: u32,
    ssrc: u32,
    payload_sizes: Vec<usize>,
    gaps: u64,
    duplicates: u64,
    out_of_order: u64,
}

fn receive(port: u16, channels: usize, silence: Duration, max_wait: Duration) -> RxResult {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut buf = [0u8; 4096];
    let start = Instant::now();
    let mut last = Instant::now();
    let mut packets = 0u64;
    let mut samples: Vec<f32> = Vec::new();
    let mut payload_sizes = Vec::new();
    let mut seq_first = None;
    let mut seq_last: Option<u16> = None;
    let mut ts_first = None;
    let mut ts_last = None;
    let mut ssrc = 0u32;
    let (mut gaps, mut duplicates, mut out_of_order) = (0u64, 0u64, 0u64);
    loop {
        if packets > 0 && last.elapsed() > silence {
            break;
        }
        if packets == 0 && start.elapsed() > max_wait {
            break;
        }
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                let hdr = RtpHeader::parse(&buf[..n]).expect("RTP header");
                let payload = &buf[RTP_HEADER_LEN..n];
                assert_eq!(payload.len() % (3 * channels), 0);
                payload_sizes.push(payload.len());
                if seq_first.is_none() {
                    seq_first = Some(hdr.sequence);
                    ts_first = Some(hdr.timestamp);
                    ssrc = hdr.ssrc;
                }
                if let Some(prev) = seq_last {
                    let diff = hdr.sequence.wrapping_sub(prev) as i16;
                    if diff == 0 {
                        duplicates += 1;
                    } else if diff > 1 {
                        gaps += (diff - 1) as u64;
                    } else if diff < 0 {
                        out_of_order += 1;
                    }
                }
                seq_last = Some(hdr.sequence);
                ts_last = Some(hdr.timestamp);
                let mut decoded = Vec::new();
                pcm::decode_l24(payload, &mut decoded);
                samples.extend_from_slice(&decoded);
                packets += 1;
                last = Instant::now();
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => panic!("recv failed: {e}"),
        }
    }
    RxResult {
        packets,
        frames: (samples.len() / channels) as u64,
        samples,
        seq_first: seq_first.unwrap_or(0),
        seq_last: seq_last.unwrap_or(0),
        ts_first: ts_first.unwrap_or(0),
        ts_last: ts_last.unwrap_or(0),
        ssrc,
        payload_sizes,
        gaps,
        duplicates,
        out_of_order,
    }
}

fn channel_of(samples: &[f32], channels: usize, c: usize) -> Vec<f32> {
    samples.chunks(channels).map(|f| f[c]).collect()
}

#[test]
fn sine_loopback_1ms_stereo_tone_content() {
    let port = {
        let s = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        s.local_addr().unwrap().port()
    };
    let rx =
        thread::spawn(move || receive(port, 2, Duration::from_millis(400), Duration::from_secs(3)));
    thread::sleep(Duration::from_millis(50));

    let mut src = SineSource::new(2, 1000.0, 2000.0, 0.5, Some(0.5)).unwrap();
    let cfg = SenderConfig {
        dest: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        multicast: false,
        iface_ip: None,
        ptime_ms: 1.0,
        duration_s: Some(0.5),
        verbose: false,
        ..Default::default()
    };
    let run = run_sender(&cfg, &mut src).unwrap();
    assert_eq!(
        run.stats.packets_sent, 500,
        "expected 500 packets for 0.5 s @1 ms"
    );
    assert_eq!(run.stats.frames_sent, 24_000);
    assert_eq!(run.stats.underrun_packets, 0);

    let got = rx.join().unwrap();
    assert_eq!(got.packets, 500);
    assert_eq!(got.frames, 24_000);
    assert_eq!(got.gaps, 0, "sequence gaps: {}", got.gaps);
    assert_eq!(got.duplicates, 0);
    assert_eq!(got.out_of_order, 0);
    assert_eq!(got.seq_first, 0);
    assert_eq!(got.seq_last, 499);
    assert_eq!(got.ssrc, 0x5453_5049);
    assert_eq!(got.ts_first, 0);
    assert_eq!(got.ts_last, 499 * 48);
    assert!(got.payload_sizes.iter().all(|s| *s == 48 * 2 * 3));

    let left = channel_of(&got.samples, 2, 0);
    let right = channel_of(&got.samples, 2, 1);
    let fl = analysis::estimate_frequency(&left, 48_000.0);
    let fr = analysis::estimate_frequency(&right, 48_000.0);
    assert!((fl - 1000.0).abs() < 5.0, "left tone {fl} Hz");
    assert!((fr - 2000.0).abs() < 5.0, "right tone {fr} Hz");
    let amp_l = analysis::goertzel_amplitude(&left, 48_000.0, 1000.0);
    let amp_r = analysis::goertzel_amplitude(&right, 48_000.0, 2000.0);
    assert!((amp_l - 0.5).abs() < 0.02, "left amp {amp_l}");
    assert!((amp_r - 0.5).abs() < 0.02, "right amp {amp_r}");
    // channel isolation
    assert!(analysis::goertzel_amplitude(&left, 48_000.0, 2000.0) < 0.05);
    assert!(analysis::goertzel_amplitude(&right, 48_000.0, 1000.0) < 0.05);
}

#[test]
fn sine_loopback_0125ms_ptime_timestamps() {
    let port = {
        let s = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        s.local_addr().unwrap().port()
    };
    let rx =
        thread::spawn(move || receive(port, 2, Duration::from_millis(300), Duration::from_secs(3)));
    thread::sleep(Duration::from_millis(50));
    let mut src = SineSource::new(2, 1000.0, 2000.0, 0.5, Some(0.2)).unwrap();
    let cfg = SenderConfig {
        dest: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        multicast: false,
        ptime_ms: 0.125,
        duration_s: Some(0.2),
        verbose: false,
        ..Default::default()
    };
    let run = run_sender(&cfg, &mut src).unwrap();
    assert_eq!(run.stats.packets_sent, 1600); // 0.2 s / 0.125 ms
    assert_eq!(run.stats.frames_sent, 9_600);
    let got = rx.join().unwrap();
    assert_eq!(got.packets, 1600);
    assert_eq!(got.gaps, 0);
    assert!(got.payload_sizes.iter().all(|s| *s == 6 * 2 * 3));
    assert_eq!(got.ts_last, got.ts_first + 1599 * 6);
}

#[test]
fn wav_file_loopback_forms_rising_edge() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-tmp")
        .join("wav-loopback");
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone48k_stereo_l24.wav");
    let n = 12_000usize; // 0.25 s
    let mut samples = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / 48_000.0;
        samples.push((2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.5);
        samples.push((2.0 * std::f32::consts::PI * 2000.0 * t).sin() * 0.5);
    }
    write_wav_file(&wav, WavWriteSpec::l24_48k(2), &samples).unwrap();

    let port = {
        let s = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        s.local_addr().unwrap().port()
    };
    let rx =
        thread::spawn(move || receive(port, 2, Duration::from_millis(400), Duration::from_secs(3)));
    thread::sleep(Duration::from_millis(50));
    let mut src = WavFileSource::open(&wav, false).unwrap();
    assert_eq!(src.channels(), 2);
    let cfg = SenderConfig {
        dest: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        multicast: false,
        duration_s: None, // EOF-bounded
        verbose: false,
        ..Default::default()
    };
    let run = run_sender(&cfg, &mut src).unwrap();
    assert_eq!(run.stats.frames_sent, n as u64);
    assert_eq!(run.stats.packets_sent, 250); // 12000/48
    let got = rx.join().unwrap();
    assert_eq!(got.frames, n as u64);
    assert_eq!(got.gaps, 0);
    let left = channel_of(&got.samples, 2, 0);
    assert!((analysis::estimate_frequency(&left, 48_000.0) - 1000.0).abs() < 5.0);
}

#[test]
fn sender_uses_real_time_pacing() {
    let port = {
        let s = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        s.local_addr().unwrap().port()
    };
    let rx =
        thread::spawn(move || receive(port, 2, Duration::from_millis(300), Duration::from_secs(3)));
    thread::sleep(Duration::from_millis(50));
    let mut src = SineSource::new(2, 1000.0, 2000.0, 0.5, Some(0.4)).unwrap();
    let cfg = SenderConfig {
        dest: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        multicast: false,
        ptime_ms: 1.0,
        duration_s: Some(0.4),
        verbose: false,
        ..Default::default()
    };
    let start = Instant::now();
    let run = run_sender(&cfg, &mut src).unwrap();
    let elapsed = start.elapsed();
    // 0.4 s of audio must take about 0.4 s (warm-up + scheduling slack).
    assert!(
        elapsed >= Duration::from_millis(380) && elapsed <= Duration::from_millis(900),
        "pacing elapsed {elapsed:?}"
    );
    assert_eq!(run.stats.packets_sent, 400);
    let _ = rx.join().unwrap();
}
