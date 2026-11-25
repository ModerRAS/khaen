//! RTP（RFC 3550）头解析 + L24/24bit 线性解包/打包（**热路径零分配**）。
//!
//! AES67 媒体面：L24 = 24-bit linear packed，网络字节序（big-endian），通道交错。
//! 本模块只做纯函数、固定大小入参/出参，不分配、不格式化、无 syscall；由
//! `receiver` IO 线程在收包热路径调用。
//!
//! 边界：只支持 RFC 3550 基础头 + CSRC 列表 + 单个 RFC 8285 one-byte header
//! extension（按长度跳过；不解释扩展内容）。不实现 RTCP/SRTP/redundancy。

/// RTP 版本号（v=2）。
pub const RTP_VERSION: u8 = 2;
/// 基础 RTP 头长度（无 CSRC/扩展）。
pub const RTP_HEADER_LEN: usize = 12;
/// L24 每样本字节数。
pub const L24_SAMPLE_BYTES: usize = 3;
/// L24 满量程归一化（与 T22 format.rs S32 decode 同式：i32 / 2^31）。
pub const FULL_SCALE_DIVISOR: f32 = 2_147_483_648.0;

/// RTP 固定头字段（解析结果；Copy）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// 版本（必须 2）。
    pub version: u8,
    /// P 位：末尾 padding。
    pub padding: bool,
    /// X 位：header extension。
    pub extension: bool,
    /// CSRC 计数（0..=15）。
    pub csrc_count: u8,
    /// M 位（AES67 通常 0；只记录）。
    pub marker: bool,
    /// 动态 payload type（7 bit）。
    pub payload_type: u8,
    /// 序号（16 bit 回绕）。
    pub sequence: u16,
    /// 媒体时间戳（48k 采样；16/32 bit 回绕按 RTP 语义）。
    pub timestamp: u32,
    /// 同步源（流切换检测）。
    pub ssrc: u32,
}

impl Header {
    /// 解析 RTP 头，返回头与 payload 在包内的偏移/长度（padding 已扣除）。
    ///
    /// `buf` 为实际收到的 UDP 载荷；返回 `ParsedPacket`（无分配）。
    pub fn parse(buf: &[u8]) -> Result<ParsedPacket, RtpError> {
        if buf.len() < RTP_HEADER_LEN {
            return Err(RtpError::TooShort);
        }
        let version = buf[0] >> 6;
        if version != RTP_VERSION {
            return Err(RtpError::BadVersion);
        }
        let padding = buf[0] & 0x20 != 0;
        let extension = buf[0] & 0x10 != 0;
        let csrc_count = buf[0] & 0x0f;
        let marker = buf[1] & 0x80 != 0;
        let payload_type = buf[1] & 0x7f;
        let sequence = u16::from_be_bytes([buf[2], buf[3]]);
        let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

        let mut offset = RTP_HEADER_LEN + csrc_count as usize * 4;
        if buf.len() < offset {
            return Err(RtpError::CsrcOverflow);
        }
        if extension {
            if buf.len() < offset + 4 {
                return Err(RtpError::ExtensionOverflow);
            }
            let words = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]) as usize;
            offset += 4 + words * 4;
            if buf.len() < offset {
                return Err(RtpError::ExtensionOverflow);
            }
        }
        let mut payload_len = buf.len() - offset;
        if padding {
            if payload_len == 0 {
                return Err(RtpError::BadPadding);
            }
            let padding_len = buf[buf.len() - 1] as usize;
            if padding_len == 0 || padding_len > payload_len {
                return Err(RtpError::BadPadding);
            }
            payload_len -= padding_len;
        }
        Ok(ParsedPacket {
            header: Header {
                version,
                padding,
                extension,
                csrc_count,
                marker,
                payload_type,
                sequence,
                timestamp,
                ssrc,
            },
            payload_offset: offset,
            payload_len,
        })
    }

    /// 写入 12 字节基础头（无 CSRC/扩展；sender/测试用）。
    pub fn write_into(&self, buf: &mut [u8; RTP_HEADER_LEN]) {
        buf[0] = (RTP_VERSION << 6) | ((self.padding as u8) << 5) | ((self.extension as u8) << 4);
        buf[1] = (self.payload_type & 0x7f) | ((self.marker as u8) << 7);
        buf[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        buf[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        buf[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
    }
}

/// 解析后的包视图（header + payload 在 base buffer 内的偏移/长度）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedPacket {
    /// RTP 头。
    pub header: Header,
    /// payload 起始偏移（含跳过 CSRC/扩展后）。
    pub payload_offset: usize,
    /// payload 字节数（已扣除 padding）。
    pub payload_len: usize,
}

impl ParsedPacket {
    /// base buffer 中的 payload 切片。
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.payload_offset..self.payload_offset + self.payload_len]
    }
}

