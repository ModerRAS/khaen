//! MOTU UltraLite-mk5 / CueMix 5 control library and CLI (khaen T40).
//!
//! Purpose: let the khaen daemon detect the device, read its state and make
//! single-key changes (mute / main volume) over the USB-CDC control network,
//! with an immediate readback for every write. No batch writes: one call sends
//! at most one datastore value in one WebSocket frame.
//!
//! The crate is pure std (no external dependencies): a minimal RFC 6455 client,
//! the CueMix datastore codec and RSA-OAEP(SHA-1) auth are implemented here.
//!
//! Typical daemon flow:
//! 1. detect(&DetectConfig::default()) -> DeviceDetection (ready / reasons);
//! 2. CueMixClient::connect(ClientConfig::default());
//! 3. read_state() / set_mute(on) / set_volume(linear);
//! 4. branch on CueMixError::code(), never on Display text.
//!
//! See the crate README for the protocol summary, key mapping evidence and the
//! list of items that can only be validated on the real device (NOT_RUN).

#![forbid(unsafe_code)]

mod client;
mod crypto;
mod detect;
mod error;
mod fader;
mod message;
mod props;
mod rng;
mod snapshot;
mod transport;
mod value;
mod ws;

pub mod cli;

/// Crate version (workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use client::{
    resolve_control_addr, ClientConfig, CueMixClient, CueMixState, DeviceInfo, MainChannel,
    MuteReport, SnapshotReport, Stats, VolumeError, VolumeReport, WriteReport,
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_CONTROL_ADDR, DEFAULT_CONTROL_PATH, DEFAULT_IDLE_DRAIN,
    DEFAULT_READBACK_TIMEOUT, DEFAULT_SNAPSHOT_DRAIN, DEFAULT_SYNC_TIMEOUT,
};
pub use detect::{
    detect, detect_from_text, detect_with, find_motu, parse_asound_cards, DetectConfig,
    DeviceDetection, SoundCard, DEFAULT_CARDS_PATH, DEFAULT_PROBE_TIMEOUT,
    REASON_CARDS_UNAVAILABLE, REASON_CONTROL_UNREACHABLE, REASON_DETECTED, REASON_NO_MOTU_CARD,
};
pub use error::{codes, CueMixError};
pub use fader::{
    db_to_fader, describe_linear, fader_to_db, format_db, parse_volume_text, quantize_linear,
    FADER_DB_FLOOR, FADER_MAX, FADER_MIN,
};
pub use props::{
    bus_label, keys, BUS_NAMES, ID_KO_BUS_FADER, ID_KO_BUS_MUTE, ID_MAIN_TRIM, ID_MODEL_ID,
    ID_MUTE_ENABLE, ID_SAMPLE_RATE, MAIN_FADER_CHANNELS, MAIN_FADER_LEFT_INDEX,
    MAIN_FADER_RIGHT_INDEX,
};
pub use transport::{Transport, WsConfig, WsTransport};
pub use value::Value;
