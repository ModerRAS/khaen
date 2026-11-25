//! aes67-play — render a WAV / generated tone to a WASAPI render endpoint.
//!
//! Used to drive the cross-process WASAPI loopback evidence without audible
//! output: render into a virtual endpoint (e.g. VB-Audio "CABLE Input" or
//! "Steam Streaming Speakers"), then let aes67-send capture it via loopback.

use std::path::PathBuf;

use aes67_sender::cli::Args;
use aes67_sender::wav::WavReader;

const USAGE: &str = r#"aes67-play — render WAV or a generated tone to a WASAPI render endpoint

USAGE:
  aes67-play --wav <file> [--device <selector>] [options]
  aes67-play --gen-tone [--freq-l 1000 --freq-r 2000] [options]

OPTIONS:
  --device <selector>   render endpoint: default, #index, endpoint id, or name substring
                        (e.g. "CABLE Input" / "Steam Streaming Speakers")
  --wav <path>          WAV file to render (PCM 8/16/24/32, f32/f64; resampled if needed)
  --gen-tone            generate a stereo tone instead of reading a WAV
  --freq-l <hz>         generated channel-0 tone (default 1000)
  --freq-r <hz>         generated channel-1 tone (default 2000)
  --amp <0..1>          generated tone amplitude (default 0.5)
  --rate <hz>           generated tone sample rate (default 48000)
  --channels <n>        generated tone channels (default 2)
  --duration <s>        render duration limit (default: WAV length or 3 s for generated tone)
  --gain <x>            linear gain applied while rendering (default 1.0)
  --loop                loop the source until --duration/stop
  --list-devices        list render endpoints and exit
  --help                this help

No ASIO driver is involved; WASAPI shared mode only.
"#;

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
    #[cfg(windows)]
    let _com = aes67_sender::wasapi::ComGuard::init_mta()?;
    if args.bool_flag("list-devices") {
        let eps = aes67_sender::wasapi::list_render_endpoints()?;
        println!("render endpoints:");
        println!("{}", aes67_sender::wasapi::format_endpoints(&eps));
        return Ok(());
    }
    let device = args.opt_string("device");
    let gain = args.f64_or("gain", 1.0)? as f32;
    let loop_audio = args.bool_flag("loop");
    let duration_limit = args.f64_opt("duration")?;

    let (samples, channels, rate, default_duration) = if let Some(wav) = args.opt_string("wav") {
        let path = PathBuf::from(wav);
        let mut r = WavReader::open(&path)?;
        let ch = r.spec.channels as usize;
        let rate = r.spec.sample_rate;
        let total = r.total_frames() as usize;
        let mut samples = Vec::with_capacity(total * ch);
        loop {
            let (chunk, eof) = r.read_frames(4096)?;
            samples.extend_from_slice(&chunk);
            if eof {
                break;
            }
        }
        let dur = samples.len() as f64 / (ch as f64 * rate as f64);
        eprintln!(
            "play: WAV {:?} -> {} frames {} ch {} Hz ({:.3}s)",
            path, total, ch, rate, dur
        );
        (samples, ch, rate, Some(dur))
    } else {
        let ch = args.usize_or("channels", 2)?;
        let rate = args.u64_or("rate", 48_000)? as u32;
        let freq_l = args.f64_or("freq-l", 1000.0)?;
        let freq_r = args.f64_or("freq-r", 2000.0)?;
        let amp = args.f64_or("amp", 0.5)?;
        let dur = duration_limit.unwrap_or(3.0);
        let frames = (dur * rate as f64).round() as usize;
        let mut samples = Vec::with_capacity(frames * ch);
        for f in 0..frames {
            let t = f as f64 / rate as f64;
            for c in 0..ch {
                let hz = match c {
                    0 => freq_l,
                    1 => freq_r,
                    _ => freq_l,
                };
                samples.push((2.0 * std::f64::consts::PI * hz * t).sin() as f32 * amp as f32);
            }
        }
        (samples, ch, rate, Some(dur))
    };

    let effective_duration = duration_limit.or(default_duration);
    let stats = aes67_sender::wasapi::play_interleaved(
        device.as_deref(),
        &samples,
        channels,
        rate,
        gain,
        loop_audio,
        effective_duration,
    )?;
    println!(
        "play done: endpoint={:?} format={} frames_written={} buffer={} frames",
        stats.endpoint_name, stats.format_desc, stats.frames_written, stats.buffer_frames
    );
    Ok(())
}
