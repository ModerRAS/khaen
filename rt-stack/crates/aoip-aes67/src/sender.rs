//! 最小 Rust AES67 RTP/L24 **发送端**（离线 loopback 测试 + 手工 demo）。
//!
//! 只实现测试所需子集：单流、L24/48k、组播或单播、正弦/静音/任意交错样本、可变 ptime
//! （每包帧数）。生产发送端/CLI 归 **task-236**（`rt-stack/tools/aes67-sender`，独立
//! Cargo workspace）；本模块只作为 receiver 集成测试与示例的自带载荷源，不写入
//! `tools/` 目录（任务卡与 task-236 的写域保持一致）。
//!
//! - 启动期分配：发送缓冲 `12 + frames×channels×3` 字节 + 每通道正弦相位（一次）；
//! - 发送热路径：固定缓冲写 L24 big-endian + `send_to`，无分配；
//! - 时间戳按每包帧数递增（48k 媒体时钟，RTP timestamp 32 bit 回绕自然）；序号递增；
//! - 组播：可选 TTL（默认 1）与 loopback；本机 loopback 测试用单播 127.0.0.1 更稳定。

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

use crate::rtp::{self, Header};

/// 默认每包帧数（1 ms @48k）。
pub const DEFAULT_FRAMES_PER_PACKET: usize = 48;
/// 默认 payload type（AES67 动态 96..=127）。
pub const DEFAULT_PAYLOAD_TYPE: u8 = 96;
/// 默认 SSRC（测试固定值）。
pub const DEFAULT_SSRC: u32 = 0x5eed_a017;
/// 默认正弦幅度（-12 dBFS，避免 L24 量化/削波争议）。
pub const DEFAULT_SINE_AMPLITUDE: f32 = 0.25;
/// 发送端最大每包帧数（10 ms @48k；超过按配置错误拒收）。
pub const MAX_FRAMES_PER_PACKET: usize = 480;

/// 发送端配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenderConfig {
    /// 目的地址（组播/单播）。
    pub destination: SocketAddrV4,
    /// 通道数（1..=8；与 SDP rtpmap 一致）。
    pub channels: u16,
    /// 每包帧数（ptime×48k；默认 48 = 1 ms）。
    pub frames_per_packet: usize,
    /// 媒体采样率（48k）。
    pub rate_hz: u32,
    /// 动态 payload type（96..=127）。
    pub payload_type: u8,
    /// SSRC。
    pub ssrc: u32,
    /// 起始序号。
    pub sequence_start: u16,
    /// 起始 RTP 时间戳。
    pub timestamp_start: u32,
    /// 组播 TTL（单播忽略）。
    pub multicast_ttl: u32,
    /// 发送 socket 绑定地址（None = 0.0.0.0；loopback 测试用 127.0.0.1）。
    pub bind_interface: Option<Ipv4Addr>,
}

impl SenderConfig {
    /// 单播配置（loopback 测试：127.0.0.1:port）。
    pub fn unicast(destination: SocketAddr, channels: u16) -> Self {
        Self {
            destination: match destination {
                SocketAddr::V4(address) => address,
                SocketAddr::V6(_) => SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
            },
            channels,
            frames_per_packet: DEFAULT_FRAMES_PER_PACKET,
            rate_hz: crate::AES67_RATE_HZ,
            payload_type: DEFAULT_PAYLOAD_TYPE,
            ssrc: DEFAULT_SSRC,
            sequence_start: 0,
            timestamp_start: 0,
            multicast_ttl: 1,
            bind_interface: Some(Ipv4Addr::LOCALHOST),
        }
    }

    /// 组播配置。
    pub fn multicast(
        group: Ipv4Addr,
        port: u16,
        channels: u16,
        bind_interface: Option<Ipv4Addr>,
    ) -> Self {
        let mut config = Self::unicast(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), channels);
        config.destination = SocketAddrV4::new(group, port);
        config.bind_interface = bind_interface;
        config
    }

    /// 校验配置。
    pub fn validate(&self) -> Result<(), SenderError> {
        if self.channels == 0 || self.channels as usize > crate::jitter::MAX_CHANNELS {
            return Err(SenderError::Config);
        }
        if self.frames_per_packet == 0 || self.frames_per_packet > MAX_FRAMES_PER_PACKET {
            return Err(SenderError::Config);
        }
        if self.rate_hz == 0 {
            return Err(SenderError::Config);
        }
        if self.payload_type < 96 {
            return Err(SenderError::Config);
        }
        Ok(())
    }
}

