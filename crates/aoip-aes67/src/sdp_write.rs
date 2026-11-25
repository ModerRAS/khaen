//! SDP 生成（RFC 8866/4566 子集；AES67 媒体面；兼容 ST 2110-30 信令字段）。
//!
//! 与 [`crate::sdp`] 的解析器互逆：本模块**只生成**，解析器**只解析**；两者由单测做
//! 写-读 round-trip 核验（[`crate::sap`] 通告也用本模块输出作为 SAP payload）。
//!
//! 覆盖字段：
//! - 会话级：`v=0` / `o=`（RFC 8866 §5.2，含 origin 地址）/ `s=` / `c=`（IPv4，
//!   组播默认 `/32`）/ `t=0 0`；
//! - 媒体级：`m=audio <port> RTP/AVP <pt>`、`a=rtpmap:<pt> <L16|L24>/<rate>/<ch>`
//!   （RFC 3551 / RFC 3190）、`a=ptime:<ms>`（≤3 位小数，µs→ms）、
//!   `a=framecount:<n>`（ST 2110-30/RAVENNA 扩展；AES67 本身不定义）、
//!   `a=ts-refclk:`（RFC 7273：`ptp=IEEE1588-2008[:<gmid>][:<domain>]` / `local` /
//!   legacy `localmac=`）、`a=mediaclk:direct=<n>|sender`（RFC 7273）、
//!   `a=fmtp:<pt> channel-order=SMPTE2110.(...)`（ST 2110-30；本子集只校验数字分组
//!   总数 = 通道数）、`a=source-filter: incl|excl IN IP4 <dest> <src>`（RFC 4570
//!   子集，单源）、`a=sendonly|recvonly|sendrecv|inactive`（RFC 8866 §6）。
//!
//! 输出契约：
//! - **CRLF 行尾**（RFC 8866 §5：SDP 行以 CRLF 结束；生成文本每行含 `\r\n` 且以
//!   `\r\n` 结尾）；
//! - **严格 ASCII**：任何字段含非 ASCII / CR / LF / NUL 一律返回明确错误，不输出半成品；
//! - 字段全部由结构体决定，不做隐式猜测；`sendrecv` 是缺省方向，生成时省略该属性行。
//!
//! EXTENSION / 口径：
//! - `framecount`、`channel-order` 不是 AES67-2013/2015 强制字段，属 ST 2110-30 或
//!   RAVENNA 实践；本模块可选输出并在解析侧记录，不改变 AES67 媒体 profile 门（48k/L24）。
//! - `source-filter` 的 SSM 源加入（IGMPv3）不在 P0 实现内（P2）；这里只做 SDP 声明
//!   与一致性校验。
//! - 44.1k 等非 48k 采样率可以生成/解析（`EXTENSION`），但 `validate_media_profile`
//!   会按 P0 口径拒绝（44.1k 1 ms = 48 帧 → `a=ptime:1.09`，见实现指南 §7）。

use std::net::Ipv4Addr;

use crate::sdp::{
    parse_channel_order, Connection, Direction, Encoding, MediaClk, RefClk, SourceFilter,
    AES67_MAX_FRAMECOUNT,
};

/// SDP 生成错误（Copy + 稳定错误名；不分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpWriteError {
    /// 文本字段含非 ASCII 字符。
    NonAscii,
    /// 文本字段含 CR / LF / NUL（防注入，且 SDP 行必须 CRLF 分隔）。
    ControlChar,
    /// 必填文本字段为空或含空白（如 `o=` username）。
    EmptyValue,
    /// `o=` 的会话 id/版本/用户名不合法。
    BadOrigin,
    /// 媒体端口为 0（AES67 媒体面不接受禁用媒体）。
    BadPort,
    /// 每 flow 通道数为 0。
    BadChannels,
    /// `a=ptime` 为 0，或与 `a=framecount` 不一致。
    BadPtime,
    /// `a=framecount` 为 0 或超过 192（4 ms @48k）。
    BadFramecount,
    /// `channel-order` 语法非法，或分组总数与通道数不符。
    BadChannelOrder,
    /// `source-filter` 目的地址与 `c=` 不一致。
    BadSourceFilter,
    /// `a=rtpmap` 采样率为 0。
    BadRate,
}

