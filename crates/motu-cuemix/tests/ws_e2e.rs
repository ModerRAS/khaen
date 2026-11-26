//! End-to-end tests: real loopback TCP, real WebSocket handshake/framing, and
//! an independent fake CUE device (tests/support/mod.rs).

mod support;

use std::net::SocketAddr;
use std::time::Duration;

use khaen_motu_cuemix::{codes, ClientConfig, CueMixClient, MainChannel};
use support::{EchoMode, FakeServer, HostMessage, ServerOptions};

fn client_config(addr: SocketAddr) -> ClientConfig {
    let mut config = ClientConfig::new(addr);
    config.connect_timeout = Duration::from_secs(5);
    config.sync_timeout = Duration::from_secs(5);
    config.readback_timeout = Duration::from_secs(2);
    config.idle_drain = Duration::from_millis(20);
    config.snapshot_drain = Duration::from_millis(50);
    config
}

#[test]
fn connects_reads_state_and_sets_mute() {
    let options = ServerOptions {
        ping_first: true,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");

    let state = client.read_state().expect("read state");
    assert_eq!(state.mute, Some(false));
    assert_eq!(state.sample_rate_hz, Some(48_000));
    assert_eq!(state.main_fader, Some(1.0));
    assert_eq!(state.main_trim, Some(31));
    assert_eq!(state.ko_bus_mute, Some(false));
    assert!(state.is_complete());
    let info = client.device_info();
    assert_eq!(info.name.as_deref(), Some("UltraLite-mk5"));
    assert_eq!(info.model_id.as_deref(), Some("UL5L"));

    let report = client.set_mute(true).expect("set mute");
    assert!(report.frame_sent);
    assert!(report.changed);
    assert!(report.readback);
    assert_eq!(client.state_snapshot().mute, Some(true));

    let messages = server.messages();
    assert_eq!(
        messages,
        vec![HostMessage {
            id: support::MUTE_ENABLE_ID,
            index: 0,
            value: vec![1],
        }]
    );
    drop(client);
    let log = server.shutdown();
    assert!(log.handshake_ok);
    assert!(log.pong_received, "client must answer the device ping");
    assert_eq!(
        log.multi_message_frames, 0,
        "one datastore message per frame"
    );
}

#[test]
fn set_mute_readback_mismatch_is_reported() {
    let options = ServerOptions {
        echo: EchoMode::Wrong,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");
    let error = client.set_mute(true).expect_err("mismatch");
    assert_eq!(error.code(), codes::READBACK_MISMATCH);
    drop(client);
    server.shutdown();
}

#[test]
fn set_mute_readback_timeout_is_reported() {
    let options = ServerOptions {
        echo: EchoMode::Silent,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");
    let error = client.set_mute(true).expect_err("timeout");
    assert_eq!(error.code(), codes::READBACK_TIMEOUT);
    drop(client);
    server.shutdown();
}

#[test]
fn authenticates_with_device_password() {
    let options = ServerOptions {
        auth: true,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let mut config = client_config(server.addr);
    config.password = Some("khaen-test-password".to_string());
    let mut client = CueMixClient::connect(config).expect("connect with password");
    assert_eq!(client.password_required(), Some(true));
    assert!(client.stats().auth_response_sent);
    assert!(client.stats().authenticated);
    let state = client.read_state().expect("read state");
    assert!(state.is_complete());
    drop(client);
    let log = server.shutdown();
    assert_eq!(log.auth_cipher_len, Some(256), "2048-bit OAEP ciphertext");
}

#[test]
fn password_required_without_password_fails_closed() {
    let options = ServerOptions {
        auth: true,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let error = CueMixClient::connect(client_config(server.addr)).expect_err("auth required");
    assert_eq!(error.code(), codes::AUTH_REQUIRED);
    let log = server.shutdown();
    assert!(log.auth_cipher_len.is_none());
}

#[test]
fn bad_handshake_accept_is_rejected() {
    let options = ServerOptions {
        bad_accept: true,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let error = CueMixClient::connect(client_config(server.addr)).expect_err("bad accept");
    assert_eq!(error.code(), codes::HANDSHAKE);
    server.shutdown();
}

#[test]
fn ignores_text_frames_and_reassembles_fragmented_state() {
    let options = ServerOptions {
        text_before_state: true,
        fragment_state: true,
        ..ServerOptions::default()
    };
    let server = FakeServer::start(options);
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");
    let state = client.read_state().expect("read state");
    assert!(state.is_complete());
    assert_eq!(state.sample_rate_hz, Some(48_000));
    let info = client.device_info();
    assert_eq!(info.name.as_deref(), Some("UltraLite-mk5"));
    drop(client);
    server.shutdown();
}

#[test]
fn volume_writes_two_single_message_frames() {
    let server = FakeServer::start(ServerOptions::default());
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");
    let report = client.set_volume(0.5).expect("set volume");
    assert_eq!(report.left.index, 0);
    assert_eq!(report.right.index, 1);
    assert_eq!(client.state_snapshot().main_fader, Some(0.5));
    let messages = server.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id, support::KO_BUS_FADER_ID);
    assert_eq!(messages[0].index, 0);
    assert_eq!(messages[1].index, 1);
    for message in &messages {
        assert_eq!(message.value.len(), 4, "8.24 fader value");
        let raw = i32::from_be_bytes([
            message.value[0],
            message.value[1],
            message.value[2],
            message.value[3],
        ]);
        let linear = raw as f64 / (1 << 24) as f64;
        assert!((linear - 0.5).abs() < 1e-6, "written = {}", linear);
    }
    drop(client);
    let log = server.shutdown();
    assert_eq!(log.multi_message_frames, 0);
}

#[test]
fn volume_channel_right_writes_only_index_one() {
    let server = FakeServer::start(ServerOptions::default());
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");
    let report = client
        .set_volume_channel(MainChannel::Right, 0.25)
        .expect("right");
    assert_eq!(report.index, 1);
    let messages = server.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].index, 1);
    drop(client);
    server.shutdown();
}

#[test]
fn save_snapshot_over_tcp_is_read_only_json() {
    let server = FakeServer::start(ServerOptions::default());
    let mut client = CueMixClient::connect(client_config(server.addr)).expect("connect");
    let path = support::unique_temp_path("khaen-motu-ws-snapshot");
    let report = client.save_snapshot(&path).expect("save snapshot");
    let text = std::fs::read_to_string(&path).expect("read snapshot");
    let _ = std::fs::remove_file(&path);
    assert!(report.datastore_values >= 10);
    assert!(text.contains("\"kMuteEnable\": [0]"));
    assert!(text.contains("\"koBusFader\": [1.0,1.0]"));
    assert!(text.contains("\"model\": \"UL5L\""));
    assert!(text.contains("\"authenticated\": true"));
    let messages = server.messages();
    assert!(messages.is_empty(), "snapshot must not write to the device");
    drop(client);
    server.shutdown();
}