/// 发送端错误。
#[derive(Debug)]
pub enum SenderError {
    /// 配置非法。
    Config,
    /// socket 创建/绑定/选项失败。
    Socket(io::Error),
    /// 发送失败。
    Io(io::Error),
}

impl SenderError {
    /// 稳定错误名。
    pub const fn name(&self) -> &'static str {
        match self {
            SenderError::Config => "ERR_TX_CONFIG",
            SenderError::Socket(_) => "ERR_TX_SOCKET",
            SenderError::Io(_) => "ERR_TX_IO",
        }
    }
}

/// RTP L24 发送端（单流；单线程 `&mut self` 使用）。
#[derive(Debug)]
pub struct Aes67RtpSender {
    socket: UdpSocket,
    config: SenderConfig,
    packet: Vec<u8>,
    phase: Vec<f64>,
    sequence: u16,
    timestamp: u32,
    packets_sent: u64,
    frames_sent: u64,
}

impl Aes67RtpSender {
    /// 创建 socket（组播时设 TTL/loopback）与预分配发送缓冲。
    pub fn bind(config: SenderConfig) -> Result<Self, SenderError> {
        config.validate()?;
        let bind_addr =
            SocketAddr::from((config.bind_interface.unwrap_or(Ipv4Addr::UNSPECIFIED), 0));
        let socket = UdpSocket::bind(bind_addr).map_err(SenderError::Socket)?;
        if crate::sdp::is_multicast_ipv4(*config.destination.ip()) {
            socket
                .set_multicast_ttl_v4(config.multicast_ttl)
                .map_err(SenderError::Socket)?;
            socket
                .set_multicast_loop_v4(true)
                .map_err(SenderError::Socket)?;
        }
        let packet_len = rtp::RTP_HEADER_LEN
            + config.frames_per_packet * config.channels as usize * rtp::L24_SAMPLE_BYTES;
        Ok(Self {
            socket,
            config,
            packet: vec![0u8; packet_len],
            phase: vec![0.0f64; config.channels as usize],
            sequence: config.sequence_start,
            timestamp: config.timestamp_start,
            packets_sent: 0,
            frames_sent: 0,
        })
    }

    /// 配置。
    pub fn config(&self) -> SenderConfig {
        self.config
    }

    /// 已发包数。
    pub fn packets_sent(&self) -> u64 {
        self.packets_sent
    }

    /// 已发帧数。
    pub fn frames_sent(&self) -> u64 {
        self.frames_sent
    }

    /// 发送 1 ms 静音包（全 0 L24）。
    pub fn send_silence(&mut self) -> Result<usize, SenderError> {
        for slot in self.packet[rtp::RTP_HEADER_LEN..].iter_mut() {
            *slot = 0;
        }
        self.send_current_packet()
    }

    /// 发送正弦包（所有通道同频同相；栈上音调表，无堆分配）。
    pub fn send_sine(&mut self, tone_hz: f32) -> Result<usize, SenderError> {
        let channels = self.config.channels as usize;
        if channels > crate::jitter::MAX_CHANNELS {
            return Err(SenderError::Config);
        }
        let mut tones = [tone_hz; crate::jitter::MAX_CHANNELS];
        let slice = &mut tones[..channels];
        slice.fill(tone_hz);
        self.send_sine_channels(slice, DEFAULT_SINE_AMPLITUDE)
    }

    /// 发送逐通道正弦包（tones.len() 必须 = channels）。
    pub fn send_sine_channels(
        &mut self,
        tones: &[f32],
        amplitude: f32,
    ) -> Result<usize, SenderError> {
        if tones.len() != self.config.channels as usize {
            return Err(SenderError::Config);
        }
        let frames = self.config.frames_per_packet;
        let channels = self.config.channels as usize;
        let rate = self.config.rate_hz as f64;
        let mut payload = rtp::RTP_HEADER_LEN;
        for _frame in 0..frames {
            for channel in 0..channels {
                let sample = (self.phase[channel].sin() as f32) * amplitude;
                let full_scale = rtp::f32_to_i32_full_scale(sample);
                let mut encoded = [0u8; rtp::L24_SAMPLE_BYTES];
                rtp::pack_l24_sample(full_scale, &mut encoded);
                self.packet[payload..payload + rtp::L24_SAMPLE_BYTES].copy_from_slice(&encoded);
                payload += rtp::L24_SAMPLE_BYTES;
            }
            for channel in 0..channels {
                let increment = std::f64::consts::TAU * tones[channel] as f64 / rate;
                self.phase[channel] += increment;
                if self.phase[channel] >= std::f64::consts::TAU {
                    self.phase[channel] -= std::f64::consts::TAU;
                }
            }
        }
        self.send_current_packet()
    }