impl SdpWriteError {
    /// 稳定错误名（日志/证据 JSON 用）。
    pub const fn name(self) -> &'static str {
        match self {
            SdpWriteError::NonAscii => "ERR_SDPW_NON_ASCII",
            SdpWriteError::ControlChar => "ERR_SDPW_CONTROL_CHAR",
            SdpWriteError::EmptyValue => "ERR_SDPW_EMPTY_VALUE",
            SdpWriteError::BadOrigin => "ERR_SDPW_BAD_ORIGIN",
            SdpWriteError::BadPort => "ERR_SDPW_BAD_PORT",
            SdpWriteError::BadChannels => "ERR_SDPW_BAD_CHANNELS",
            SdpWriteError::BadPtime => "ERR_SDPW_BAD_PTIME",
            SdpWriteError::BadFramecount => "ERR_SDPW_BAD_FRAMECOUNT",
            SdpWriteError::BadChannelOrder => "ERR_SDPW_BAD_CHANNEL_ORDER",
            SdpWriteError::BadSourceFilter => "ERR_SDPW_BAD_SOURCE_FILTER",
            SdpWriteError::BadRate => "ERR_SDPW_BAD_RATE",
        }
    }
}

/// SDP `o=` 会话源（拥有所有权；生成用）。解析结果见 [`crate::sdp::Origin`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdpOrigin {
    /// 用户名（RFC 8866 §5.2；无用户名用 `-`，不得含空白）。
    pub username: String,
    /// 会话 id（NTP 时间戳风格；调用方生成，会话生命周期内稳定）。
    pub session_id: u64,
    /// 会话版本（SDP 内容变化时递增；SAP 侧配合 msg id hash 变化）。
    pub session_version: u64,
    /// 源 IPv4 地址（SAP 通告要求与 originating source 一致）。
    pub address: Ipv4Addr,
}

impl SdpOrigin {
    /// 以 `-` 用户名构造。
    pub fn new(session_id: u64, session_version: u64, address: Ipv4Addr) -> Self {
        Self {
            username: "-".to_string(),
            session_id,
            session_version,
            address,
        }
    }

    /// 生成 `o=` 行（不含 CRLF）。
    ///
    /// SAP 删除包 payload 就是这一行（RFC 2974 §6：application/sdp 的删除消息是
    /// 「单行 origin 字段」），由 [`crate::sap`] 复用。
    pub fn line(&self) -> Result<String, SdpWriteError> {
        check_text(&self.username)?;
        if self.username.is_empty() || self.username.chars().any(char::is_whitespace) {
            return Err(SdpWriteError::EmptyValue);
        }
        Ok(format!(
            "o={} {} {} IN IP4 {}",
            self.username, self.session_id, self.session_version, self.address
        ))
    }
}

/// 一条 `m=audio` 媒体描述（生成用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdpMedia {
    /// RTP 端口（`m=` 的 <port>）。
    pub port: u16,
    /// 动态 payload type（AES67 要求 96..=127；本层不强制，媒体 profile 门校验）。
    pub payload_type: u8,
    /// 编码（L24 = AES67 媒体面；L16 = 兼容）。
    pub encoding: Encoding,
    /// RTP 时钟率（AES67 媒体面 48k；44.1k 等为非 P0 profile 的 EXTENSION）。
    pub rate_hz: u32,
    /// 每包通道数（1..=8 为 AES67 媒体面）。
    pub channels: u16,
    /// `a=ptime`（µs；None = 不输出该行）。
    pub ptime_us: Option<u32>,
    /// `a=framecount`（每包媒体帧数；None = 不输出）。
    pub framecount: Option<u32>,
    /// `a=ts-refclk`（None = 不输出；AES67 媒体面接收端会拒绝缺省）。
    pub refclk: Option<RefClk>,
    /// `a=mediaclk`（None = 不输出）。
    pub mediaclk: Option<MediaClk>,
    /// 媒体方向（SendRecv 省略属性行）。
    pub direction: Direction,
    /// `fmtp` 的 channel-order 原始值（如 `SMPTE2110.(71)`；None = 不输出）。
    pub channel_order: Option<String>,
    /// `a=source-filter`（None = 不输出）。
    pub source_filter: Option<SourceFilter>,
}