/// RTP 解析/解码错误（Copy；稳定错误名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtpError {
    /// 短于 12 字节基础头。
    TooShort,
    /// 版本不是 2。
    BadVersion,
    /// CSRC 列表越界。
    CsrcOverflow,
    /// header extension 越界。
    ExtensionOverflow,
    /// padding 长度非法。
    BadPadding,
    /// payload 长度不是整帧 / 通道数非法。
    BadPayload,
    /// 输出缓冲不足。
    BufferTooSmall,
}

impl RtpError {
    /// 稳定错误名（证据/日志/JSON 用）。
    pub const fn name(self) -> &'static str {
        match self {
            RtpError::TooShort => "ERR_RTP_TOO_SHORT",
            RtpError::BadVersion => "ERR_RTP_BAD_VERSION",
            RtpError::CsrcOverflow => "ERR_RTP_CSRC_OVERFLOW",
            RtpError::ExtensionOverflow => "ERR_RTP_EXTENSION_OVERFLOW",
            RtpError::BadPadding => "ERR_RTP_BAD_PADDING",
            RtpError::BadPayload => "ERR_RTP_BAD_PAYLOAD",
            RtpError::BufferTooSmall => "ERR_RTP_BUFFER_TOO_SMALL",
        }
    }
}

/// 由 payload 长度与通道数计算帧数（必须整除）。
pub fn frames_in_payload(
    payload_len: usize,
    channels: u16,
    sample_bytes: usize,
) -> Result<usize, RtpError> {
    if channels == 0 || sample_bytes == 0 {
        return Err(RtpError::BadPayload);
    }
    let frame_bytes = channels as usize * sample_bytes;
    if payload_len == 0 || payload_len % frame_bytes != 0 {
        return Err(RtpError::BadPayload);
    }
    Ok(payload_len / frame_bytes)
}

/// 读取第 `sample_index` 个 L24 样本（big-endian 3 字节）、符号扩展并左移 8 位，
/// 得到满量程 i32 表示（与 T22 S32_LE decode 对齐：i32 / 2^31 = 原始 24bit / 2^23）。
///
/// 调用方保证 `sample_index * 3 + 3 <= payload.len()`（由 `frames_in_payload` 校验）。
#[inline]
pub fn unpack_l24_sample(payload: &[u8], sample_index: usize) -> i32 {
    let offset = sample_index * L24_SAMPLE_BYTES;
    let raw = ((payload[offset] as i32) << 16)
        | ((payload[offset + 1] as i32) << 8)
        | (payload[offset + 2] as i32);
    // 符号扩展到 32 位再左移 8 位（低 8 位补 0），得到满量程 i32。
    (raw << 8) as i32
}

/// L24 解包到交错 i32（满量程；`out` 长度须 ≥ frames*channels）。
pub fn unpack_l24_be(payload: &[u8], channels: u16, out: &mut [i32]) -> Result<usize, RtpError> {
    let frames = frames_in_payload(payload.len(), channels, L24_SAMPLE_BYTES)?;
    let samples = frames * channels as usize;
    if out.len() < samples {
        return Err(RtpError::BufferTooSmall);
    }
    for index in 0..samples {
        out[index] = unpack_l24_sample(payload, index);
    }
    Ok(frames)
}

