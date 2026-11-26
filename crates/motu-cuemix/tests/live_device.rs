//! Real-device integration tests. Ignored by default: they require the N150
//! host with the MOTU UltraLite-mk5 attached (no device on a dev machine).
//!
//! Run on N150 (read only):
//!   cargo test -p khaen-motu-cuemix --offline --test live_device -- --ignored --nocapture
//!
//! Write verification (lead-controlled, changes mute twice and restores it):
//!   KHAEN_MOTU_LIVE_WRITE=1 cargo test ... -- --ignored --nocapture

use std::time::Duration;

use khaen_motu_cuemix::{detect, ClientConfig, CueMixClient, DetectConfig};

#[test]
#[ignore = "requires N150 + MOTU UltraLite-mk5"]
fn live_read_state() {
    let detection = detect(&DetectConfig::default());
    println!("detection: {:?}", detection);
    if !detection.is_ready() {
        println!("device not ready ({}); nothing to read", detection.reason);
        return;
    }
    let config = ClientConfig {
        connect_timeout: Duration::from_secs(3),
        sync_timeout: Duration::from_secs(5),
        ..ClientConfig::default()
    };
    let mut client = CueMixClient::connect(config).expect("connect to real device");
    let state = client.read_state().expect("read state");
    println!("state: {:?}", state);
    println!("device: {:?}", client.device_info());
    println!("stats: {:?}", client.stats());
    println!("transport: {}", client.transport_description());
    assert!(
        state.is_complete(),
        "live state fields missing: {:?}",
        state.missing()
    );
}

#[test]
#[ignore = "requires N150 + MOTU UltraLite-mk5 and KHAEN_MOTU_LIVE_WRITE=1"]
fn live_mute_roundtrip() {
    if std::env::var("KHAEN_MOTU_LIVE_WRITE").ok().as_deref() != Some("1") {
        println!("KHAEN_MOTU_LIVE_WRITE != 1; skipping write verification");
        return;
    }
    let detection = detect(&DetectConfig::default());
    if !detection.is_ready() {
        println!("device not ready ({}); skip", detection.reason);
        return;
    }
    let config = ClientConfig {
        connect_timeout: Duration::from_secs(3),
        sync_timeout: Duration::from_secs(5),
        ..ClientConfig::default()
    };
    let mut client = CueMixClient::connect(config).expect("connect");
    let before = client.read_state().expect("read state");
    let original = before.mute.expect("mute value from device");
    let flipped = !original;
    let report = client.set_mute(flipped).expect("set mute (flip)");
    println!(
        "wrote mute={} readback={}: {:?}",
        flipped, report.readback, report
    );
    let restore = client.set_mute(original).expect("set mute (restore)");
    println!("restored mute={}: {:?}", original, restore);
    let after = client.read_state().expect("read state");
    assert_eq!(after.mute, Some(original), "mute restored");
}