    /// 发送一包任意交错 f32 样本（len 必须 = frames_per_packet × channels）。
    pub fn send_samples(&mut self, samples: &[f32]) -> Result<usize, SenderError> {
        let expected = self.config.frames_per_packet * self.config.channels as usize;
        if samples.len() != expected {
            return Err(SenderError::Config);
        }
        let mut payload = rtp::RTP_HEADER_LEN;
        for &sample in samples.iter() {
            let full_scale = rtp::f32_to_i32_full_scale(sample);
            let mut encoded = [0u8; rtp::L24_SAMPLE_BYTES];
            rtp::pack_l24_sample(full_scale, &mut encoded);
            self.packet[payload..payload + rtp::L24_SAMPLE_BYTES].copy_from_slice(&encoded);
            payload += rtp::L24_SAMPLE_BYTES;
        }
        self.send_current_packet()
    }

    /// 写 RTP 头并发出当前包（热路径：固定缓冲 + send_to）。
    fn send_current_packet(&mut self) -> Result<usize, SenderError> {
        let header = Header {
            version: rtp::RTP_VERSION,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: self.config.payload_type,
            sequence: self.sequence,
            timestamp: self.timestamp,
            ssrc: self.config.ssrc,
        };
        let mut head = [0u8; rtp::RTP_HEADER_LEN];
        header.write_into(&mut head);
        self.packet[..rtp::RTP_HEADER_LEN].copy_from_slice(&head);
        let sent = self
            .socket
            .send_to(&self.packet, self.config.destination)
            .map_err(SenderError::Io)?;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self
            .timestamp
            .wrapping_add(self.config.frames_per_packet as u32);
        self.packets_sent += 1;
        self.frames_sent += self.config.frames_per_packet as u64;
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::time::Duration;

    #[test]
    fn sender_config_validation_and_defaults() {
        let config = SenderConfig::unicast(SocketAddr::from((Ipv4Addr::LOCALHOST, 5_004)), 2);
        config.validate().expect("valid");
        assert_eq!(config.frames_per_packet, 48);
        assert_eq!(config.payload_type, 96);
        let mut bad = config;
        bad.channels = 0;
        assert_eq!(bad.validate().unwrap_err().name(), "ERR_TX_CONFIG");
        let mut bad = config;
        bad.frames_per_packet = MAX_FRAMES_PER_PACKET + 1;
        assert_eq!(bad.validate().unwrap_err().name(), "ERR_TX_CONFIG");
        let multicast = SenderConfig::multicast(Ipv4Addr::new(239, 69, 16, 146), 5_004, 8, None);
        assert!(crate::sdp::is_multicast_ipv4(*multicast.destination.ip()));
    }

    #[test]
    fn sender_sends_rtp_l24_packets_with_expected_layout() {
        let listener = UdpSocket::bind("127.0.0.1:0").expect("listener");
        listener
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("timeout");
        let port = listener.local_addr().expect("addr").port();
        let mut sender = Aes67RtpSender::bind(SenderConfig::unicast(
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            2,
        ))
        .expect("sender");
        let mut buffer = [0u8; 4_096];
        for _ in 0..3 {
            sender.send_sine(1_000.0).expect("send sine");
        }
        for index in 0..3u16 {
            let (length, _) = listener.recv_from(&mut buffer).expect("recv");
            let parsed = rtp::Header::parse(&buffer[..length]).expect("rtp parse");
            assert_eq!(parsed.header.sequence, index);
            assert_eq!(parsed.header.timestamp, index as u32 * 48);
            assert_eq!(parsed.header.payload_type, 96);
            assert_eq!(parsed.header.ssrc, DEFAULT_SSRC);
            assert_eq!(
                parsed.payload_len,
                48 * 2 * rtp::L24_SAMPLE_BYTES,
                "48 frames × 2ch × 3B"
            );
            let mut samples = vec![0i32; 48 * 2];
            let frames = rtp::unpack_l24_be(parsed.payload(&buffer[..length]), 2, &mut samples)
                .expect("unpack");
            assert_eq!(frames, 48);
            // 正弦非全零且不削波。
            assert!(samples.iter().any(|&sample| sample != 0));
            assert!(samples
                .iter()
                .all(|&sample| sample > i32::MIN && sample < i32::MAX));
        }
        assert_eq!(sender.packets_sent(), 3);
        assert_eq!(sender.frames_sent(), 144);
    }
}
