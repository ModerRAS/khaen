//! CLI end-to-end tests: argument parsing + real execution against the fake
//! device over loopback TCP.

mod support;

use std::time::Duration;

use khaen_motu_cuemix::cli::{self, Command, EXIT_DEVICE, EXIT_OK, EXIT_USAGE};
use support::{FakeServer, ServerOptions};

fn run_cli(args: &[String], env_password: Option<&str>) -> (i32, String, String) {
    let command =
        cli::parse_args(args, env_password.map(|value| value.to_string())).expect("parse cli args");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = cli::execute(command, &mut out, &mut err);
    (
        code,
        String::from_utf8_lossy(&out).to_string(),
        String::from_utf8_lossy(&err).to_string(),
    )
}

fn base_args(command: &[&str], port: u16) -> Vec<String> {
    let mut args: Vec<String> = command.iter().map(|value| value.to_string()).collect();
    args.push("--host".to_string());
    args.push("127.0.0.1".to_string());
    args.push("--port".to_string());
    args.push(port.to_string());
    args.push("--connect-timeout".to_string());
    args.push("5".to_string());
    args.push("--sync-timeout".to_string());
    args.push("5".to_string());
    args.push("--readback-timeout".to_string());
    args.push("2".to_string());
    args
}

fn write_cards_file() -> std::path::PathBuf {
    let path = support::unique_temp_path("khaen-motu-cards");
    std::fs::write(&path, support::N150_CARDS).expect("write cards fixture");
    path
}

#[test]
fn info_reports_detection_and_state() {
    let server = FakeServer::start(ServerOptions::default());
    let cards = write_cards_file();
    let mut args = base_args(&["info"], server.addr.port());
    args.push("--cards".to_string());
    args.push(cards.display().to_string());
    let (code, out, err) = run_cli(&args, None);
    let _ = std::fs::remove_file(&cards);
    server.shutdown();
    assert_eq!(code, EXIT_OK, "stderr: {}", err);
    assert!(out.contains("detection: detected=true"), "out: {}", out);
    assert!(out.contains("card_id=UltraLitemk5"), "out: {}", out);
    assert!(out.contains("model=UltraLite-mk5"), "out: {}", out);
    assert!(out.contains("control_reachable=true"), "out: {}", out);
    assert!(out.contains("state: mute=off(0)"), "out: {}", out);
    assert!(out.contains("sample_rate_hz=48000"), "out: {}", out);
    assert!(
        out.contains("main_fader=1.000000 (0.00 dB)"),
        "out: {}",
        out
    );
    assert!(out.contains("auth: authenticated=true"), "out: {}", out);
}

#[test]
fn info_json_is_machine_readable() {
    let server = FakeServer::start(ServerOptions::default());
    let cards = write_cards_file();
    let mut args = base_args(&["info"], server.addr.port());
    args.push("--cards".to_string());
    args.push(cards.display().to_string());
    args.push("--json".to_string());
    let (code, out, err) = run_cli(&args, None);
    let _ = std::fs::remove_file(&cards);
    server.shutdown();
    assert_eq!(code, EXIT_OK, "stderr: {}", err);
    assert!(out.contains("\"detected\":true"), "out: {}", out);
    assert!(out.contains("\"card_id\":\"UltraLitemk5\""), "out: {}", out);
    assert!(out.contains("\"name\":\"UltraLite-mk5\""), "out: {}", out);
    assert!(out.contains("\"mute\":false"), "out: {}", out);
    assert!(out.contains("\"sample_rate_hz\":48000"), "out: {}", out);
}

#[test]
fn mute_on_writes_one_key_and_prints_readback() {
    let server = FakeServer::start(ServerOptions::default());
    let args = base_args(&["mute", "on"], server.addr.port());
    let (code, out, err) = run_cli(&args, None);
    let messages = server.messages();
    let log = server.shutdown();
    assert_eq!(code, EXIT_OK, "stderr: {}", err);
    assert!(
        out.contains("mute: before=off(0) wrote=on readback=on [verified]"),
        "out: {}",
        out
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, support::MUTE_ENABLE_ID);
    assert_eq!(messages[0].index, 0);
    assert_eq!(messages[0].value, vec![1]);
    assert_eq!(log.multi_message_frames, 0);
}

