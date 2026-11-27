//! IO I2a：启动时从 SDP 解析 AES67 单流参数（离线、无设备、可单测）。
//!
//! 这是“SDP → Receiver”的第一步：把生产发送端/部署侧给出的 SDP 变成 host 现有的
//! `Aes67InputConfig` 字段（组播地址/端口/PT/通道/映射）。仍然复用 host 的静态单流
//! 接收器；真正的 SDP 驱动运行时发现/多流属于 I2/I3 后续。
//!
//! 边界：
//! - 只接受 host 接收器当前支持的 **IPv4 组播**；单播 SDP 如实拒绝（I2 的宽松接收
//!   尚未改 socket 绑定策略，不假装已经支持）。
//! - 采样率必须与引擎一致；L24/48k/≤8 通道等 AES67 profile 规则由
//!   `Session::validate_media_profile` 统一校验，不在这里重复。
//! - 隐式通道映射：RTP 通道 i → 输出槽位 i。需要重排时仍走 `KHAEN_AES67_MAP`（I2 后续）。

use std::net::Ipv4Addr;

use khaen_aoip_aes67::sdp::parse_sdp;

/// 从 SDP 得到的输入参数（全部为 host `Aes67InputConfig` 的既有字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdpInput {
    /// 组播地址。
    pub multicast_addr: Ipv4Addr,
    /// RTP 端口。
    pub port: u16,
    /// 动态 payload type。
    pub payload_type: u8,
    /// RTP 通道数。
    pub channels: usize,
    /// RTP 通道 → 输出槽位（隐式顺序映射）。
    pub channels_map: Vec<u16>,
}

/// 解析 SDP 文本并校验它是否能交给当前 host 接收器。
///
/// `input_channels` 是运行时输入宽度；SDP 声明的 RTP 通道数必须 ≤ 它。
pub fn parse_sdp_input(
    text: &str,
    sample_rate: u32,
    input_channels: usize,
) -> Result<SdpInput, String> {
    let session = parse_sdp(text).map_err(|error| format!("SDP parse failed: {error:?}"))?;
    session
        .validate_media_profile()
        .map_err(|error| format!("SDP is not an AES67 media profile: {error:?}"))?;
    let connection = session
        .connection()
        .ok_or_else(|| "SDP has no connection (c=) line".to_string())?;
    let multicast_addr = connection.ipv4();
    if !multicast_addr.is_multicast() {
        return Err(format!(
            "SDP connection {multicast_addr} is unicast; the current host receiver only joins IPv4 multicast (unicast SDP reception is not implemented yet)"
        ));
    }
    let media = &session.media;
    if media.rtpmap.rate_hz != sample_rate {
        return Err(format!(
            "SDP media rate {} Hz does not match the engine sample rate {sample_rate} Hz",
            media.rtpmap.rate_hz
        ));
    }
    let channels = usize::from(media.rtpmap.channels);
    if channels == 0 || channels > input_channels {
        return Err(format!(
            "SDP declares {channels} RTP channel(s), but the runtime input width is {input_channels}; increase KHAEN_INPUT_CHANNELS or split the stream"
        ));
    }
    if media.port == 0 {
        return Err("SDP media port is 0".to_string());
    }
    let channels_map = (0..media.rtpmap.channels).collect();
    Ok(SdpInput {
        multicast_addr,
        port: media.port,
        payload_type: media.payload_type,
        channels,
        channels_map,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_sdp_input;

    const H16_SDP: &str = "v=0\r\n\
o=- 0 0 IN IP4 127.0.0.1\r\n\
s=NuPrime-a227-5240:16\r\n\
c=IN IP4 239.69.16.146/32\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 103\r\n\
a=rtpmap:103 L24/48000/8\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n\
a=mediaclk:direct=0\r\n";

    const UNICAST_SDP: &str = "v=0\r\n\
o=- 0 0 IN IP4 0.0.0.0\r\n\
s=unicast\r\n\
c=IN IP4 192.0.2.222\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n\
a=ptime:1\r\n\
a=sendonly\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";

    #[test]
    fn parses_multicast_sdp_into_host_input_fields() {
        let input = parse_sdp_input(H16_SDP, 48_000, 22).expect("valid SDP");
        assert_eq!(input.multicast_addr.to_string(), "239.69.16.146");
        assert_eq!(input.port, 5004);
        assert_eq!(input.payload_type, 103);
        assert_eq!(input.channels, 8);
        assert_eq!(input.channels_map, (0..8).collect::<Vec<u16>>());
    }

    #[test]
    fn rejects_unicast_and_channel_width_mismatch() {
        let unicast = parse_sdp_input(UNICAST_SDP, 48_000, 22).expect_err("unicast");
        assert!(unicast.contains("unicast"), "{unicast}");
        let too_wide = parse_sdp_input(H16_SDP, 48_000, 2).expect_err("channels");
        assert!(too_wide.contains("input width is 2"), "{too_wide}");
    }

    #[test]
    fn rejects_rate_mismatch_and_malformed_sdp() {
        let rate = parse_sdp_input(H16_SDP, 96_000, 22).expect_err("rate");
        assert!(rate.contains("96"), "{rate}");
        let malformed = parse_sdp_input("not an sdp", 48_000, 22).expect_err("parse");
        assert!(malformed.contains("SDP parse failed"), "{malformed}");
    }
}
