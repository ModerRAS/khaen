//! IO I2a：启动时从 SDP 解析 AES67 单流参数（离线、无设备、可单测）。
//!
//! 这是“SDP → Receiver”的第一步：把生产发送端/部署侧给出的 SDP 变成 host 现有的
//! `Aes67InputConfig` 字段（组播地址/端口/PT/通道/映射）。仍然复用 host 的静态单流
//! 接收器；真正的 SDP 驱动运行时发现/多流属于 I2/I3 后续。
//!
//! 边界：
//! - 同时接受 **IPv4 组播与单播**：组播走 join 组，单播直接 bind 本端目的地址
//!   （I2b 起 host 接收器支持两种绑定）。
//! - 采样率必须与引擎一致；编码必须是 L24、PT 96..=127、通道 1..=8、端口非 0。
//!   ptime 2ms/4ms、缺 ts-refclk、framecount 差异只给 conformance warning，不拒收。
//! - 隐式通道映射：RTP 通道 i → 输出槽位 i；SDP 模式下不支持重排（`KHAEN_AES67_MAP`
//!   必须与隐式映射一致，否则启动 fail-closed）。重排属于 I2 后续。

use std::net::Ipv4Addr;

use khaen_aoip_aes67::sdp::{parse_sdp, Encoding};

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
    /// 非致命 conformance 提示（不拒收；启动时打印）。
    pub warnings: Vec<String>,
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
    let connection = session
        .connection()
        .ok_or_else(|| "SDP has no connection (c=) line".to_string())?;
    let destination = connection.ipv4();
    if destination.is_unspecified() {
        return Err("SDP connection address is 0.0.0.0 (unspecified)".to_string());
    }
    let media = &session.media;
    // IO I2b 宽松接收：只拒绝当前接收器真的做不了的参数（编码/采样率/通道数/PT/端口），
    // ptime 2ms/4ms、缺 ts-refclk、framecount 差异等只给 conformance warning，不拒收。
    let mut warnings = Vec::new();
    if media.rtpmap.encoding != Encoding::L24 {
        return Err(format!(
            "SDP encoding {} is not supported (the host receiver decodes L24 only)",
            media.rtpmap.encoding.as_str()
        ));
    }
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
    if !(96..=127).contains(&media.payload_type) {
        return Err(format!(
            "SDP payload type {} is outside the dynamic RTP range 96..=127",
            media.payload_type
        ));
    }
    if media.port == 0 {
        return Err("SDP media port is 0".to_string());
    }
    if media.ptime_us.is_some_and(|ptime| ptime > 1_000) {
        warnings.push(format!(
            "SDP ptime {} us is larger than the AES67 1 ms profile (accepted; jitter is configured separately)",
            media.ptime_us.unwrap_or_default()
        ));
    }
    if media.refclk.is_none() {
        warnings.push(
            "SDP has no a=ts-refclk (conformance warning; the receiver does not derive its clock from it yet)"
                .to_string(),
        );
    }
    if media
        .framecount
        .is_some_and(|frames| frames == 0 || frames > 192)
    {
        warnings.push(format!(
            "SDP framecount {:?} is outside 1..=192 (accepted; ptime/framecount are informational here)",
            media.framecount
        ));
    }
    let channels_map = (0..media.rtpmap.channels).collect();
    Ok(SdpInput {
        multicast_addr: destination,
        port: media.port,
        payload_type: media.payload_type,
        channels,
        channels_map,
        warnings,
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

    const LOOSE_PTIME_SDP: &str = "v=0\r\n\
o=- 0 0 IN IP4 0.0.0.0\r\n\
s=loose\r\n\
c=IN IP4 239.69.16.147/32\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 97\r\n\
a=rtpmap:97 L24/48000/2\r\n\
a=ptime:4\r\n\
a=sendonly\r\n";

    #[test]
    fn parses_multicast_sdp_into_host_input_fields() {
        let input = parse_sdp_input(H16_SDP, 48_000, 22).expect("valid SDP");
        assert_eq!(input.multicast_addr.to_string(), "239.69.16.146");
        assert_eq!(input.port, 5004);
        assert_eq!(input.payload_type, 103);
        assert_eq!(input.channels, 8);
        assert_eq!(input.channels_map, (0..8).collect::<Vec<u16>>());
        assert!(input.warnings.is_empty(), "{:?}", input.warnings);
    }

    #[test]
    fn accepts_unicast_and_loose_ptime_with_warnings() {
        let unicast = parse_sdp_input(UNICAST_SDP, 48_000, 22).expect("unicast SDP");
        assert_eq!(unicast.multicast_addr.to_string(), "192.0.2.222");
        assert_eq!(unicast.channels, 2);

        let loose = parse_sdp_input(LOOSE_PTIME_SDP, 48_000, 22).expect("loose SDP");
        assert_eq!(loose.payload_type, 97);
        assert!(
            loose
                .warnings
                .iter()
                .any(|warning| warning.contains("ptime")),
            "{:?}",
            loose.warnings
        );
        assert!(
            loose
                .warnings
                .iter()
                .any(|warning| warning.contains("ts-refclk")),
            "{:?}",
            loose.warnings
        );

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