/// 单会话 SDP 生成描述（RFC 4566 子集；AES67 媒体面一条 flow 一个 SDP）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdpDescription {
    /// `o=` 会话源。
    pub origin: SdpOrigin,
    /// `s=` 会话名（只用于展示，不影响媒体）。
    pub session_name: String,
    /// `c=` 连接（IPv4；组播未给 TTL 时按 AES67 实践输出 `/32`）。
    pub connection: Connection,
    /// 唯一 `m=audio` 媒体描述。
    pub media: SdpMedia,
}

impl SdpDescription {
    /// 构造。
    pub fn new(
        origin: SdpOrigin,
        session_name: impl Into<String>,
        connection: Connection,
        media: SdpMedia,
    ) -> Self {
        Self {
            origin,
            session_name: session_name.into(),
            connection,
            media,
        }
    }

    /// 生成 SDP 文本（CRLF 行尾、严格 ASCII；失败返回明确错误，不输出半成品）。
    pub fn to_sdp(&self) -> Result<String, SdpWriteError> {
        let media = &self.media;
        check_text(&self.session_name)?;
        let origin_line = self.origin.line()?;
        if media.port == 0 {
            return Err(SdpWriteError::BadPort);
        }
        if media.channels == 0 {
            return Err(SdpWriteError::BadChannels);
        }
        if media.rate_hz == 0 {
            return Err(SdpWriteError::BadRate);
        }
        if let Some(ptime_us) = media.ptime_us {
            if ptime_us == 0 {
                return Err(SdpWriteError::BadPtime);
            }
        }
        if let Some(framecount) = media.framecount {
            if framecount == 0 || framecount > AES67_MAX_FRAMECOUNT {
                return Err(SdpWriteError::BadFramecount);
            }
            if let Some(ptime_us) = media.ptime_us {
                // ptime（µs）↔ framecount 一致性：允许 ±1 帧量化（44.1k 1.09 ms = 48 帧）。
                let expected_us = framecount as u64 * 1_000_000 / media.rate_hz as u64;
                let tolerance = 1_000_000 / media.rate_hz as u64;
                let actual = ptime_us as u64;
                if actual + tolerance < expected_us || actual > expected_us + tolerance {
                    return Err(SdpWriteError::BadPtime);
                }
            }
        }
        if let Some(order) = &media.channel_order {
            check_text(order)?;
            if order.chars().any(char::is_whitespace) {
                return Err(SdpWriteError::BadChannelOrder);
            }
            let groups = parse_channel_order(order).map_err(|_| SdpWriteError::BadChannelOrder)?;
            let total: u32 = groups.iter().map(|n| *n as u32).sum();
            if total != media.channels as u32 {
                return Err(SdpWriteError::BadChannelOrder);
            }
        }
        if let Some(filter) = media.source_filter {
            if filter.destination != self.connection.ipv4() {
                return Err(SdpWriteError::BadSourceFilter);
            }
        }

        let mut lines: Vec<String> = Vec::with_capacity(16);
        lines.push("v=0".to_string());
        lines.push(origin_line);
        lines.push(format!("s={}", self.session_name));
        lines.push(render_connection(self.connection));
        lines.push("t=0 0".to_string());
        lines.push(format!(
            "m=audio {} RTP/AVP {}",
            media.port, media.payload_type
        ));
        lines.push(format!(
            "a=rtpmap:{} {}/{}/{}",
            media.payload_type,
            media.encoding.as_str(),
            media.rate_hz,
            media.channels
        ));
        if let Some(order) = &media.channel_order {
            lines.push(format!(
                "a=fmtp:{} channel-order={}",
                media.payload_type, order
            ));
        }
        if let Some(filter) = media.source_filter {
            lines.push(format!(
                "a=source-filter: {} IN IP4 {} {}",
                if filter.include { "incl" } else { "excl" },
                filter.destination,
                filter.source
            ));
        }
        if let Some(attribute) = media.direction.attribute() {
            lines.push(attribute.to_string());
        }
        if let Some(ptime_us) = media.ptime_us {
            lines.push(format!("a=ptime:{}", format_ptime_ms(ptime_us)?));
        }
        if let Some(framecount) = media.framecount {
            lines.push(format!("a=framecount:{framecount}"));
        }
        if let Some(refclk) = media.refclk {
            lines.push(format!("a=ts-refclk:{}", render_refclk(refclk)));
        }
        if let Some(mediaclk) = media.mediaclk {
            lines.push(format!("a=mediaclk:{}", render_mediaclk(mediaclk)));
        }

        let mut text = String::with_capacity(lines.iter().map(|line| line.len() + 2).sum());
        for line in &lines {
            if !line.is_ascii() {
                return Err(SdpWriteError::NonAscii);
            }
            text.push_str(line);
            text.push_str("\r\n");
        }
        Ok(text)
    }
}

