//! aes67-send — Windows user-space AES67 sender (WASAPI loopback / WAV / raw / sine).
//!
//! See README.md for the full flag list and evidence/aes67-tx-win/CLOCK-AND-SYNC.md
//! for the non-PTP clock caveat.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;

use aes67_sender::cli::Args;
use aes67_sender::json::J;
use aes67_sender::pacer::{parse_iface, run_sender, SenderConfig};
use aes67_sender::sdp::SdpConfig;
use aes67_sender::source::{RawFileSource, RawFormat, SineSource, Source, WavFileSource};
use aes67_sender::{DEFAULT_MCAST, DEFAULT_PAYLOAD_TYPE, DEFAULT_PORT, DEFAULT_PTIME_MS};

const USAGE: &str = r#"aes67-send — Windows user-space AES67 L24/48k RTP sender (task-236)

USAGE:
  aes67-send --source <sine|wav|raw|wasapi-loopback|wasapi-process> [options]

SOURCE OPTIONS:
  --source sine                 synthetic tone (default)
  --freq-l <hz>                 left/first-channel tone (default 1000)
  --freq-r <hz>                 right/second-channel tone (default 2000)
  --amp <0..1>                  tone amplitude (default 0.5)
  --wav <path>                  WAV input (any PCM8/16/24/32 or f32/f64; resampled to 48k)
  --raw <path>                  raw interleaved PCM input (48 kHz; see --raw-format)
  --raw-format <l16|l24|l32|f32>  raw sample format, little-endian file (default l24)
  --channels <n>                channels for sine/raw (1..=8; WAV uses its own)
  --loop-file                   loop WAV/raw input instead of stopping at EOF
  --wasapi-device <selector>    system loopback: default endpoint or #index / name substring / endpoint id
  --pid <n>                     process loopback target PID (Win10 2004+)

NETWORK / RTP:
  --dest <ip>                   destination IP (default 239.69.63.10; non-224.x => unicast)
  --port <n>                    UDP port (default 5004)
  --iface-ip <ipv4>             bind/multicast interface (e.g. 192.0.2.94)
  --ttl <n>                     multicast TTL (default 1; 0 = host-local, safest for self-test)
  --no-mcast-loop               disable IP_MULTICAST_LOOP
  --ptime <ms>                  packet time (default 1 ms => 48 frames @48k)
  --pt <n>                      RTP payload type (default 97)
  --ssrc <u32|0x...>            SSRC (default 0x54535049 "TSPI")
  --seq <u16>                   initial RTP sequence number (default 0)
  --ts <u32>                    initial RTP timestamp (default 0)
  --ts-refclk <value>           SDP a=ts-refclk (default "local"; non-PTP first stage)

RUN CONTROL / OUTPUT:
  --duration <s>                send duration (required for sine/wasapi unless --infinite)
  --infinite                    keep sending (Ctrl-C to stop)
  --warmup-ms <ms>              prebuffer before first packet (default 50)
  --warmup-timeout-ms <ms>      max wait for first live capture data (default 2000)
  --sdp-out <path>              write SDP to file
  --print-sdp                   print SDP to stdout
  --stats-json <path>           write sender statistics JSON
  --list-devices                list render endpoints (WASAPI loopback targets) and exit
  --quiet                       less stderr progress
  --help                        this help

EXAMPLES:
  aes67-send --source sine --duration 3 --dest 127.0.0.1 --port 5504 \
      --sdp-out ev/sdp/unicast.sdp --stats-json ev/sender.json
  aes67-send --source wasapi-loopback --wasapi-device "CABLE Input" --duration 5 \
      --dest 239.69.63.10 --iface-ip 192.0.2.94 --ttl 0 --sdp-out ev/sdp/mcast.sdp
  aes67-send --source wasapi-process --pid 1234 --duration 5 --dest 127.0.0.1 --port 5506
"#;

