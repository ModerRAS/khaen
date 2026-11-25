//! KHAEN Windows user-space AES67 sender + minimal loopback receiver.
//!
//! Design goals (task-236):
//! - Windows user mode only: no ASIO driver, no virtual sound card installation.
//! - Sources: WAV / raw PCM / synthetic sine / WASAPI loopback (system) /
//!   WASAPI process loopback (Win10 2004+, PER_PROCESS_LOOPBACK).
//! - Output: AES67-style RTP (L24, 48 kHz, configurable ptime/channels),
//!   multicast (239.69.x.x) or unicast, with SDP generation.
//! - No PTP/PHC on this host: RTP timestamps are driven by the local monotonic
//!   clock (QPC). This is NOT strict AES67 PTP media-clock sync; documented in
//!   evidence/aes67-tx-win/CLOCK-AND-SYNC.md.
//!
//! Caveat: this crate keeps its own minimal SDP/RTP types on purpose (decoupled
//! from rt-aes67rx's `crates/aoip-aes67`). TODO(task-236): converge on shared
//! types once the receiver crate freezes its public API.

pub mod analysis;
pub mod cli;
pub mod clock;
pub mod json;
pub mod pacer;
pub mod pcm;
pub mod resample;
pub mod rtp;
pub mod sdp;
pub mod source;
pub mod wav;

#[cfg(windows)]
pub mod wasapi;

/// Audio pipeline constants (AES67 class-A style defaults).
pub const AES67_RATE: u32 = 48_000;
pub const DEFAULT_PTIME_MS: f64 = 1.0;
pub const DEFAULT_PAYLOAD_TYPE: u8 = 97;
pub const DEFAULT_PORT: u16 = 5004;
pub const DEFAULT_MCAST: &str = "239.69.63.10";