/// `c=` 行：组播未给 TTL 时按 AES67 实践输出 `/32`；单播不输出后缀。
fn render_connection(connection: Connection) -> String {
    let address = connection.ipv4();
    match connection.ttl {
        Some(ttl) => format!("c=IN IP4 {address}/{ttl}"),
        None if connection.is_multicast() => format!("c=IN IP4 {address}/32"),
        None => format!("c=IN IP4 {address}"),
    }
}

/// µs → SDP `ptime`（ms，最多 3 位小数；整数 ms 不带小数点）。
fn format_ptime_ms(ptime_us: u32) -> Result<String, SdpWriteError> {
    if ptime_us == 0 {
        return Err(SdpWriteError::BadPtime);
    }
    let whole = ptime_us / 1_000;
    let fraction = ptime_us % 1_000;
    if fraction == 0 {
        return Ok(whole.to_string());
    }
    let mut digits = format!("{fraction:03}");
    while digits.ends_with('0') {
        digits.pop();
    }
    Ok(format!("{whole}.{digits}"))
}

/// `a=ts-refclk` 值（RFC 7273 子集；legacy localmac 兼容旧 NSP 发送端）。
fn render_refclk(refclk: RefClk) -> String {
    match refclk {
        RefClk::Ptp(ptp) => {
            if ptp.has_grandmaster {
                format!(
                    "ptp=IEEE1588-2008:{}:{}",
                    format_hex_dash(&ptp.grandmaster),
                    ptp.domain
                )
            } else {
                format!("ptp=IEEE1588-2008:{}", ptp.domain)
            }
        }
        RefClk::Local => "local".to_string(),
        RefClk::LocalMac(mac) => format!("localmac={}", format_hex_dash(&mac)),
    }
}

/// `a=mediaclk` 值（RFC 7273 子集：direct=<offset> / sender）。
fn render_mediaclk(mediaclk: MediaClk) -> String {
    if mediaclk.direct {
        format!("direct={}", mediaclk.offset_sec)
    } else {
        "sender".to_string()
    }
}