fn guess_local_ip(dest: Ipv4Addr) -> Option<Ipv4Addr> {
    let bind: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
    let sock = UdpSocket::bind(bind).ok()?;
    let target = if dest.is_multicast() {
        SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900)
    } else {
        SocketAddrV4::new(dest, 9)
    };
    sock.connect(target).ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> anyhow::Result<()> {
    let args = Args::from_env()?;
    if args.help {
        print!("{USAGE}");
        return Ok(());
    }
    // WASAPI/COM init is cheap and harmless for file/sine modes; needed before
    // any endpoint enumeration or capture.
    #[cfg(windows)]
    let _com = aes67_sender::wasapi::ComGuard::init_mta()?;
    if args.bool_flag("list-devices") {
        let eps = aes67_sender::wasapi::list_render_endpoints()?;
        println!("render endpoints (WASAPI loopback capture targets):");
        println!("{}", aes67_sender::wasapi::format_endpoints(&eps));
        return Ok(());
    }

    let quiet = args.bool_flag("quiet");
    let channels_arg = args.usize_or("channels", 2)?;
    let duration = args.f64_opt("duration")?;
    let infinite = args.bool_flag("infinite");
    let source_kind = args.string("source", "sine");

    // Build source (+ capture stats handle for WASAPI sources).
    let mut capture_stats: Option<std::sync::Arc<aes67_sender::wasapi::CaptureStats>> = None;
    let mut capture_desc: Option<String> = None;
    let mut source: Box<dyn Source> = match source_kind.as_str() {
        "sine" => {
            if duration.is_none() && !infinite {
                anyhow::bail!("--source sine requires --duration <s> (or --infinite)");
            }
            let freq_l = args.f64_or("freq-l", 1000.0)?;
            let freq_r = args.f64_or("freq-r", 2000.0)?;
            let amp = args.f64_or("amp", 0.5)?;
            Box::new(SineSource::new(
                channels_arg,
                freq_l,
                freq_r,
                amp,
                duration,
            )?)
        }
        "wav" => {
            let path = PathBuf::from(args.require("wav")?);
            Box::new(WavFileSource::open(&path, args.bool_flag("loop-file"))?)
        }
        "raw" => {
            let path = PathBuf::from(args.require("raw")?);
            let fmt = RawFormat::parse(&args.string("raw-format", "l24"))?;
            let rate = args.u64_or("raw-rate", 48_000)?;
            anyhow::ensure!(
                rate == 48_000,
                "--raw-rate {rate} unsupported: raw mode is 48 kHz only (use --source wav for SRC)"
            );
            let ch = args.usize_or("raw-channels", channels_arg)?;
            Box::new(RawFileSource::open(
                &path,
                ch,
                fmt,
                args.bool_flag("loop-file"),
            )?)
        }
        "wasapi-loopback" => {
            #[cfg(windows)]
            {
                use aes67_sender::wasapi::WasapiCaptureSource;
                let selector = args.opt_string("wasapi-device");
                let src = WasapiCaptureSource::system_loopback(selector.as_deref())?;
                capture_stats = Some(src.stats_arc());
                capture_desc = Some(src.capture_format_desc());
                Box::new(src)
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("--source wasapi-loopback requires Windows")
            }
        }
        "wasapi-process" => {
            #[cfg(windows)]
            {
                use aes67_sender::wasapi::WasapiCaptureSource;
                let pid = args
                    .u64_opt("pid")?
                    .ok_or_else(|| anyhow::anyhow!("--source wasapi-process requires --pid <n>"))?
                    as u32;
                let src = WasapiCaptureSource::process_loopback(pid)?;
                capture_stats = Some(src.stats_arc());
                capture_desc = Some(src.capture_format_desc());
                Box::new(src)
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("--source wasapi-process requires Windows")
            }
        }
        other => anyhow::bail!(
            "unknown --source {other:?} (sine|wav|raw|wasapi-loopback|wasapi-process)"
        ),
    };

    let channels = source.channels();
    anyhow::ensure!(
        (1..=8).contains(&channels),
        "source has {channels} channels; AES67 flow supports 1..=8"
    );

    // Destination classification: non-multicast => unicast.
    let dest_ip: Ipv4Addr = args.string("dest", DEFAULT_MCAST).parse()?;
    let multicast = dest_ip.is_multicast();
    let port = args.u16_or("port", DEFAULT_PORT)?;
    let ptime_ms = args.f64_or("ptime", DEFAULT_PTIME_MS)?;
    let iface_ip = match args.opt_string("iface-ip") {
        Some(s) => Some(parse_iface(&s)?),
        None => None,
    };

    // SDP (written before sending; the receiver needs it out-of-band).
    let origin_ip = iface_ip
        .or_else(|| guess_local_ip(dest_ip))
        .unwrap_or(Ipv4Addr::UNSPECIFIED);
    let sdp = SdpConfig {
        session_name: args.string("session-name", "TSPI Windows AES67 sender"),
        origin_ip: origin_ip.to_string(),
        dest_ip: dest_ip.to_string(),
        multicast,
        port,
        payload_type: args.u16_or("pt", DEFAULT_PAYLOAD_TYPE as u16)? as u8,
        channels: channels as u8,
        ptime_ms,
        ts_refclk: args.string("ts-refclk", "local"),
        ssrc: args.u64_or("ssrc", 0x5453_5049)? as u32,
    }
    .render()?;
    if let Some(path) = args.opt_string("sdp-out") {
        std::fs::write(&path, &sdp)
            .map_err(|e| anyhow::anyhow!("failed to write SDP {path:?}: {e}"))?;
        if !quiet {
            eprintln!("sdp: wrote {path}");
        }
    }
    if args.bool_flag("print-sdp") {
        print!("{sdp}");
    } else if args.opt_string("sdp-out").is_none() && !quiet {
        eprintln!("sdp:\n{sdp}");
    }

    let cfg = SenderConfig {
        dest: SocketAddrV4::new(dest_ip, port),
        multicast,
        iface_ip,
        ttl: args.u16_or("ttl", 1)? as u32,
        mcast_loop: !args.bool_flag("no-mcast-loop"),
        ptime_ms,
        duration_s: duration,
        payload_type: sdp_pt(&args)?,
        ssrc: args.u64_or("ssrc", 0x5453_5049)? as u32,
        initial_seq: args.u16_or("seq", 0)?,
        initial_ts: args.u64_or("ts", 0)? as u32,
        warmup_ms: args.f64_or("warmup-ms", 50.0)?,
        warmup_timeout_ms: args.f64_or("warmup-timeout-ms", 2000.0)?,
        verbose: !quiet,
    };

    let run = run_sender(&cfg, source.as_mut())?;
    let s = &run.stats;
    if !quiet {
        eprintln!(
            "sender done: packets={} frames={} bytes={} underruns={} late_p50={}us late_p99={}us late_max={}us errors={} runtime={:.3}s rate={:.1}pps",
            s.packets_sent,
            s.frames_sent,
            s.datagram_bytes,
            s.underrun_packets,
            s.late.percentile_ns(0.5) / 1000,
            s.late.percentile_ns(0.99) / 1000,
            s.late.max_ns / 1000,
            s.send_errors,
            s.run_time_s,
            s.effective_packet_rate_hz
        );
        for (i, hex) in run.first_packets_hex.iter().enumerate() {
            eprintln!("rtp[{i}]: {hex}");
        }
    }
    if let Some(stats) = capture_stats {
        let snap = stats.snapshot();
        if !quiet {
            eprintln!(
                "wasapi capture: packets={} frames_in={} frames_out={} discontinuity={} silent={} overflow_drops={} read_errors={}",
                snap.packets,
                snap.frames_in,
                snap.frames_out,
                snap.discontinuity,
                snap.silent_packets,
                snap.overflow_drops,
                snap.read_errors
            );
        }
        if let Some(path) = args.opt_string("stats-json") {
            let js = sender_json(
                s,
                capture_desc.as_deref(),
                Some(&snap),
                &run.first_packets_hex,
            );
            std::fs::write(&path, js.to_json_pretty())
                .map_err(|e| anyhow::anyhow!("failed to write stats JSON {path:?}: {e}"))?;
        }
    } else if let Some(path) = args.opt_string("stats-json") {
        let js = sender_json(s, capture_desc.as_deref(), None, &run.first_packets_hex);
        std::fs::write(&path, js.to_json_pretty())
            .map_err(|e| anyhow::anyhow!("failed to write stats JSON {path:?}: {e}"))?;
    }
    Ok(())
}