#[test]
fn volume_db_writes_two_channels_and_prints_readbacks() {
    let server = FakeServer::start(ServerOptions::default());
    let args = base_args(&["volume", "-6dB"], server.addr.port());
    let (code, out, err) = run_cli(&args, None);
    let messages = server.messages();
    server.shutdown();
    assert_eq!(code, EXIT_OK, "stderr: {}", err);
    assert!(out.contains("volume: target -6.00 dB"), "out: {}", out);
    assert!(
        out.contains("koBusFader[0] (Main 1 (left))"),
        "out: {}",
        out
    );
    assert!(
        out.contains("koBusFader[1] (Main 2 (right))"),
        "out: {}",
        out
    );
    assert_eq!(out.matches("[verified]").count(), 2, "out: {}", out);
    assert_eq!(messages.len(), 2);
    let raw = i32::from_be_bytes([
        messages[0].value[0],
        messages[0].value[1],
        messages[0].value[2],
        messages[0].value[3],
    ]);
    let linear = raw as f64 / (1 << 24) as f64;
    assert!((linear - 0.501_187_2).abs() < 1e-5, "linear = {}", linear);
}

#[test]
fn save_writes_a_read_only_snapshot_file() {
    let server = FakeServer::start(ServerOptions::default());
    let path = support::unique_temp_path("khaen-motu-cli-snapshot");
    let mut args = base_args(
        &["save", path.to_str().expect("utf8 path")],
        server.addr.port(),
    );
    args.push("--drain".to_string());
    args.push("1".to_string());
    let (code, out, err) = run_cli(&args, None);
    let messages = server.messages();
    server.shutdown();
    assert_eq!(code, EXIT_OK, "stderr: {}", err);
    assert!(out.contains("save:"), "out: {}", out);
    let text = std::fs::read_to_string(&path).expect("snapshot file");
    let _ = std::fs::remove_file(&path);
    assert!(
        text.contains("\"values\""),
        "snapshot: {}",
        &text[..200.min(text.len())]
    );
    assert!(text.contains("\"kMuteEnable\": [0]"));
    assert!(messages.is_empty(), "save must be read-only");
}

#[test]
fn volume_out_of_range_is_rejected() {
    // The CLI parser refuses an out-of-range volume before any I/O happens.
    let error = cli::parse_args(&["volume".to_string(), "1.5".to_string()], None)
        .expect_err("out of range volume");
    assert!(
        error.to_string().contains("outside 0.0..=1.0"),
        "error: {}",
        error
    );

    // The executor also refuses a manually constructed out-of-range target.
    let mut out = Vec::new();
    let mut err = Vec::new();
    let command = Command::Volume {
        target: khaen_motu_cuemix::cli::VolumeTarget::Linear(1.5),
        options: khaen_motu_cuemix::cli::Options::default(),
    };
    assert_eq!(cli::execute(command, &mut out, &mut err), EXIT_USAGE);
    assert!(out.is_empty());
    assert!(
        String::from_utf8_lossy(&err).contains("outside 0.0..=1.0"),
        "err: {}",
        String::from_utf8_lossy(&err)
    );
}

#[test]
fn unknown_command_is_a_usage_error() {
    let error = cli::parse_args(&["bogus".to_string()], None).expect_err("unknown command");
    assert!(error.to_string().contains("unknown command"));
    let error = cli::parse_args(&["mute".to_string()], None).expect_err("missing value");
    assert!(error.to_string().contains("requires"));
}

#[test]
fn help_exits_zero() {
    let (code, out, _err) = run_cli(&["--help".to_string()], None);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("USAGE"));
    assert!(out.contains("EXIT CODES"));
}

#[test]
fn unreachable_control_network_exits_three() {
    // Reserve a port then drop the listener so the probe is refused.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let mut args = base_args(&["mute", "on"], port);
    args.push("--connect-timeout".to_string());
    args.push("0.2".to_string());
    let (code, out, err) = run_cli(&args, None);
    assert_eq!(code, EXIT_DEVICE);
    assert!(out.is_empty());
    assert!(err.contains("unreachable"), "err: {}", err);
}

#[test]
fn password_from_environment_is_used() {
    let options = ServerOptions {
        auth: true,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let cards = write_cards_file();
    let mut args = base_args(&["info"], server.addr.port());
    args.push("--cards".to_string());
    args.push(cards.display().to_string());
    let (code, out, err) = run_cli(&args, Some("khaen-test-password"));
    let _ = std::fs::remove_file(&cards);
    let log = server.shutdown();
    assert_eq!(code, EXIT_OK, "stderr: {}", err);
    assert!(out.contains("password_required=true"), "out: {}", out);
    assert!(out.contains("auth: authenticated=true"), "out: {}", out);
    assert_eq!(log.auth_cipher_len, Some(256));
}

#[test]
fn command_enum_matches_parsed_args() {
    let command = cli::parse_args(
        &[
            "mute".to_string(),
            "off".to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
        ],
        None,
    )
    .expect("parse");
    assert!(matches!(command, Command::Mute { on: false, .. }));
    let _ = Duration::from_secs(1);
}