/// 字节数组 → `xx-xx-...` 小写十六进制（GMID / MAC 共用）。
fn format_hex_dash(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            text.push('-');
        }
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// 文本字段校验：严格 ASCII，且不含 CR/LF/NUL。
fn check_text(value: &str) -> Result<(), SdpWriteError> {
    if !value.is_ascii() {
        return Err(SdpWriteError::NonAscii);
    }
    if value.contains('\r') || value.contains('\n') || value.contains('\0') {
        return Err(SdpWriteError::ControlChar);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::{parse_sdp, Direction, Encoding, PtpRefClk, RefClk};

    fn multicast_connection() -> Connection {
        Connection {
            address: [239, 69, 1, 1],
            ttl: Some(32),
        }
    }

    fn unicast_connection() -> Connection {
        Connection {
            address: [192, 0, 2, 222],
            ttl: None,
        }
    }

    fn aes67_8ch_media() -> SdpMedia {
        SdpMedia {
            port: 5_004,
            payload_type: 96,
            encoding: Encoding::L24,
            rate_hz: 48_000,
            channels: 8,
            ptime_us: Some(1_000),
            framecount: Some(48),
            refclk: Some(RefClk::Ptp(PtpRefClk {
                domain: 0,
                has_grandmaster: true,
                grandmaster: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
            })),
            mediaclk: Some(MediaClk {
                direct: true,
                offset_sec: 0,
            }),
            direction: Direction::SendOnly,
            channel_order: Some("SMPTE2110.(71)".to_string()),
            source_filter: Some(SourceFilter {
                destination: Ipv4Addr::new(239, 69, 1, 1),
                source: Ipv4Addr::new(192, 0, 2, 222),
                include: true,
            }),
        }
    }

    fn aes67_8ch_description() -> SdpDescription {
        SdpDescription::new(
            SdpOrigin::new(1_311_738_121, 1, Ipv4Addr::new(192, 0, 2, 222)),
            "NativeSpatialPipe AES67 output flow0",
            multicast_connection(),
            aes67_8ch_media(),
        )
    }

    #[test]
    fn sdp_write_roundtrip_8ch_l24_1ms_full_fields() {
        let description = aes67_8ch_description();
        let text = description.to_sdp().expect("render");
        assert!(text.is_ascii(), "SDP 必须严格 ASCII");
        assert!(text.ends_with("\r\n"));
        assert!(!text.replace("\r\n", "").contains('\n'), "只允许 CRLF 行尾");
        for expected in [
            "v=0\r\n",
            "o=- 1311738121 1 IN IP4 192.0.2.222\r\n",
            "s=NativeSpatialPipe AES67 output flow0\r\n",
            "c=IN IP4 239.69.1.1/32\r\n",
            "t=0 0\r\n",
            "m=audio 5004 RTP/AVP 96\r\n",
            "a=rtpmap:96 L24/48000/8\r\n",
            "a=fmtp:96 channel-order=SMPTE2110.(71)\r\n",
            "a=source-filter: incl IN IP4 239.69.1.1 192.0.2.222\r\n",
            "a=sendonly\r\n",
            "a=ptime:1\r\n",
            "a=framecount:48\r\n",
            "a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n",
            "a=mediaclk:direct=0\r\n",
        ] {
            assert!(text.contains(expected), "缺少行 {expected:?}；SDP:\n{text}");
        }

        let session = parse_sdp(&text).expect("generated SDP must parse");
        let origin = session.origin.expect("o=");
        assert_eq!(origin.username, "-");
        assert_eq!(origin.session_id, 1_311_738_121);
        assert_eq!(origin.session_version, 1);
        assert_eq!(origin.ipv4(), Ipv4Addr::new(192, 0, 2, 222));
        assert_eq!(session.name, Some("NativeSpatialPipe AES67 output flow0"));
        let connection = session.connection().expect("c=");
        assert_eq!(connection.ipv4(), Ipv4Addr::new(239, 69, 1, 1));
        assert_eq!(connection.ttl, Some(32));
        assert!(connection.is_multicast());
        let media = &session.media;
        assert_eq!(media.port, 5_004);
        assert_eq!(media.payload_type, 96);
        assert_eq!(media.rtpmap.encoding, Encoding::L24);
        assert_eq!(media.rtpmap.rate_hz, 48_000);
        assert_eq!(media.rtpmap.channels, 8);
        assert_eq!(media.ptime_us, Some(1_000));
        assert_eq!(media.framecount, Some(48));
        assert_eq!(media.channel_order, Some("SMPTE2110.(71)"));
        assert_eq!(media.direction, Direction::SendOnly);
        assert!(media.mediaclk.expect("mediaclk").direct);
        let RefClk::Ptp(ptp) = media.refclk.expect("refclk") else {
            panic!("PTP refclk expected");
        };
        assert!(ptp.has_grandmaster);
        assert_eq!(ptp.domain, 0);
        assert_eq!(ptp.grandmaster[7], 0x77);
        let filter = media.source_filter.expect("source-filter");
        assert!(filter.include);
        assert_eq!(filter.destination, Ipv4Addr::new(239, 69, 1, 1));
        assert_eq!(filter.source, Ipv4Addr::new(192, 0, 2, 222));
        session
            .validate_media_profile()
            .expect("写出的 AES67 8ch/L24/1ms SDP 必须通过媒体 profile 门");
    }

    #[test]
    fn sdp_write_44k1_corrected_sample_roundtrip() {
        // 44.1k 1 ms 不是整数样本：AES67 Table 2 / 实现指南 §7 取 48 帧 → a=ptime:1.09。
        let mut media = aes67_8ch_media();
        media.rate_hz = 44_100;
        media.ptime_us = Some(1_090);
        media.framecount = Some(48);
        media.refclk = Some(RefClk::Local);
        media.mediaclk = Some(MediaClk {
            direct: false,
            offset_sec: 0,
        });
        media.channel_order = None;
        media.source_filter = None;
        media.direction = Direction::SendOnly;
        let description = SdpDescription::new(
            SdpOrigin::new(42, 3, Ipv4Addr::new(192, 0, 2, 222)),
            "44.1k corrected flow",
            unicast_connection(),
            media,
        );
        let text = description.to_sdp().expect("render 44.1k");
        assert!(text.contains("a=rtpmap:96 L24/44100/8\r\n"));
        assert!(text.contains("a=ptime:1.09\r\n"));
        assert!(text.contains("a=framecount:48\r\n"));
        assert!(text.contains("c=IN IP4 192.0.2.222\r\n"));
        assert!(text.contains("a=ts-refclk:local\r\n"));
        assert!(text.contains("a=mediaclk:sender\r\n"));

        let session = parse_sdp(&text).expect("44.1k SDP must parse");
        assert_eq!(session.origin.expect("o=").session_version, 3);
        assert_eq!(session.media.rtpmap.rate_hz, 44_100);
        assert_eq!(session.media.ptime_us, Some(1_090));
        assert_eq!(session.media.framecount, Some(48));
        assert_eq!(session.media.refclk, Some(RefClk::Local));
        assert!(!session.media.mediaclk.expect("mediaclk").direct);
        // 44.1k 非 P0 AES67 媒体 profile（本 crate P0 只宣称 48k）。
        assert_eq!(
            session.validate_media_profile().unwrap_err(),
            crate::sdp::SdpError::UnsupportedRate
        );
    }

    #[test]
    fn sdp_write_recvonly_l16_and_optional_fields_roundtrip() {
        let mut media = aes67_8ch_media();
        media.encoding = Encoding::L16;
        media.channels = 2;
        media.ptime_us = Some(250);
        media.framecount = None;
        media.refclk = Some(RefClk::LocalMac([0x02, 0x4E, 0x53, 0x50, 0x00, 0x01]));
        media.mediaclk = None;
        media.direction = Direction::RecvOnly;
        media.channel_order = None;
        media.source_filter = None;
        media.port = 5_006;
        media.payload_type = 97;
        let description = SdpDescription::new(
            SdpOrigin::new(7, 2, Ipv4Addr::new(192, 0, 2, 222)),
            "recvonly monitoring flow",
            unicast_connection(),
            media,
        );
        let text = description.to_sdp().expect("render recvonly");
        assert!(text.contains("a=rtpmap:97 L16/48000/2\r\n"));
        assert!(text.contains("a=ptime:0.25\r\n"));
        assert!(text.contains("a=recvonly\r\n"));
        assert!(text.contains("a=ts-refclk:localmac=02-4e-53-50-00-01\r\n"));
        assert!(!text.contains("a=mediaclk:"));
        assert!(!text.contains("a=framecount:"));

        let session = parse_sdp(&text).expect("recvonly SDP must parse");
        assert_eq!(session.media.rtpmap.encoding, Encoding::L16);
        assert_eq!(session.media.rtpmap.channels, 2);
        assert_eq!(session.media.ptime_us, Some(250));
        assert_eq!(session.media.direction, Direction::RecvOnly);
        assert_eq!(
            session.media.refclk,
            Some(RefClk::LocalMac([0x02, 0x4E, 0x53, 0x50, 0x00, 0x01]))
        );
        assert_eq!(session.media.mediaclk, None);
        assert_eq!(session.media.framecount, None);
    }

    #[test]
    fn sdp_write_crlf_ascii_defaults_and_ptime_formatting() {
        let description = aes67_8ch_description();
        let text = description.to_sdp().expect("render");
        // 每行均以 CRLF 结束，末行同样；没有裸 LF；无 NUL。
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines.last(), Some(&""));
        assert!(lines
            .iter()
            .all(|line| !line.contains('\n') && !line.contains('\r')));
        assert!(!text.contains('\0'));
        assert!(text.is_ascii());
        // 组播缺省 TTL → /32；MediaDescription 的 sendrecv 缺省被省略。
        let mut media = aes67_8ch_media();
        media.direction = Direction::SendRecv;
        media.channel_order = None;
        media.source_filter = None;
        let mut connection = multicast_connection();
        connection.ttl = None;
        let minimal = SdpDescription::new(
            SdpOrigin::new(1, 1, Ipv4Addr::new(10, 0, 0, 7)),
            "min",
            connection,
            media.clone(),
        );
        let minimal_text = minimal.to_sdp().expect("render minimal");
        assert!(minimal_text.contains("c=IN IP4 239.69.1.1/32\r\n"));
        assert!(!minimal_text.contains("a=sendonly"));
        assert!(!minimal_text.contains("a=sendrecv"));
        // ptime µs → ms 文本（去掉 framecount，避免与 ptime 一致性校验耦合）。
        media.framecount = None;
        let cases = [
            (1_000u32, "1"),
            (250, "0.25"),
            (333, "0.333"),
            (1_090, "1.09"),
            (500, "0.5"),
        ];
        for (ptime_us, expected) in cases {
            media.ptime_us = Some(ptime_us);
            let description = SdpDescription::new(
                SdpOrigin::new(1, 1, Ipv4Addr::new(10, 0, 0, 7)),
                "ptime",
                Connection {
                    address: [10, 0, 0, 7],
                    ttl: None,
                },
                media.clone(),
            );
            let text = description.to_sdp().expect("render ptime");
            assert!(
                text.contains(&format!("a=ptime:{expected}\r\n")),
                "ptime {ptime_us}us 应输出 {expected}: {text}"
            );
        }
    }

    #[test]
    fn sdp_write_rejects_bad_inputs_with_stable_names() {
        let base = aes67_8ch_description();
        // 非 ASCII 会话名。
        let mut description = base.clone();
        description.session_name = "校准流".to_string();
        assert_eq!(description.to_sdp().unwrap_err(), SdpWriteError::NonAscii);
        // CR/LF 注入。
        let mut description = base.clone();
        description.session_name = "evil\r\nm=audio 0 RTP/AVP 0".to_string();
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::ControlChar
        );
        // NUL。
        let mut description = base.clone();
        description.session_name = "evil\0".to_string();
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::ControlChar
        );
        // 端口 0。
        let mut description = base.clone();
        description.media.port = 0;
        assert_eq!(description.to_sdp().unwrap_err(), SdpWriteError::BadPort);
        // 通道 0。
        let mut description = base.clone();
        description.media.channels = 0;
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::BadChannels
        );
        // 采样率 0。
        let mut description = base.clone();
        description.media.rate_hz = 0;
        assert_eq!(description.to_sdp().unwrap_err(), SdpWriteError::BadRate);
        // ptime 0 / 与 framecount 不一致。
        let mut description = base.clone();
        description.media.ptime_us = Some(0);
        assert_eq!(description.to_sdp().unwrap_err(), SdpWriteError::BadPtime);
        let mut description = base.clone();
        description.media.ptime_us = Some(2_000);
        assert_eq!(description.to_sdp().unwrap_err(), SdpWriteError::BadPtime);
        // framecount 0 / 超上限。
        let mut description = base.clone();
        description.media.framecount = Some(0);
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::BadFramecount
        );
        let mut description = base.clone();
        description.media.framecount = Some(AES67_MAX_FRAMECOUNT + 1);
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::BadFramecount
        );
        // channel-order 分组总数不符。
        let mut description = base.clone();
        description.media.channel_order = Some("SMPTE2110.(51)".to_string());
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::BadChannelOrder
        );
        // source-filter 目的地址与 c= 不一致。
        let mut description = base.clone();
        description.media.source_filter = Some(SourceFilter {
            destination: Ipv4Addr::new(239, 9, 9, 9),
            source: Ipv4Addr::new(192, 0, 2, 222),
            include: true,
        });
        assert_eq!(
            description.to_sdp().unwrap_err(),
            SdpWriteError::BadSourceFilter
        );
        // o= username 含空白。
        let mut description = base;
        description.origin.username = "bad name".to_string();
        assert_eq!(description.to_sdp().unwrap_err(), SdpWriteError::EmptyValue);
        assert_eq!(SdpWriteError::NonAscii.name(), "ERR_SDPW_NON_ASCII");
        assert_eq!(
            SdpWriteError::BadSourceFilter.name(),
            "ERR_SDPW_BAD_SOURCE_FILTER"
        );
    }
}
