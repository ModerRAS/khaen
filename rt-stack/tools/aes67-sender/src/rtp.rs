//! Minimal RTP (RFC 3550) header + AES67 L24 packetizer.
//!
//! Decoupled on purpose from rt-aes67rx's receiver crate (task-235).
//! TODO(task-236/shared): converge on shared SDP/RTP types when the receiver
//! crate freezes its public API; keep field names/semantics aligned until then.

use crate::pcm;

pub const RTP_HEADER_LEN: usize = 12;
pub const RTP_VERSION: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl RtpHeader {
    pub fn new(payload_type: u8, sequence: u16, timestamp: u32, ssrc: u32) -> Self {
        Self {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type,
            sequence,
            timestamp,
            ssrc,
        }
    }

    pub fn to_bytes(&self) -> [u8; RTP_HEADER_LEN] {
        let mut b = [0u8; RTP_HEADER_LEN];
        b[0] = (self.version & 0x03) << 6
            | (u8::from(self.padding)) << 5
            | (u8::from(self.extension)) << 4
            | (self.csrc_count & 0x0F);
        b[1] = (u8::from(self.marker)) << 7 | (self.payload_type & 0x7F);
        b[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        b[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        b[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        b
    }

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < RTP_HEADER_LEN {
            return None;
        }
        let version = buf[0] >> 6;
        if version != RTP_VERSION {
            return None;
        }
        Some(Self {
            version,
            padding: buf[0] & 0x20 != 0,
            extension: buf[0] & 0x10 != 0,
            csrc_count: buf[0] & 0x0F,
            marker: buf[1] & 0x80 != 0,
            payload_type: buf[1] & 0x7F,
            sequence: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }

    /// Byte offset of the payload (header + CSRC list + extension header when present).
    pub fn payload_offset(&self) -> Option<usize> {
        let mut off = RTP_HEADER_LEN + 4 * self.csrc_count as usize;
        if self.extension {
            // 4-byte extension header (profile + length in 32-bit words)
            off += 4;
        }
        Some(off)
    }
}

/// AES67 L24 packetizer: increments sequence per packet, timestamp per sample frame.
pub struct Packetizer {
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl Packetizer {
    pub fn new(payload_type: u8, sequence: u16, timestamp: u32, ssrc: u32) -> Self {
        Self {
            payload_type,
            sequence,
            timestamp,
            ssrc,
        }
    }

    /// Build a full RTP packet (header + L24 payload) into `out`.
    /// `samples` is interleaved f32; `frames` = samples / channels.
    /// Returns the number of audio frames advanced (== frames).
    pub fn next_packet(&mut self, samples: &[f32], marker: bool, out: &mut Vec<u8>) -> usize {
        let mut hdr = RtpHeader::new(self.payload_type, self.sequence, self.timestamp, self.ssrc);
        hdr.marker = marker;
        out.clear();
        out.extend_from_slice(&hdr.to_bytes());
        let bytes_before = out.len();
        out.resize(bytes_before + samples.len() * 3, 0);
        pcm::encode_l24(samples, &mut out[bytes_before..]);
        let frames = if samples.is_empty() { 0 } else { samples.len() };
        // callers always pass interleaved data; frame count is derived by caller.
        self.sequence = self.sequence.wrapping_add(1);
        frames
    }

    pub fn advance(&mut self, frames: u32) {
        self.timestamp = self.timestamp.wrapping_add(frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: true,
            payload_type: 97,
            sequence: 0xBEEF,
            timestamp: 0x1234_5678,
            ssrc: 0xDEAD_BEEF,
        };
        let b = h.to_bytes();
        assert_eq!(b[0], 0x80);
        assert_eq!(b[1], 0x80 | 97);
        assert_eq!(RtpHeader::parse(&b), Some(h));
    }

    #[test]
    fn rejects_short_and_wrong_version() {
        assert!(RtpHeader::parse(&[0u8; 8]).is_none());
        let mut b = [0u8; 12];
        b[0] = 0x40; // version 1
        assert!(RtpHeader::parse(&b).is_none());
    }

    #[test]
    fn packetizer_increments_seq_and_keeps_ts_until_advanced() {
        let mut p = Packetizer::new(97, 65535, 0, 1);
        let mut out = Vec::new();
        let samples = vec![0.0f32; 48 * 2];
        let head = p.next_packet(&samples, false, &mut out);
        assert_eq!(head, 96);
        assert_eq!(out.len(), RTP_HEADER_LEN + 48 * 2 * 3);
        let hdr = RtpHeader::parse(&out).unwrap();
        assert_eq!(hdr.sequence, 65535);
        assert_eq!(hdr.timestamp, 0);
        assert_eq!(hdr.payload_type, 97);
        p.advance(48);
        assert_eq!(p.sequence, 0);
        assert_eq!(p.timestamp, 48);
    }
}
