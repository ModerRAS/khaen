//! aes67-recv — minimal Rust RTP/L24 receiver used for sender loopback self-check.
//!
//! This is intentionally small and decoupled from rt-aes67rx's `crates/aoip-aes67`
//! (task-235). It exists so task-236 can verify the Windows sender end-to-end on
//! one machine and produce machine-checkable evidence (tone/frames/channels).

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use aes67_sender::analysis;
use aes67_sender::cli::Args;
use aes67_sender::json::J;
use aes67_sender::pcm;
use aes67_sender::rtp::{RtpHeader, RTP_HEADER_LEN};
use aes67_sender::wav::{WavWriteSpec, WavWriter};
use aes67_sender::AES67_RATE;

const USAGE: &str = r#"aes67-recv — minimal RTP/L24 receiver for AES67 sender loopback self-check

USAGE:
  aes67-recv --port <n> [options]

NETWORK:
  --port <n>                 UDP listen port (default 5004)
  --multicast <group>        join this IPv4 multicast group (e.g. 239.69.63.10)
  --iface-ip <ipv4>          interface to join the multicast group on
  --channels <n>             payload channels (default 2)
  --bind <ipv4>              bind address (default 0.0.0.0)

STOP CONDITIONS:
  --duration <s>             stop once this many seconds of packets were received
  --silence-ms <ms>          stop after this much silence following the last packet (default 1200)
  --max-wait-ms <ms>         fail if no packet arrives within this window (default 10000)

EVIDENCE / CHECKS:
  --wav-out <path>           write received audio as WAV
  --wav-bits <16|24>         WAV bit depth (default 24)
  --stats-json <path>        write receiver statistics/assertions JSON
  --expect-tone <hz>         expected tone per channel; repeat once per channel (e.g. 1000 then 2000)
  --expect-tone-tol <hz>     frequency tolerance (default 5)
  --expect-min-amp <0..1>    minimum Goertzel amplitude for the expected tone (default 0.01)
  --dump-rtp <n>             include first n RTP datagrams (hex) in JSON/stderr (default 2)
  --quiet                    less stderr output
  --help                     this help

EXIT CODE: 0 = PASS, 2 = FAIL (checks), 1 = usage/runtime error.
"#;

#[derive(Debug, Default)]
struct RxAccum {
    packets: u64,
    payload_bytes: u64,
    unique_seqs: HashSet<u16>,
    first_seq: Option<u16>,
    last_seq: Option<u16>,
    missing: u64,
    duplicates: u64,
    out_of_order: u64,
    first_ts: Option<u32>,
    last_ts: Option<u32>,
    ts_deltas: u64,
    ts_delta_mismatches: u64,
    expected_delta: Option<u32>,
    frames_per_packet: Option<usize>,
    payload_len_mismatches: u64,
    ssrc: Option<u32>,
    payload_type: Option<u8>,
    ssrc_changes: u64,
    samples: Vec<f32>,
    first_packet_hex: Vec<String>,
    interval_ms: Vec<f64>,
}

