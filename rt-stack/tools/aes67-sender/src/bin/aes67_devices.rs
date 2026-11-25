//! aes67-devices — list WASAPI render endpoints (loopback capture targets) and
//! their mix formats. No audio is played, no device state is modified.

use aes67_sender::cli::Args;

const USAGE: &str = r#"aes67-devices — list Windows WASAPI render endpoints (loopback targets)

USAGE:
  aes67-devices [--formats] [--help]

OPTIONS:
  --formats   also query each endpoint's mix format (sample rate / channels / sample type)
  --help      this help

Notes:
  * WASAPI system loopback captures a RENDER endpoint; select it with
    aes67-send --source wasapi-loopback --wasapi-device "<name substring|#index>".
  * Process loopback takes a PID instead: --source wasapi-process --pid <n>.
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
    let eps = aes67_sender::wasapi::list_render_endpoints()?;
    println!("render endpoints (WASAPI loopback capture targets):");
    for e in &eps {
        println!(
            "#{:<2} {}{}",
            e.index,
            e.name,
            if e.is_default { "  [default]" } else { "" }
        );
        println!("    id={}", e.id);
    }
    if args.bool_flag("formats") {
        for e in &eps {
            match aes67_sender::wasapi::query_endpoint_format(&e.id) {
                Ok(desc) => println!("    format(#{}): {}", e.index, desc),
                Err(err) => println!("    format(#{}): <error: {err}>", e.index),
            }
        }
    }
    Ok(())
}