/// 满量程 i32 → L24 big-endian 3 字节（sender/测试用；取高 24 位）。
#[inline]
pub fn pack_l24_sample(sample: i32, out: &mut [u8; L24_SAMPLE_BYTES]) {
    let raw = (sample as u32) >> 8;
    out[0] = (raw >> 16) as u8;
    out[1] = (raw >> 8) as u8;
    out[2] = raw as u8;
}

/// f32 [-1,1] → 满量程 i32（sender 用；饱和到 i32 范围）。
#[inline]
pub fn f32_to_i32_full_scale(sample: f32) -> i32 {
    let scaled = sample as f64 * 2_147_483_648.0;
    if scaled >= 2_147_483_647.0 {
        i32::MAX
    } else if scaled <= -2_147_483_648.0 {
        i32::MIN
    } else {
        scaled as i32
    }
}

/// 满量程 i32 → f32（host 图输入归一化；与 T22 S32 decode 同式）。
#[inline]
pub fn i32_full_scale_to_f32(sample: i32) -> f32 {
    sample as f32 / FULL_SCALE_DIVISOR
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(seq: u16, ts: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
        let header = Header {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 103,
            sequence: seq,
            timestamp: ts,
            ssrc,
        };
        let mut bytes = vec![0u8; RTP_HEADER_LEN + payload.len()];
        {
            let mut head = [0u8; RTP_HEADER_LEN];
            header.write_into(&mut head);
            bytes[..RTP_HEADER_LEN].copy_from_slice(&head);
        }
        bytes[RTP_HEADER_LEN..].copy_from_slice(payload);
        bytes
    }

    #[test]
    fn rtp_header_roundtrip_and_payload_view() {
        let payload = [1u8, 2, 3, 4, 5, 6];
        let bytes = packet(0x1234, 0xdead_beef, 0x0102_0304, &payload);
        let parsed = Header::parse(&bytes).expect("parse");
        assert_eq!(parsed.header.version, RTP_VERSION);
        assert_eq!(parsed.header.payload_type, 103);
        assert_eq!(parsed.header.sequence, 0x1234);
        assert_eq!(parsed.header.timestamp, 0xdead_beef);
        assert_eq!(parsed.header.ssrc, 0x0102_0304);
        assert_eq!(parsed.payload_offset, RTP_HEADER_LEN);
        assert_eq!(parsed.payload(&bytes), &payload);
    }

    #[test]
    fn rtp_header_skips_extension_and_csrc_and_trims_padding() {
        let mut bytes = packet(7, 48, 9, &[0u8; 8]);
        // 构造 CSRC=1 + extension(2 words) 的包：重排为 [header][csrc][ext][payload][pad]。
        let mut full = Vec::new();
        full.extend_from_slice(&bytes[..RTP_HEADER_LEN]);
        full[0] |= 0x01 | 0x10; // CC=1 + X
        full.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]); // csrc
        full.extend_from_slice(&[0xbe, 0xde, 0x00, 0x02]); // profile + 2 words
        full.extend_from_slice(&[0u8; 8]); // extension data
        full.extend_from_slice(&[1, 2, 3, 4]); // payload
        full.extend_from_slice(&[0, 2]); // 2 字节 padding，末字节 = padding 长度 2
        full[0] |= 0x20; // P
        bytes.clear();
        let parsed = Header::parse(&full).expect("parse ext");
        assert_eq!(parsed.payload_offset, RTP_HEADER_LEN + 4 + 4 + 8);
        assert_eq!(parsed.payload(&full), &[1, 2, 3, 4]);
        assert_eq!(parsed.header.csrc_count, 1);
        assert!(parsed.header.extension);
        assert!(parsed.header.padding);
    }

    #[test]
    fn rtp_rejects_malformed() {
        assert_eq!(Header::parse(&[]).unwrap_err(), RtpError::TooShort);
        let mut bad = packet(1, 1, 1, &[0u8; 3]);
        bad[0] = 0x80; // v=2? 0x80>>6 = 2 is fine; force v=1: 0x40
        bad[0] = 0x40;
        assert_eq!(Header::parse(&bad).unwrap_err(), RtpError::BadVersion);
        let mut csrc = packet(1, 1, 1, &[]);
        csrc[0] |= 0x0f; // CC=15 but no CSRC bytes
        assert_eq!(Header::parse(&csrc).unwrap_err(), RtpError::CsrcOverflow);
        let mut padding = packet(1, 1, 1, &[0u8; 4]);
        padding[0] |= 0x20;
        padding[15] = 0; // padding len 0
        assert_eq!(Header::parse(&padding).unwrap_err(), RtpError::BadPadding);
    }

    #[test]
    fn l24_unpack_matches_reference_values() {
        // 3 样本：+1, -1, +0.5（24bit 满量程）
        let payload = [
            0x00, 0x00, 0x01, // +1
            0xff, 0xff, 0xff, // -1
            0x00, 0x00, 0x00, // 0
            0x40, 0x00, 0x00, // +0.5（0x400000 = 2^22 = 0.5 满量程）
        ];
        let mut out = [0i32; 4];
        let frames = unpack_l24_be(&payload, 1, &mut out).expect("unpack");
        assert_eq!(frames, 4);
        assert_eq!(out[0], 1 << 8);
        assert_eq!(out[1], (-1i32) << 8);
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 0x40 << 24); // 0x400000 (24bit) << 8 = 2^30
        assert_eq!(unpack_l24_sample(&payload, 0), 0x1_00);
        // 2 通道交错 1 帧。
        let stereo = [0x00, 0x00, 0x10, 0xff, 0xff, 0xf0];
        let mut out2 = [0i32; 2];
        let frames2 = unpack_l24_be(&stereo, 2, &mut out2).unwrap();
        assert_eq!(frames2, 1);
        assert_eq!(out2[0], 0x10 << 8);
        assert_eq!(out2[1], -(0x10i32) << 8);
    }

    #[test]
    fn l24_pack_roundtrip_and_f32_scale() {
        let mut out = [0u8; 3];
        pack_l24_sample(1 << 8, &mut out);
        assert_eq!(out, [0, 0, 1]);
        pack_l24_sample((-1i32) << 8, &mut out);
        assert_eq!(out, [0xff, 0xff, 0xff]);
        let full = f32_to_i32_full_scale(1.0);
        assert_eq!(i32_full_scale_to_f32(full), 1.0);
        let half = f32_to_i32_full_scale(0.5);
        assert_eq!(i32_full_scale_to_f32(half), 0.5);
        assert_eq!(f32_to_i32_full_scale(2.0), i32::MAX);
        assert_eq!(f32_to_i32_full_scale(-2.0), i32::MIN);
        let mut buf = [0u8; 3];
        // 0.25 满量程 = 0x20000000 (2^29)；取高 24 位 = 0x200000。
        pack_l24_sample(f32_to_i32_full_scale(0.25), &mut buf);
        assert_eq!(buf, [0x20, 0x00, 0x00]);
    }

    #[test]
    fn frames_in_payload_strict() {
        assert_eq!(frames_in_payload(48 * 8 * 3, 8, 3).unwrap(), 48);
        assert_eq!(
            frames_in_payload(47, 8, 3).unwrap_err(),
            RtpError::BadPayload
        );
        assert_eq!(
            frames_in_payload(0, 2, 3).unwrap_err(),
            RtpError::BadPayload
        );
        assert_eq!(
            frames_in_payload(6, 0, 3).unwrap_err(),
            RtpError::BadPayload
        );
    }
}
