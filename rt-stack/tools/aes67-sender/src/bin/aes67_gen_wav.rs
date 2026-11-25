//! aes67-gen-wav — generate deterministic stereo/multichannel test WAV (+ raw L24)
//! for sender evidence and user testing.

use std::path::PathBuf;

use aes67_sender::cli::Args;
use aes67_sender::wav::{WavWriteSpec, WavWriter};

const USAGE: &str = r#"aes67-gen-wav — generate a deterministic test WAV (and optional raw L24)

USAGE:
  aes67-gen-wav --out tone.wav [options]

OPTIONS:
  --out <path>        output WAV path (required)
  --duration <s>      duration in seconds (default 3)
  --rate <hz>         sample rate (default 48000)
  --channels <n>      channels (default 2)
  --bits <16|24>      WAV bit depth (default 24)
  --float             write 32-bit float WAV instead of PCM (overrides --bits)
  --freq-l <hz>       channel 0 tone (default 1000)
  --freq-r <hz>       channel 1 tone (default 2000); other channels use freq-l
  --amp <0..1>        amplitude (default 0.5)
  --raw-out <path>    also write interleaved raw L24 little-endian PCM (no header)
  --help              this help
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
    let out = PathBuf::from(args.require("out")?);
    let duration = args.f64_or("duration", 3.0)?;
    anyhow::ensure!(duration > 0.0, "--duration must be > 0");
    let rate = args.u64_or("rate", 48_000)? as u32;
    let channels = args.usize_or("channels", 2)?;
    anyhow::ensure!((1..=8).contains(&channels), "--channels must be 1..=8");
    let bits = args.u16_or("bits", 24)?;
    anyhow::ensure!(
        bits == 16 || bits == 24 || bits == 32,
        "--bits must be 16/24/32"
    );
    let float = args.bool_flag("float");
    let freq_l = args.f64_or("freq-l", 1000.0)?;
    let freq_r = args.f64_or("freq-r", 2000.0)?;
    let amp = args.f64_or("amp", 0.5)?;
    anyhow::ensure!(amp > 0.0 && amp <= 1.0, "--amp must be in (0,1]");

    let frames = (duration * rate as f64).round() as usize;
    let mut samples = Vec::with_capacity(frames * channels);
    for f in 0..frames {
        let t = f as f64 / rate as f64;
        for c in 0..channels {
            let hz = match c {
                0 => freq_l,
                1 => freq_r,
                _ => freq_l,
            };
            let v = (2.0 * std::f64::consts::PI * hz * t).sin() * amp;
            samples.push(v as f32);
        }
    }
    let spec = WavWriteSpec {
        channels: channels as u16,
        sample_rate: rate,
        bits: if float { 32 } else { bits },
        float,
    };
    let mut w = WavWriter::new(spec)?;
    w.write_frames(&samples)?;
    let bytes = w.finish();
    std::fs::write(&out, &bytes)?;
    println!(
        "wrote {} ({} frames, {} ch, {} Hz, {} bits{}) size={}",
        out.display(),
        frames,
        channels,
        rate,
        spec.bits,
        if float { " float" } else { "" },
        bytes.len()
    );
    if let Some(raw) = args.opt_string("raw-out") {
        let mut raw_bytes = Vec::with_capacity(samples.len() * 3);
        let mut tmp = [0u8; 3];
        for s in &samples {
            let v = aes67_sender::pcm::f32_to_l24(*s);
            let u = (v & 0x00FF_FFFF) as u32;
            tmp[0] = u as u8; // raw file mode is little-endian
            tmp[1] = (u >> 8) as u8;
            tmp[2] = (u >> 16) as u8;
            raw_bytes.extend_from_slice(&tmp);
        }
        std::fs::write(&raw, &raw_bytes)?;
        println!("wrote raw L24LE {} ({} bytes)", raw, raw_bytes.len());
    }
    Ok(())
}