fn sdp_pt(args: &Args) -> anyhow::Result<u8> {
    let pt = args.u16_or("pt", DEFAULT_PAYLOAD_TYPE as u16)?;
    anyhow::ensure!(pt <= 127, "RTP payload type must be 0..=127");
    Ok(pt as u8)
}

fn sender_json(
    s: &aes67_sender::pacer::SenderStats,
    capture_desc: Option<&str>,
    capture: Option<&aes67_sender::wasapi::CaptureSnapshot>,
    first_packets_hex: &[String],
) -> J {
    let mut pairs: Vec<(String, J)> = vec![
        ("tool".into(), J::str("aes67-send")),
        ("version".into(), J::str(env!("CARGO_PKG_VERSION"))),
        ("dest".into(), J::str(s.dest.clone())),
        ("multicast".into(), J::bool(s.multicast)),
        ("iface".into(), J::str(s.iface.clone())),
        ("ttl".into(), J::num(s.ttl)),
        ("channels".into(), J::num(s.channels as u64)),
        ("payload_type".into(), J::num(s.payload_type as u64)),
        ("ptime_ms".into(), J::num(s.ptime_ms)),
        (
            "samples_per_packet".into(),
            J::num(s.samples_per_packet as u64),
        ),
        ("ssrc".into(), J::num(s.ssrc as u64)),
        ("first_seq".into(), J::num(s.first_seq as u64)),
        ("last_seq".into(), J::num(s.last_seq as u64)),
        ("first_ts".into(), J::num(s.first_ts as u64)),
        ("last_ts".into(), J::num(s.last_ts as u64)),
        ("packets_sent".into(), J::num(s.packets_sent)),
        ("frames_sent".into(), J::num(s.frames_sent)),
        ("payload_bytes".into(), J::num(s.payload_bytes)),
        ("datagram_bytes".into(), J::num(s.datagram_bytes)),
        ("send_errors".into(), J::num(s.send_errors)),
        ("underrun_packets".into(), J::num(s.underrun_packets)),
        ("underrun_frames".into(), J::num(s.underrun_frames)),
        ("eof_early".into(), J::bool(s.eof_early)),
        ("warmup_frames".into(), J::num(s.warmup_frames)),
        ("run_time_s".into(), J::num(s.run_time_s)),
        (
            "effective_packet_rate_hz".into(),
            J::num(s.effective_packet_rate_hz),
        ),
        (
            "effective_bitrate_kbps".into(),
            J::num(s.effective_bitrate_kbps),
        ),
        ("clock_source".into(), J::str(s.clock_source.clone())),
        (
            "deadline".into(),
            J::obj(vec![
                ("count", J::num(s.late.count)),
                ("late_over_50us", J::num(s.late.late_count)),
                ("miss_over_period", J::num(s.late.miss_count)),
                ("mean_us", J::num(s.late.mean_ns() as f64 / 1000.0)),
                ("p50_us", J::num(s.late.percentile_ns(0.5) as f64 / 1000.0)),
                ("p99_us", J::num(s.late.percentile_ns(0.99) as f64 / 1000.0)),
                ("max_us", J::num(s.late.max_ns as f64 / 1000.0)),
            ]),
        ),
        (
            "rtp_first_packets_hex".into(),
            J::arr(first_packets_hex.iter().map(|h| J::str(h.clone()))),
        ),
    ];
    if let Some(desc) = capture_desc {
        pairs.push(("wasapi_capture_format".into(), J::str(desc)));
    }
    if let Some(c) = capture {
        pairs.push((
            "wasapi_capture".into(),
            J::obj(vec![
                ("packets", J::num(c.packets)),
                ("frames_in", J::num(c.frames_in)),
                ("frames_out", J::num(c.frames_out)),
                ("discontinuity", J::num(c.discontinuity)),
                ("silent_packets", J::num(c.silent_packets)),
                ("timestamp_errors", J::num(c.timestamp_errors)),
                ("overflow_drops", J::num(c.overflow_drops)),
                ("read_errors", J::num(c.read_errors)),
            ]),
        ));
    }
    J::obj(pairs)
}