fn main() {
    match real_main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn real_main() -> Result<i32> {
    let args = Args::from_env()?;
    if args.help {
        print!("{USAGE}");
        return Ok(0);
    }
    let quiet = args.bool_flag("quiet");
    let port = args.u16_or("port", 5004)?;
    let channels = args.usize_or("channels", 2)?;
    anyhow::ensure!((1..=8).contains(&channels), "--channels must be 1..=8");
    let multicast: Option<Ipv4Addr> = match args.opt_string("multicast") {
        Some(s) => {
            let ip: Ipv4Addr = s.parse()?;
            anyhow::ensure!(ip.is_multicast(), "{ip} is not a multicast address");
            Some(ip)
        }
        None => None,
    };
    let iface_ip = match args.opt_string("iface-ip") {
        Some(s) => Some(s.parse::<Ipv4Addr>()?),
        None => None,
    };
    let bind_ip = match args.opt_string("bind") {
        Some(s) => s.parse::<Ipv4Addr>()?,
        None => Ipv4Addr::UNSPECIFIED,
    };
    let duration = args.f64_opt("duration")?;
    let silence_ms = args.f64_or("silence-ms", 1200.0)?;
    let max_wait_ms = args.f64_or("max-wait-ms", 10_000.0)?;
    let wav_out: Option<PathBuf> = args.opt_string("wav-out").map(PathBuf::from);
    let wav_bits = args.u16_or("wav-bits", 24)?;
    anyhow::ensure!(
        wav_bits == 16 || wav_bits == 24,
        "--wav-bits must be 16 or 24"
    );
    let stats_json: Option<PathBuf> = args.opt_string("stats-json").map(PathBuf::from);
    let expect_tones: Vec<f64> = args
        .get_all("expect-tone")
        .iter()
        .map(|s| s.parse::<f64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("--expect-tone must be a number (Hz); repeat once per channel")?;
    let tone_tol = args.f64_or("expect-tone-tol", 5.0)?;
    let min_amp = args.f64_or("expect-min-amp", 0.01)?;
    let dump_rtp = args.usize_or("dump-rtp", 2)?;

    // --- socket (std UdpSocket: supports join_multicast_v4 + read timeout) ---
    let sock = UdpSocket::bind(SocketAddrV4::new(bind_ip, port))
        .with_context(|| format!("bind {bind_ip}:{port} failed"))?;
    if let Some(group) = multicast {
        let iface = iface_ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
        sock.join_multicast_v4(&group, &iface)
            .with_context(|| format!("join multicast {group} on {iface} failed"))?;
    }
    sock.set_read_timeout(Some(Duration::from_millis(100)))?;
    if !quiet {
        eprintln!(
            "recv: listening on {}:{} multicast={:?} iface={:?} channels={}",
            bind_ip, port, multicast, iface_ip, channels
        );
    }

    let mut acc = RxAccum::default();
    let mut buf = [0u8; 4096];
    let start = Instant::now();
    let mut first_arrival: Option<Instant> = None;
    let mut last_arrival: Option<Instant> = None;
    #[allow(unused_assignments)]
    let mut received_stop_reason = "duration";

    loop {
        // Stop conditions before blocking again.
        if let Some(first) = first_arrival {
            if let Some(dur) = duration {
                if first.elapsed().as_secs_f64() >= dur {
                    received_stop_reason = "duration";
                    break;
                }
            }
        }
        if first_arrival.is_none() && start.elapsed().as_secs_f64() * 1000.0 > max_wait_ms {
            received_stop_reason = "no-packets";
            break;
        }
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Some(last) = last_arrival {
                    if last.elapsed().as_secs_f64() * 1000.0 > silence_ms {
                        received_stop_reason = "silence";
                        break;
                    }
                }
                if n < RTP_HEADER_LEN + 3 * channels {
                    continue;
                }
                let hdr = match RtpHeader::parse(&buf[..n]) {
                    Some(h) => h,
                    None => continue,
                };
                let off = match hdr.payload_offset() {
                    Some(o) if o <= n => o,
                    _ => continue,
                };
                let payload = &buf[off..n];
                if payload.len() % (3 * channels) != 0 {
                    acc.payload_len_mismatches += 1;
                    continue;
                }
                let frames = payload.len() / (3 * channels);
                if frames == 0 {
                    continue;
                }
                if acc.first_packet_hex.len() < dump_rtp {
                    acc.first_packet_hex
                        .push(aes67_sender::pacer::hex_dump(&buf[..n], 96));
                }
                // Sequence accounting.
                match acc.last_seq {
                    None => {
                        acc.first_seq = Some(hdr.sequence);
                    }
                    Some(last) => {
                        let diff = hdr.sequence.wrapping_sub(last) as i16;
                        if diff == 0 {
                            acc.duplicates += 1;
                        } else if diff > 0 {
                            if diff > 1 {
                                acc.missing += (diff - 1) as u64;
                            }
                        } else {
                            acc.out_of_order += 1;
                        }
                    }
                }
                acc.last_seq = Some(hdr.sequence);
                acc.unique_seqs.insert(hdr.sequence);
                // Timestamp accounting.
                match acc.last_ts {
                    None => acc.first_ts = Some(hdr.timestamp),
                    Some(last) => {
                        let delta = hdr.timestamp.wrapping_sub(last);
                        acc.ts_deltas += 1;
                        match acc.expected_delta {
                            None => acc.expected_delta = Some(delta),
                            Some(exp) => {
                                if exp != delta {
                                    acc.ts_delta_mismatches += 1;
                                }
                            }
                        }
                    }
                }
                acc.last_ts = Some(hdr.timestamp);
                // SSRC / PT accounting.
                match acc.ssrc {
                    None => acc.ssrc = Some(hdr.ssrc),
                    Some(prev) if prev != hdr.ssrc => acc.ssrc_changes += 1,
                    _ => {}
                }
                match acc.payload_type {
                    None => acc.payload_type = Some(hdr.payload_type),
                    _ => {}
                }
                if acc.frames_per_packet.is_none() {
                    acc.frames_per_packet = Some(frames);
                } else if acc.frames_per_packet != Some(frames) {
                    acc.payload_len_mismatches += 1;
                }
                // Decode L24 payload.
                let mut decoded = Vec::with_capacity(payload.len() / 3);
                pcm::decode_l24(payload, &mut decoded);
                acc.samples.extend_from_slice(&decoded);
                acc.packets += 1;
                acc.payload_bytes += payload.len() as u64;
                let now = Instant::now();
                if first_arrival.is_none() {
                    first_arrival = Some(now);
                } else if let Some(last) = last_arrival {
                    acc.interval_ms
                        .push(now.duration_since(last).as_secs_f64() * 1000.0);
                }
                last_arrival = Some(now);
                let _ = from;
                if !quiet && acc.packets % 1000 == 0 {
                    eprintln!(
                        "recv: {} packets, {} frames",
                        acc.packets,
                        acc.samples.len() / channels
                    );
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if let Some(last) = last_arrival {
                    if last.elapsed().as_secs_f64() * 1000.0 > silence_ms {
                        received_stop_reason = "silence";
                        break;
                    }
                }
                continue;
            }
            Err(e) => return Err(anyhow::anyhow!("recv_from failed: {e}")),
        }
    }

    let frames_total = acc.samples.len() / channels;
    let elapsed = first_arrival
        .map(|f| f.elapsed().as_secs_f64())
        .unwrap_or_else(|| start.elapsed().as_secs_f64());
    if !quiet {
        eprintln!(
            "recv: stop reason={} packets={} frames={} elapsed={:.3}s",
            received_stop_reason, acc.packets, frames_total, elapsed
        );
    }

    // --- analysis ---
    let mut per_channel = Vec::with_capacity(channels);
    let mut channel_vectors: Vec<Vec<f32>> = vec![Vec::with_capacity(frames_total); channels];
    for f in 0..frames_total {
        for (c, vec) in channel_vectors.iter_mut().enumerate() {
            vec.push(acc.samples[f * channels + c]);
        }
    }
    // Per-channel tone frequency refined by a Goertzel scan near the expected
    // value (the virtual-cable/audio-engine clock may shift the tone slightly).
    let refined: Vec<(f64, f64)> = expect_tones
        .iter()
        .enumerate()
        .map(|(c, hz)| {
            if c >= channel_vectors.len() {
                return (0.0, 0.0);
            }
            analysis::tone_estimate(&channel_vectors[c], AES67_RATE as f64, *hz, 0.05, 101)
        })
        .collect();
    let refined_freqs: Vec<f64> = refined.iter().map(|(f, _)| *f).collect();
    for (c, ch_samples) in channel_vectors.iter().enumerate() {
        let amp_at_expected: Vec<f64> = expect_tones
            .iter()
            .map(|hz| analysis::goertzel_amplitude(ch_samples, AES67_RATE as f64, *hz))
            .collect();
        let own_amp = refined.get(c).map(|(_, a)| *a).unwrap_or(0.0);
        let freq_est_zc = analysis::estimate_frequency(ch_samples, AES67_RATE as f64);
        let freq_est = match expect_tones.get(c) {
            Some(hz) => {
                analysis::estimate_frequency_near(ch_samples, AES67_RATE as f64, *hz, 0.05, 101)
            }
            None => freq_est_zc,
        };
        per_channel.push(J::obj(vec![
            ("channel", J::num(c as u64)),
            ("samples", J::num(ch_samples.len() as u64)),
            ("rms_db", J::num(round3(analysis::rms_db(ch_samples)))),
            (
                "peak_db",
                J::num(round3(db_amp(analysis::peak(ch_samples)))),
            ),
            ("freq_est_hz", J::num(round3(freq_est))),
            ("freq_est_zc_hz", J::num(round3(freq_est_zc))),
            (
                "goertzel_at_expected",
                J::arr(amp_at_expected.iter().map(|a| J::num(round6(*a)))),
            ),
            ("own_expected_amp", J::num(round6(own_amp))),
            (
                "own_expected_amp_nominal",
                J::num(round6(amp_at_expected.get(c).copied().unwrap_or(0.0))),
            ),
        ]));
    }

    // --- assertions ---
    let seq_ok = acc.payload_len_mismatches == 0;
    let seq_ok = seq_ok && acc.out_of_order == 0 && acc.duplicates == 0 && acc.missing == 0;
    let unique_expected = acc.last_seq.map(|last| {
        let first = acc.first_seq.unwrap();
        last.wrapping_sub(first) as u64 + 1
    });
    let seq_unique_ok = match unique_expected {
        Some(exp) => exp == acc.unique_seqs.len() as u64,
        None => false,
    };
    let ts_ok = acc.ts_deltas > 0
        && acc.ts_delta_mismatches == 0
        && acc
            .expected_delta
            .map(|d| d as usize == acc.frames_per_packet.unwrap_or(0))
            .unwrap_or(false);
    let payload_ok = acc.payload_len_mismatches == 0 && acc.frames_per_packet.is_some();

    let mut tone_ok = true;
    let mut level_ok = true;
    let mut channel_map_ok = true;
    for (c, hz) in expect_tones.iter().enumerate() {
        if c >= channel_vectors.len() {
            tone_ok = false;
            level_ok = false;
            break;
        }
        let ch = &channel_vectors[c];
        let est = refined_freqs[c];
        if (est - hz).abs() > tone_tol {
            tone_ok = false;
        }
        // Level must be measured at the *measured* tone frequency: a tiny clock
        // offset (e.g. virtual cable SRC ~0.05%) otherwise smears a Goertzel
        // evaluation at the nominal frequency over a multi-second window.
        let amp = refined[c].1;
        if amp < min_amp {
            level_ok = false;
            tone_ok = false;
        }
        // Channel isolation when two distinct tones are expected.
        for (other_c, other_hz) in expect_tones.iter().enumerate() {
            if other_c != c && (other_hz - hz).abs() > 1.0 {
                let other_amp =
                    analysis::goertzel_amplitude(ch, AES67_RATE as f64, refined_freqs[other_c]);
                if amp < 2.0 * other_amp.max(1e-9) {
                    channel_map_ok = false;
                }
            }
        }
    }
    let frames_ok = frames_total > 0;
    let verdict = seq_ok
        && seq_unique_ok
        && ts_ok
        && payload_ok
        && tone_ok
        && level_ok
        && channel_map_ok
        && frames_ok;

    // --- outputs ---
    if let Some(path) = &wav_out {
        let mut w = WavWriter::new(WavWriteSpec {
            channels: channels as u16,
            sample_rate: AES67_RATE,
            bits: wav_bits,
            float: false,
        })?;
        w.write_frames(&acc.samples)?;
        let written_frames = w.frames();
        std::fs::write(path, w.finish())
            .with_context(|| format!("failed to write WAV {:?}", path))?;
        if !quiet {
            eprintln!(
                "recv: wrote WAV {:?} ({} frames, {} ch)",
                path, written_frames, channels
            );
        }
    }

    let interval_stats = |v: &[f64], p: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = (((s.len() - 1) as f64) * p).round() as usize;
        round3(s[idx])
    };
    let json = J::obj(vec![
        ("tool", J::str("aes67-recv")),
        ("version", J::str(env!("CARGO_PKG_VERSION"))),
        ("bind", J::str(format!("{bind_ip}:{port}"))),
        (
            "multicast",
            multicast.map(|m| J::str(m.to_string())).unwrap_or(J::Null),
        ),
        (
            "iface_ip",
            iface_ip.map(|i| J::str(i.to_string())).unwrap_or(J::Null),
        ),
        ("channels", J::num(channels as u64)),
        ("packets", J::num(acc.packets)),
        ("frames", J::num(frames_total as u64)),
        ("payload_bytes", J::num(acc.payload_bytes)),
        ("elapsed_s", J::num(round3(elapsed))),
        ("stop_reason", J::str(received_stop_reason)),
        (
            "ssrc",
            acc.ssrc.map(|s| J::num(s as u64)).unwrap_or(J::Null),
        ),
        (
            "payload_type",
            acc.payload_type
                .map(|p| J::num(p as u64))
                .unwrap_or(J::Null),
        ),
        ("ssrc_changes", J::num(acc.ssrc_changes)),
        (
            "seq",
            J::obj(vec![
                (
                    "first",
                    acc.first_seq.map(|s| J::num(s as u64)).unwrap_or(J::Null),
                ),
                (
                    "last",
                    acc.last_seq.map(|s| J::num(s as u64)).unwrap_or(J::Null),
                ),
                ("unique", J::num(acc.unique_seqs.len() as u64)),
                ("missing", J::num(acc.missing)),
                ("duplicates", J::num(acc.duplicates)),
                ("out_of_order", J::num(acc.out_of_order)),
            ]),
        ),
        (
            "timestamp",
            J::obj(vec![
                (
                    "first",
                    acc.first_ts.map(|t| J::num(t as u64)).unwrap_or(J::Null),
                ),
                (
                    "last",
                    acc.last_ts.map(|t| J::num(t as u64)).unwrap_or(J::Null),
                ),
                (
                    "frames_per_packet",
                    acc.frames_per_packet
                        .map(|f| J::num(f as u64))
                        .unwrap_or(J::Null),
                ),
                (
                    "expected_delta",
                    acc.expected_delta
                        .map(|d| J::num(d as u64))
                        .unwrap_or(J::Null),
                ),
                ("deltas", J::num(acc.ts_deltas)),
                ("delta_mismatches", J::num(acc.ts_delta_mismatches)),
            ]),
        ),
        (
            "payload",
            J::obj(vec![
                (
                    "frames_per_packet",
                    acc.frames_per_packet
                        .map(|f| J::num(f as u64))
                        .unwrap_or(J::Null),
                ),
                ("length_mismatches", J::num(acc.payload_len_mismatches)),
            ]),
        ),
        (
            "arrival_ms",
            J::obj(vec![
                ("mean", J::num(interval_stats(&acc.interval_ms, 0.5))),
                ("p50", J::num(interval_stats(&acc.interval_ms, 0.5))),
                ("p99", J::num(interval_stats(&acc.interval_ms, 0.99))),
                ("max", J::num(interval_stats(&acc.interval_ms, 1.0))),
            ]),
        ),
        ("per_channel", J::arr(per_channel)),
        (
            "expected_tones_hz",
            J::arr(expect_tones.iter().map(|f| J::num(*f))),
        ),
        (
            "assertions",
            J::obj(vec![
                ("frames_ok", J::bool(frames_ok)),
                ("seq_ok", J::bool(seq_ok)),
                ("seq_unique_ok", J::bool(seq_unique_ok)),
                ("timestamp_ok", J::bool(ts_ok)),
                ("payload_ok", J::bool(payload_ok)),
                ("tone_ok", J::bool(tone_ok)),
                ("level_ok", J::bool(level_ok)),
                ("channel_map_ok", J::bool(channel_map_ok)),
            ]),
        ),
        (
            "rtp_first_packets_hex",
            J::arr(acc.first_packet_hex.iter().map(|s| J::str(s.clone()))),
        ),
        ("verdict", J::str(if verdict { "PASS" } else { "FAIL" })),
    ]);

    if let Some(path) = &stats_json {
        std::fs::write(path, json.to_json_pretty())
            .with_context(|| format!("failed to write stats JSON {:?}", path))?;
    }
    println!("{}", json.to_json_pretty());
    let exit_code = if packets_zero(&acc) || !verdict { 2 } else { 0 };
    eprintln!("recv: EXIT_CODE={exit_code}");
    Ok(exit_code)
}

fn packets_zero(acc: &RxAccum) -> bool {
    acc.packets == 0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}
fn db_amp(v: f32) -> f64 {
    if v <= 1e-10 {
        -200.0
    } else {
        20.0 * (v as f64).log10()
    }
}

/// Keep unused import warnings low: RTP header length is used for min-size checks.
const _: usize = RTP_HEADER_LEN;
