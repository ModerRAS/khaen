//! SDP（RFC 4566）AES67 媒体面子集解析 + 文件输入（[NONRT]）。
//!
//! 只支持单 `m=audio` 会话（H16/RAVENNA 每条 flow 一个 SDP；多 `m=`/IPv6/非
//! RTP/AVP 一律拒收并给出稳定错误名）。解析是**只读、无堆分配以外副作用**的：
//! 文本由调用方持有，字段以 `&str` 借用（`Session<'_>`）。
//!
//! 覆盖字段：
//! - `m=audio <port> RTP/AVP <pt>`
//! - `c=IN IP4 <mcast/addr>[/ttl]`（媒体级优先，会话级兜底）
//! - `a=rtpmap:<pt> L24/48000/<ch>`（也解析 L16 供诊断；媒体 profile 校验只接受 L24）
//! - `a=ptime:<ms>`（支持小数 ms，内部换算成 µs；AES67 媒体面 ≤1 ms 由
//!   `Session::validate_media_profile` 校验）
//! - `a=ts-refclk:ptp=IEEE1588-2008:<gmid>:<domain>`（RFC 7273 子集；记录是否带
//!   grandmaster id 与 PTP domain）、RFC 7273 `a=ts-refclk:local`，以及 legacy
//!   `a=ts-refclk:localmac=<MAC>`（非 RFC 7273，仅容忍旧 NSP 发送端）
//! - `a=mediaclk:direct=<n>` 或 `a=mediaclk:sender`（RFC 7273；Phase 1 无 PTP 用 sender）
//!
//! 字段语义与旧实现的 `parse_aes67_sdp` 对齐，但本模块独立成 crate（接收端自带解析，
//! 不依赖其它 crate，也不实现端点/daemon）。
//!
//! 本模块同时补齐 SAP 通告所需的解析字段与序列化互逆字段：
//! - `o=`（RFC 8866 §5.2）：会话源地址/版本；SAP 通告要求 `o=` 地址与 SAP 头的
//!   originating source 一致（见 [`crate::sap`]）。
//! - `a=framecount:<n>`（ST 2110-30/RAVENNA 扩展；AES67 本身不定义）。
//! - `a=fmtp:<pt> channel-order=...`（ST 2110-30；本子集只校验数字分组总数）。
//! - `a=source-filter: incl|excl IN IP4 <dest> <src>`（RFC 4570 子集，单源）。
//! - `a=sendonly` / `a=recvonly` / `a=sendrecv` / `a=inactive`（RFC 8866 §6）。
//!
//! SDP 序列化（生成）在 [`crate::sdp_write`]：本模块只解析，`sdp_write` 只生成，
//! 两者由单测做写-读互逆（round-trip）核验。

use std::net::Ipv4Addr;

/// AES67 媒体面采样率（H16/RAVENNA 固定 48k）。
pub const AES67_RATE_HZ: u32 = 48_000;
/// AES67 媒体面单 flow 最大通道数。
pub const AES67_MAX_CHANNELS_PER_FLOW: u16 = 8;
/// AES67 媒体面 ptime 上限（µs）：≤1 ms（AES67 media profile）。
pub const AES67_PTIME_MAX_US: u32 = 1_000;
/// 单包媒体帧数上限（`a=framecount`；4 ms @48k = 192 帧）。
pub const AES67_MAX_FRAMECOUNT: u32 = 192;

/// 线上线性编码（L24 = 24-bit packed big-endian，AES67 媒体面必须；L16 = 诊断兼容）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// 24-bit linear packed（RFC 3190；网络字节序 = big-endian）。
    L24,
    /// 16-bit linear（RFC 3551）。
    L16,
}

impl Encoding {
    /// SDP 编码名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Encoding::L24 => "L24",
            Encoding::L16 => "L16",
        }
    }

    /// 每样本字节数（L24=3 / L16=2）。
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Encoding::L24 => 3,
            Encoding::L16 => 2,
        }
    }
}

/// SDP `c=` 连接描述（IPv4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Connection {
    /// IPv4 地址（组播 224/4 或单播；是否允许单播由接收端策略决定）。
    pub address: [u8; 4],
    /// `/ttl` 后缀（IPv4 组播 TTL；单播地址通常也带 /32）。
    pub ttl: Option<u8>,
}

impl Connection {
    /// 连接地址的 `Ipv4Addr` 视图。
    pub const fn ipv4(self) -> Ipv4Addr {
        Ipv4Addr::new(
            self.address[0],
            self.address[1],
            self.address[2],
            self.address[3],
        )
    }

    /// 是否 IPv4 组播地址（224.0.0.0/4）。
    pub const fn is_multicast(self) -> bool {
        is_multicast_ipv4(self.ipv4())
    }
}

/// 是否 IPv4 组播地址（224.0.0.0/4）。
pub const fn is_multicast_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] >= 224 && octets[0] <= 239
}

/// SDP 媒体方向（RFC 8866 §6 `a=sendonly` 等；缺省 `sendrecv`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// `a=sendrecv`（缺省：既可发也可收）。
    SendRecv,
    /// `a=sendonly`（发送端声明；接收端可收这条流）。
    SendOnly,
    /// `a=recvonly`（该媒体只收；本接收端不能把它当媒体源）。
    RecvOnly,
    /// `a=inactive`（无媒体）。
    Inactive,
}

impl Direction {
    /// 稳定名称（日志/证据 JSON 用）。
    pub const fn name(self) -> &'static str {
        match self {
            Direction::SendRecv => "sendrecv",
            Direction::SendOnly => "sendonly",
            Direction::RecvOnly => "recvonly",
            Direction::Inactive => "inactive",
        }
    }

    /// 对应 SDP 属性行；`sendrecv` 是缺省值，返回 `None` 表示生成时可省略。
    pub const fn attribute(self) -> Option<&'static str> {
        match self {
            Direction::SendRecv => None,
            Direction::SendOnly => Some("a=sendonly"),
            Direction::RecvOnly => Some("a=recvonly"),
            Direction::Inactive => Some("a=inactive"),
        }
    }
}

/// `a=source-filter`（RFC 4570 子集：单源 incl/excl）。
///
/// AES67 本身不要求 source-filter；本 crate 记录 SSM 声明并在媒体 profile 校验时
/// 检查目的地址与 `c=` 一致（EXTENSION：多源过滤不在本子集）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFilter {
    /// 声明的目的地址（应与生效 `c=` 一致）。
    pub destination: Ipv4Addr,
    /// 声明的源地址（SSM 源；本接收端暂不参与源特定加入，只记录/校验）。
    pub source: Ipv4Addr,
    /// `true` = `incl`（只收该源）；`false` = `excl`（排除该源）。
    pub include: bool,
}

/// SDP `o=` 会话源（RFC 8866 §5.2）。
///
/// SAP 通告要求 `o=` 的地址与 SAP 头的 originating source 一致（本 crate 在
/// [`crate::sap`] 通告路径校验；接收端只记录，不做安全判定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin<'a> {
    /// 用户名（SDP `o=` 第 1 字段；无用户名为 `-`）。
    pub username: &'a str,
    /// 会话 id（`o=` 第 2 字段；NTP 时间戳风格，调用方生成）。
    pub session_id: u64,
    /// 会话版本（`o=` 第 3 字段；内容变化时递增）。
    pub session_version: u64,
    /// 源 IPv4 地址（`o=` 的 address 字段）。
    pub address: [u8; 4],
}

impl<'a> Origin<'a> {
    /// 源地址的 `Ipv4Addr` 视图。
    pub const fn ipv4(self) -> Ipv4Addr {
        Ipv4Addr::new(
            self.address[0],
            self.address[1],
            self.address[2],
            self.address[3],
        )
    }
}

/// `a=ts-refclk` 解析结果（RFC 7273）：PTP 参考钟、local 或 localmac。
///
/// AES67 部署通常声明 `ptp=IEEE1588-2008`；不含 PTP grandmaster 的现场源
/// 按 RFC 7273 声明 `local`（本任务 Phase 1 NSP 16ch SDP 即 `local` +
/// `mediaclk:sender`）。历史 NSP standalone 发送端声明过 `localmac=<MAC>`
/// （非 RFC 7273 语法，仅作 legacy 兼容解析）。参考钟只描述"发送端时钟来源"；
/// 接收端媒体时钟仍是进程内 `UsrvClock`（PTP 未接线时报告 not-wired）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefClk {
    /// PTP 参考钟（`a=ts-refclk:ptp=IEEE1588-2008[:<gmid>][:<domain>]`）。
    Ptp(PtpRefClk),
    /// 本地参考钟（RFC 7273：`a=ts-refclk:local`；无额外参数）。
    Local,
    /// 本地 MAC 参考钟（`a=ts-refclk:localmac=02-4E-53-50-00-01`；legacy 容忍）。
    LocalMac([u8; 6]),
}

impl RefClk {
    /// 稳定名称（证据 JSON 用）。
    pub const fn name(self) -> &'static str {
        match self {
            RefClk::Ptp(_) => "ptp",
            RefClk::Local => "local",
            RefClk::LocalMac(_) => "localmac",
        }
    }
}

/// `a=ts-refclk` 的 PTP 参考钟（RFC 7273 子集）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtpRefClk {
    /// PTP domain（AES67 默认 0）。
    pub domain: u8,
    /// 是否带 8 字节 grandmaster id（EUI-64）。
    pub has_grandmaster: bool,
    /// grandmaster id（`xx-xx-...` 8 组；无则全 0）。
    pub grandmaster: [u8; 8],
}

/// `a=mediaclk` 的媒体时钟描述（RFC 7273；本原型只记录 direct）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaClk {
    /// `direct=<offset>`（参考钟直接映射；AES67 常用 direct=0）。
    pub direct: bool,
    /// direct offset（秒；仅记录）。
    pub offset_sec: u32,
}

/// `a=rtpmap` 的编码描述。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rtpmap {
    /// 编码（L24/L16）。
    pub encoding: Encoding,
    /// RTP 时钟率（AES67 48k）。
    pub rate_hz: u32,
    /// 每包通道数（rtpmap 省略时按 RFC 3551 默认 1）。
    pub channels: u16,
}

/// 一个 `m=audio` 媒体描述（单 flow）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaDescription<'a> {
    /// 媒体类型（本子集恒为 `"audio"`）。
    pub media: &'a str,
    /// RTP 端口（`m=` 的 <port>；`port/number` 取斜杠前）。
    pub port: u16,
    /// 传输协议（本子集恒为 `"RTP/AVP"`）。
    pub protocol: &'a str,
    /// 动态 payload type（AES67 要求 96..=127；解析保留原值）。
    pub payload_type: u8,
    /// rtpmap 描述。
    pub rtpmap: Rtpmap,
    /// `a=ptime`（µs；缺省 None）。
    pub ptime_us: Option<u32>,
    /// `a=ts-refclk`（AES67 媒体面必需；解析可选：`ptp=` / `local` / legacy `localmac=`）。
    pub refclk: Option<RefClk>,
    /// `a=mediaclk:...`（可选）。
    pub mediaclk: Option<MediaClk>,
    /// `a=framecount:<n>`（ST 2110-30/RAVENNA 扩展；AES67 本身不定义每包帧数字段）。
    pub framecount: Option<u32>,
    /// `a=fmtp:<pt> channel-order=...` 的原始值（ST 2110-30；本子集只校验数字分组总数）。
    pub channel_order: Option<&'a str>,
    /// `a=source-filter: incl|excl IN IP4 <dest> <src>`（RFC 4570 子集，单源）。
    pub source_filter: Option<SourceFilter>,
    /// 媒体方向（缺省 `sendrecv`；RFC 8866 §6）。
    pub direction: Direction,
    /// 媒体级 `c=`（优先于会话级）。
    pub connection: Option<Connection>,
}

/// 单会话 SDP 解析结果（字段借用输入文本）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session<'a> {
    /// `o=` 会话源（可选；SAP 通告要求 `o=` 地址与 originating source 一致）。
    pub origin: Option<Origin<'a>>,
    /// `s=` 会话名（可选，只记录不解释布局）。
    pub name: Option<&'a str>,
    /// 会话级 `c=`（媒体级缺省时兜底）。
    pub session_connection: Option<Connection>,
    /// 唯一 `m=audio` 媒体描述。
    pub media: MediaDescription<'a>,
}

impl<'a> Session<'a> {
    /// 生效连接：媒体级优先，会话级兜底。
    pub fn connection(&self) -> Option<Connection> {
        self.media.connection.or(self.session_connection)
    }

    /// 媒体方向（缺省 `sendrecv`）。
    pub const fn direction(&self) -> Direction {
        self.media.direction
    }

    /// SDP `o=` 源地址（若有）。
    pub fn origin_address(&self) -> Option<Ipv4Addr> {
        self.origin.map(Origin::ipv4)
    }

    /// 媒体面 profile 校验（AES67 接收端离线原型契约）。
    ///
    /// 规则：48k、每 flow 通道 1..=8、L24、动态 payload 96..=127、ptime ≤1 ms、
    /// 必须带 `a=ts-refclk`（不猜时钟）、必须有 `c=` 且端口非 0。
    /// **不校验**地址是组播还是单播：单播仅用于离线 loopback 测试，生产策略由
    /// `receiver::ReceiverConfig` 决定（组播 profile 由部署侧 SDP+配置保证）。
    /// 媒体面 profile 校验（AES67 接收端离线原型契约）。
    ///
    /// 规则：48k、每 flow 通道 1..=8、L24、动态 payload 96..=127、ptime ≤1 ms、
    /// 方向不是 `recvonly`/`inactive`、`framecount` 与 ptime 一致（±1 帧）、
    /// `channel-order` 数字分组总数等于通道数、`source-filter` 目的地址与 `c=`
    /// 一致，必须带 `a=ts-refclk`（不猜时钟）、必须有 `c=` 且端口非 0。
    /// **不校验**地址是组播还是单播：单播仅用于离线 loopback 测试，生产策略由
    /// `receiver::ReceiverConfig` 决定（组播 profile 由部署侧 SDP+配置保证）。
    pub fn validate_media_profile(&self) -> Result<(), SdpError> {
        let media = &self.media;
        if media.rtpmap.rate_hz != AES67_RATE_HZ {
            return Err(SdpError::UnsupportedRate);
        }
        if media.rtpmap.channels == 0 || media.rtpmap.channels > AES67_MAX_CHANNELS_PER_FLOW {
            return Err(SdpError::BadChannels);
        }
        if media.rtpmap.encoding != Encoding::L24 {
            return Err(SdpError::UnsupportedEncoding);
        }
        if media.direction == Direction::RecvOnly || media.direction == Direction::Inactive {
            return Err(SdpError::BadDirection);
        }
        if let Some(framecount) = media.framecount {
            if framecount == 0 || framecount > AES67_MAX_FRAMECOUNT {
                return Err(SdpError::BadFramecount);
            }
            if let Some(ptime_us) = media.ptime_us {
                // ptime（µs）↔ framecount（帧）一致性：允许 ±1 帧量化（44.1k 1.09 ms = 48 帧）。
                let expected_us = framecount as u64 * 1_000_000 / AES67_RATE_HZ as u64;
                let tolerance = 1_000_000 / AES67_RATE_HZ as u64;
                let actual = ptime_us as u64;
                if actual + tolerance < expected_us || actual > expected_us + tolerance {
                    return Err(SdpError::BadFramecount);
                }
            }
        }
        if let Some(order) = media.channel_order {
            let groups = parse_channel_order(order)?;
            let total: u32 = groups.iter().map(|n| *n as u32).sum();
            if total != media.rtpmap.channels as u32 {
                return Err(SdpError::BadChannelOrder);
            }
        }
        if let Some(filter) = media.source_filter {
            if let Some(connection) = self.connection() {
                if filter.destination != connection.ipv4() {
                    return Err(SdpError::BadSourceFilter);
                }
            }
        }
        if media.payload_type < 96 {
            return Err(SdpError::UnsupportedPayload);
        }
        if media.port == 0 {
            return Err(SdpError::BadMedia);
        }
        if let Some(ptime) = media.ptime_us {
            if ptime == 0 || ptime > AES67_PTIME_MAX_US {
                return Err(SdpError::BadPtime);
            }
        }
        if media.refclk.is_none() {
            return Err(SdpError::MissingRefClk);
        }
        if self.connection().is_none() {
            return Err(SdpError::MissingConnection);
        }
        Ok(())
    }

    /// 单包媒体帧数上限（ptime µs × 48k；缺省 ptime 按 1 ms = 48 帧）。
    pub fn max_packet_frames(&self) -> usize {
        let ptime = self.media.ptime_us.unwrap_or(1_000) as u64;
        ((ptime * AES67_RATE_HZ as u64) / 1_000_000) as usize
    }
}

/// SDP 解析/媒体 profile 校验错误（Copy + 稳定错误名；不分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpError {
    /// 空文本。
    Empty,
    /// 语法畸形（缺 v=/m=、字段重复、未知 c= 格式等）。
    Malformed,
    /// `v=` 不是 0。
    UnsupportedVersion,
    /// 没有 `m=`。
    MissingMedia,
    /// 多于一个 `m=`（多会话不是单 flow SDP）。
    MultipleMedia,
    /// 非 audio 媒体或非 RTP/AVP 协议。
    UnsupportedMedia,
    /// `m=` 格式/端口非法。
    BadMedia,
    /// `c=` 非法（非 IPv4/缺地址/坏 TTL）。
    BadConnection,
    /// 缺生效 `c=`。
    MissingConnection,
    /// `a=rtpmap` 缺失/非法/与 m= payload 不匹配。
    BadRtpmap,
    /// `a=ptime` 非法。
    BadPtime,
    /// `a=framecount` 非法，或与 ptime 不一致。
    BadFramecount,
    /// `a=fmtp: channel-order` 非法，或分组总数与通道数不符。
    BadChannelOrder,
    /// `a=source-filter` 非法，或目的地址与 `c=` 不一致。
    BadSourceFilter,
    /// 媒体方向不可接受（`recvonly`/`inactive` 不能作为接收源）。
    BadDirection,
    /// `a=ts-refclk` 非法（非 PTP/IEEE1588 或格式错误）。
    BadRefClk,
    /// 缺 AES67 媒体面必需的 `a=ts-refclk`。
    MissingRefClk,
    /// 编码不是 L24（AES67 媒体面必须；L16 只作解析诊断）。
    UnsupportedEncoding,
    /// 采样率不是 48k。
    UnsupportedRate,
    /// 每 flow 通道数 <1 或 >8。
    BadChannels,
    /// 静态/保留 payload type（AES67 要求动态 96..=127）。
    UnsupportedPayload,
    /// 读取 SDP 文件失败（I/O 层）。
    Io,
}

impl SdpError {
    /// 稳定错误名（证据/日志/JSON 用；RT 层不格式化）。
    pub const fn name(self) -> &'static str {
        match self {
            SdpError::Empty => "ERR_SDP_EMPTY",
            SdpError::Malformed => "ERR_SDP_MALFORMED",
            SdpError::UnsupportedVersion => "ERR_SDP_VERSION",
            SdpError::MissingMedia => "ERR_SDP_NO_MEDIA",
            SdpError::MultipleMedia => "ERR_SDP_MULTI_MEDIA",
            SdpError::UnsupportedMedia => "ERR_SDP_UNSUPPORTED_MEDIA",
            SdpError::BadMedia => "ERR_SDP_BAD_MEDIA",
            SdpError::BadConnection => "ERR_SDP_BAD_CONNECTION",
            SdpError::MissingConnection => "ERR_SDP_NO_CONNECTION",
            SdpError::BadRtpmap => "ERR_SDP_BAD_RTPMAP",
            SdpError::BadPtime => "ERR_SDP_BAD_PTIME",
            SdpError::BadFramecount => "ERR_SDP_BAD_FRAMECOUNT",
            SdpError::BadChannelOrder => "ERR_SDP_BAD_CHANNEL_ORDER",
            SdpError::BadSourceFilter => "ERR_SDP_BAD_SOURCE_FILTER",
            SdpError::BadDirection => "ERR_SDP_BAD_DIRECTION",
            SdpError::BadRefClk => "ERR_SDP_BAD_REFCLK",
            SdpError::MissingRefClk => "ERR_SDP_NO_REFCLK",
            SdpError::UnsupportedEncoding => "ERR_SDP_UNSUPPORTED_ENCODING",
            SdpError::UnsupportedRate => "ERR_SDP_UNSUPPORTED_RATE",
            SdpError::BadChannels => "ERR_SDP_BAD_CHANNELS",
            SdpError::UnsupportedPayload => "ERR_SDP_UNSUPPORTED_PAYLOAD",
            SdpError::Io => "ERR_SDP_IO",
        }
    }
}

/// 拥有的 SDP 文本（文件输入；解析结果借用其内容）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdpDocument {
    text: String,
    source: Option<String>,
}

impl SdpDocument {
    /// 由已读入文本构造。
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source: None,
        }
    }

    /// 从文件读入（启动期 NONRT I/O；块循环内禁止调用）。
    pub fn from_file(path: &str) -> Result<Self, SdpError> {
        let text = std::fs::read_to_string(path).map_err(|_| SdpError::Io)?;
        Ok(Self {
            text,
            source: Some(path.to_string()),
        })
    }

    /// 原始文本。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 文本来源（文件路径；内存构造为 None）。
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// 解析为借用会话。
    pub fn parse(&self) -> Result<Session<'_>, SdpError> {
        parse_sdp(&self.text)
    }

    /// 解析 + AES67 媒体 profile 校验。
    pub fn parse_validated(&self) -> Result<Session<'_>, SdpError> {
        let session = self.parse()?;
        session.validate_media_profile()?;
        Ok(session)
    }
}

/// 解析单会话 SDP 文本（见模块头覆盖字段）。
pub fn parse_sdp(text: &str) -> Result<Session<'_>, SdpError> {
    if text.trim().is_empty() {
        return Err(SdpError::Empty);
    }
    let mut version_seen = false;
    let mut name: Option<&str> = None;
    let mut origin: Option<Origin<'_>> = None;
    let mut session_connection: Option<Connection> = None;
    let mut media_seen = false;
    let mut port = 0u16;
    let mut payload_type = 0u8;
    let mut rtpmap: Option<(u8, Rtpmap)> = None;
    let mut ptime_us: Option<u32> = None;
    let mut refclk: Option<RefClk> = None;
    let mut mediaclk: Option<MediaClk> = None;
    let mut framecount: Option<u32> = None;
    let mut channel_order: Option<&str> = None;
    let mut source_filter: Option<SourceFilter> = None;
    let mut direction = Direction::SendRecv;
    let mut direction_seen = false;
    let mut media_connection: Option<Connection> = None;
    let mut media_kind: &str = "";
    let mut protocol: &str = "";

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("v=") {
            if version_seen {
                return Err(SdpError::Malformed);
            }
            if value.trim() != "0" {
                return Err(SdpError::UnsupportedVersion);
            }
            version_seen = true;
        } else if let Some(value) = line.strip_prefix("o=") {
            if origin.is_some() {
                return Err(SdpError::Malformed);
            }
            origin = Some(parse_origin(value)?);
        } else if let Some(value) = line.strip_prefix("s=") {
            if name.is_none() {
                name = Some(value.trim());
            }
        } else if let Some(value) = line.strip_prefix("c=") {
            let parsed = parse_connection(value)?;
            if media_seen {
                if media_connection.is_some() {
                    return Err(SdpError::Malformed);
                }
                media_connection = Some(parsed);
            } else {
                if session_connection.is_some() {
                    return Err(SdpError::Malformed);
                }
                session_connection = Some(parsed);
            }
        } else if let Some(value) = line.strip_prefix("m=") {
            if media_seen {
                return Err(SdpError::MultipleMedia);
            }
            let parsed = parse_media(value)?;
            port = parsed.0;
            protocol = parsed.1;
            payload_type = parsed.2;
            media_kind = parsed.3;
            media_seen = true;
        } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
            let parsed = parse_rtpmap(value)?;
            if !media_seen {
                return Err(SdpError::Malformed);
            }
            if parsed.0 == payload_type {
                if rtpmap.is_some() {
                    return Err(SdpError::Malformed);
                }
                rtpmap = Some(parsed);
            }
        } else if let Some(value) = line.strip_prefix("a=ptime:") {
            if ptime_us.is_some() {
                return Err(SdpError::Malformed);
            }
            ptime_us = Some(parse_ptime_us(value.trim())?);
        } else if let Some(value) = line.strip_prefix("a=ts-refclk:") {
            if refclk.is_some() {
                return Err(SdpError::Malformed);
            }
            refclk = Some(parse_ts_refclk(value.trim())?);
        } else if let Some(value) = line.strip_prefix("a=mediaclk:") {
            if mediaclk.is_some() {
                return Err(SdpError::Malformed);
            }
            mediaclk = Some(parse_mediaclk(value.trim())?);
        } else if let Some(value) = line.strip_prefix("a=framecount:") {
            if framecount.is_some() {
                return Err(SdpError::Malformed);
            }
            framecount = Some(parse_framecount(value.trim())?);
        } else if let Some(value) = line.strip_prefix("a=fmtp:") {
            if !media_seen {
                return Err(SdpError::Malformed);
            }
            if let Some(order) = parse_fmtp_channel_order(value, payload_type)? {
                if channel_order.is_some() {
                    return Err(SdpError::Malformed);
                }
                channel_order = Some(order);
            }
        } else if let Some(value) = line.strip_prefix("a=source-filter:") {
            if source_filter.is_some() {
                return Err(SdpError::Malformed);
            }
            source_filter = Some(parse_source_filter(value.trim())?);
        } else if line == "a=sendonly"
            || line == "a=recvonly"
            || line == "a=sendrecv"
            || line == "a=inactive"
        {
            if direction_seen {
                return Err(SdpError::Malformed);
            }
            direction_seen = true;
            direction = match line {
                "a=sendonly" => Direction::SendOnly,
                "a=recvonly" => Direction::RecvOnly,
                "a=inactive" => Direction::Inactive,
                _ => Direction::SendRecv,
            };
        }
    }

    if !version_seen || !media_seen {
        return Err(if media_seen {
            SdpError::Malformed
        } else {
            SdpError::MissingMedia
        });
    }
    let (_, rtpmap) = rtpmap.ok_or(SdpError::BadRtpmap)?;
    if media_kind != "audio" || protocol != "RTP/AVP" {
        return Err(SdpError::UnsupportedMedia);
    }
    let media = MediaDescription {
        media: media_kind,
        port,
        protocol,
        payload_type,
        rtpmap,
        ptime_us,
        refclk,
        mediaclk,
        framecount,
        channel_order,
        source_filter,
        direction,
        connection: media_connection,
    };
    Ok(Session {
        origin,
        name,
        session_connection,
        media,
    })
}

/// 解析 `c=IN IP4 <addr>[/ttl]`。
fn parse_connection(value: &str) -> Result<Connection, SdpError> {
    let mut parts = value.split_whitespace();
    let net_type = parts.next().ok_or(SdpError::BadConnection)?;
    let addr_type = parts.next().ok_or(SdpError::BadConnection)?;
    let address = parts.next().ok_or(SdpError::BadConnection)?;
    if parts.next().is_some() {
        return Err(SdpError::BadConnection);
    }
    if net_type != "IN" || addr_type != "IP4" {
        return Err(SdpError::BadConnection);
    }
    let (ip_text, ttl) = match address.split_once('/') {
        Some((ip, ttl_text)) => {
            let ttl: u8 = ttl_text.parse().map_err(|_| SdpError::BadConnection)?;
            (ip, Some(ttl))
        }
        None => (address, None),
    };
    let ip: Ipv4Addr = ip_text.parse().map_err(|_| SdpError::BadConnection)?;
    Ok(Connection {
        address: ip.octets(),
        ttl,
    })
}

/// 解析 `m=audio <port> RTP/AVP <pt>`（返回 media/ port/ protocol/ payload）。
fn parse_media(value: &str) -> Result<(u16, &str, u8, &str), SdpError> {
    let mut parts = value.split_whitespace();
    let media = parts.next().ok_or(SdpError::BadMedia)?;
    let port_text = parts.next().ok_or(SdpError::BadMedia)?;
    let protocol = parts.next().ok_or(SdpError::BadMedia)?;
    let payload_text = parts.next().ok_or(SdpError::BadMedia)?;
    if parts.next().is_some() {
        // 多 payload type 需要 payload 集合/rtpmap 多值语义；本子集只支持单 flow 单 pt。
        return Err(SdpError::UnsupportedPayload);
    }
    let port_text = match port_text.split_once('/') {
        Some((port, _count)) => port,
        None => port_text,
    };
    let port: u16 = port_text.parse().map_err(|_| SdpError::BadMedia)?;
    let payload_type: u8 = payload_text.parse().map_err(|_| SdpError::BadMedia)?;
    Ok((port, protocol, payload_type, media))
}

/// 解析 `a=rtpmap:<pt> <encoding>/<rate>[/<channels>]`。
fn parse_rtpmap(value: &str) -> Result<(u8, Rtpmap), SdpError> {
    let (pt_text, rest) = value.split_once(' ').ok_or(SdpError::BadRtpmap)?;
    let payload: u8 = pt_text.trim().parse().map_err(|_| SdpError::BadRtpmap)?;
    let rest = rest.trim();
    let mut tokens = rest.split('/');
    let encoding_name = tokens.next().ok_or(SdpError::BadRtpmap)?;
    let rate_text = tokens.next().ok_or(SdpError::BadRtpmap)?;
    let channels_text = tokens.next();
    let encoding = match encoding_name {
        "L24" => Encoding::L24,
        "L16" => Encoding::L16,
        _ => return Err(SdpError::UnsupportedEncoding),
    };
    let rate_hz: u32 = rate_text.parse().map_err(|_| SdpError::BadRtpmap)?;
    let channels: u16 = match channels_text {
        Some(text) => text.parse().map_err(|_| SdpError::BadRtpmap)?,
        None => 1,
    };
    Ok((
        payload,
        Rtpmap {
            encoding,
            rate_hz,
            channels,
        },
    ))
}

/// 解析 `a=ptime:<ms>`（整数或小数 ms；换算成 µs）。
fn parse_ptime_us(value: &str) -> Result<u32, SdpError> {
    if value.is_empty() {
        return Err(SdpError::BadPtime);
    }
    let (whole_text, frac_text) = match value.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (value, None),
    };
    if whole_text.is_empty() || !whole_text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SdpError::BadPtime);
    }
    let whole_ms: u32 = whole_text.parse().map_err(|_| SdpError::BadPtime)?;
    let mut frac_us = 0u32;
    if let Some(frac) = frac_text {
        if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SdpError::BadPtime);
        }
        let frac_value: u32 = frac.parse().map_err(|_| SdpError::BadPtime)?;
        frac_us = frac_value * 10u32.pow(3 - frac.len() as u32);
    }
    let micros = whole_ms
        .checked_mul(1_000)
        .and_then(|v| v.checked_add(frac_us))
        .ok_or(SdpError::BadPtime)?;
    if micros == 0 {
        return Err(SdpError::BadPtime);
    }
    Ok(micros)
}

/// 解析 `a=ts-refclk:`（RFC 7273 子集）：`ptp=IEEE1588-2008[:<gmid>][:<domain>]`、
/// RFC 7273 的 `local`，或 legacy `localmac=<6×2 hex，用 '-' 分隔>`。
fn parse_ts_refclk(value: &str) -> Result<RefClk, SdpError> {
    if value == "local" {
        return Ok(RefClk::Local);
    }
    if let Some(mac) = value.strip_prefix("localmac=") {
        return Ok(RefClk::LocalMac(parse_localmac(mac)?));
    }
    let rest = value.strip_prefix("ptp=").ok_or(SdpError::BadRefClk)?;
    let mut parts = rest.split(':');
    let version = parts.next().ok_or(SdpError::BadRefClk)?;
    if version != "IEEE1588-2008" && version != "IEEE1588-2019" {
        return Err(SdpError::BadRefClk);
    }
    let mut refclk = PtpRefClk {
        domain: 0,
        has_grandmaster: false,
        grandmaster: [0u8; 8],
    };
    for part in parts {
        if part.is_empty() {
            return Err(SdpError::BadRefClk);
        }
        if part.contains('-') {
            if refclk.has_grandmaster {
                return Err(SdpError::BadRefClk);
            }
            refclk.grandmaster = parse_grandmaster(part)?;
            refclk.has_grandmaster = true;
        } else {
            refclk.domain = part.parse().map_err(|_| SdpError::BadRefClk)?;
        }
    }
    Ok(RefClk::Ptp(refclk))
}

/// 解析 `localmac` 的 6 组 MAC 字节（`02-4E-53-50-00-01`）。
fn parse_localmac(text: &str) -> Result<[u8; 6], SdpError> {
    let mut mac = [0u8; 6];
    let mut count = 0usize;
    for group in text.split('-') {
        if count >= 6 || group.len() != 2 {
            return Err(SdpError::BadRefClk);
        }
        mac[count] = u8::from_str_radix(group, 16).map_err(|_| SdpError::BadRefClk)?;
        count += 1;
    }
    if count != 6 {
        return Err(SdpError::BadRefClk);
    }
    Ok(mac)
}

/// 解析 EUI-64 形式的 grandmaster id（8 组 2 位十六进制）。
fn parse_grandmaster(text: &str) -> Result<[u8; 8], SdpError> {
    let mut grandmaster = [0u8; 8];
    let mut count = 0usize;
    for group in text.split('-') {
        if count >= 8 || group.len() != 2 {
            return Err(SdpError::BadRefClk);
        }
        grandmaster[count] = u8::from_str_radix(group, 16).map_err(|_| SdpError::BadRefClk)?;
        count += 1;
    }
    if count != 8 {
        return Err(SdpError::BadRefClk);
    }
    Ok(grandmaster)
}

/// 解析 `a=mediaclk:direct=<offset>|sender`（RFC 7273；只记录 direct）。
fn parse_mediaclk(value: &str) -> Result<MediaClk, SdpError> {
    if let Some(offset) = value.strip_prefix("direct=") {
        let offset_sec: u32 = offset.parse().map_err(|_| SdpError::Malformed)?;
        return Ok(MediaClk {
            direct: true,
            offset_sec,
        });
    }
    if value == "sender" {
        return Ok(MediaClk {
            direct: false,
            offset_sec: 0,
        });
    }
    Err(SdpError::Malformed)
}

/// 解析 `o=` 会话源（RFC 8866 §5.2；本子集只接受 IPv4）。
fn parse_origin(value: &str) -> Result<Origin<'_>, SdpError> {
    let mut parts = value.split_whitespace();
    let username = parts.next().ok_or(SdpError::Malformed)?;
    let session_id = parts
        .next()
        .ok_or(SdpError::Malformed)?
        .parse::<u64>()
        .map_err(|_| SdpError::Malformed)?;
    let session_version = parts
        .next()
        .ok_or(SdpError::Malformed)?
        .parse::<u64>()
        .map_err(|_| SdpError::Malformed)?;
    let net_type = parts.next().ok_or(SdpError::Malformed)?;
    let addr_type = parts.next().ok_or(SdpError::Malformed)?;
    let address = parts.next().ok_or(SdpError::Malformed)?;
    if username.is_empty() || parts.next().is_some() || net_type != "IN" || addr_type != "IP4" {
        return Err(SdpError::Malformed);
    }
    let address: Ipv4Addr = address.parse().map_err(|_| SdpError::Malformed)?;
    Ok(Origin {
        username,
        session_id,
        session_version,
        address: address.octets(),
    })
}

/// 解析 `a=framecount:<n>`（ST 2110-30/RAVENNA 扩展；AES67 本身不定义该属性）。
fn parse_framecount(value: &str) -> Result<u32, SdpError> {
    let frames: u32 = value.parse().map_err(|_| SdpError::BadFramecount)?;
    if frames == 0 || frames > AES67_MAX_FRAMECOUNT {
        return Err(SdpError::BadFramecount);
    }
    Ok(frames)
}

/// 解析 `a=fmtp:<pt> ...` 里的 `channel-order=SMPTE2110.(...)` 参数（只取该参数）。
///
/// 其它 fmtp 参数容忍忽略（RFC 8866 §6：未知属性/参数忽略）；payload type 与
/// `m=` 不一致时也忽略（不属于本媒体描述）。
fn parse_fmtp_channel_order(value: &str, payload_type: u8) -> Result<Option<&str>, SdpError> {
    let (pt_text, params) = match value.split_once(' ') {
        Some((pt_text, params)) => (pt_text, params),
        None => return Ok(None),
    };
    let payload: u8 = pt_text.trim().parse().map_err(|_| SdpError::Malformed)?;
    if payload != payload_type {
        return Ok(None);
    }
    for param in params.split([';', ' ']) {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }
        if let Some(order) = param.strip_prefix("channel-order=") {
            parse_channel_order(order)?;
            return Ok(Some(order));
        }
    }
    Ok(None)
}

/// 解析 `SMPTE2110.(...)` channel-order 值并返回各通道组大小（ST 2110-30 Table 1 子集）。
///
/// 只支持数字分组 `SMPTE2110.(d,d,...)` 与 ST 2110-30 的符号组；分组总数与通道数一致
/// 由 [`Session::validate_media_profile`] 校验（EXTENSION：符号组按 ST 2110-30 表映射）。
pub fn parse_channel_order(value: &str) -> Result<Vec<u16>, SdpError> {
    let rest = value
        .strip_prefix("SMPTE2110.")
        .ok_or(SdpError::BadChannelOrder)?;
    let inner = rest
        .strip_prefix('(')
        .and_then(|text| text.strip_suffix(')'))
        .ok_or(SdpError::BadChannelOrder)?;
    if inner.is_empty() {
        return Err(SdpError::BadChannelOrder);
    }
    let mut groups = Vec::new();
    for symbol in inner.split(',') {
        let group = channel_group_channels(symbol).ok_or(SdpError::BadChannelOrder)?;
        if group == 0 || group > 64 {
            return Err(SdpError::BadChannelOrder);
        }
        groups.push(group);
    }
    if groups.len() > 16 || groups.iter().map(|n| *n as u32).sum::<u32>() > 64 {
        return Err(SdpError::BadChannelOrder);
    }
    Ok(groups)
}

/// ST 2110-30 Table 1 符号 → 通道数（数字分组与 `U##` 扩展见 [parse_channel_order]）。
fn channel_group_channels(symbol: &str) -> Option<u16> {
    match symbol {
        "M" => Some(1),
        "DM" | "ST" | "LtRt" => Some(2),
        "51" => Some(6),
        "71" => Some(8),
        "222" => Some(24),
        "SGRP" => Some(4),
        _ => {
            if let Some(digits) = symbol.strip_prefix('U') {
                if digits.len() == 2 && digits.bytes().all(|b| b.is_ascii_digit()) {
                    return digits.parse().ok();
                }
                return None;
            }
            let bytes = symbol.as_bytes();
            // EXTENSION：表外两位数字按 x.y 布局的通道数和（41→5）。
            if bytes.len() == 2 && bytes.iter().all(|b| b.is_ascii_digit()) {
                return Some((bytes[0] - b'0') as u16 + (bytes[1] - b'0') as u16);
            }
            // EXTENSION：单数字缩写（现场 NSP `(41,2)` 的 2 = 双声道）。
            if bytes.len() == 1 && bytes[0].is_ascii_digit() {
                return Some((bytes[0] - b'0') as u16);
            }
            None
        }
    }
}

/// 解析 `a=source-filter: incl|excl IN IP4 <dest> <src>`（RFC 4570 子集：单源）。
fn parse_source_filter(value: &str) -> Result<SourceFilter, SdpError> {
    let mut parts = value.split_whitespace();
    let include = match parts.next().ok_or(SdpError::BadSourceFilter)? {
        "incl" => true,
        "excl" => false,
        _ => return Err(SdpError::BadSourceFilter),
    };
    let net_type = parts.next().ok_or(SdpError::BadSourceFilter)?;
    let addr_type = parts.next().ok_or(SdpError::BadSourceFilter)?;
    if net_type != "IN" || addr_type != "IP4" {
        return Err(SdpError::BadSourceFilter);
    }
    let destination: Ipv4Addr = parts
        .next()
        .ok_or(SdpError::BadSourceFilter)?
        .parse()
        .map_err(|_| SdpError::BadSourceFilter)?;
    let source: Ipv4Addr = parts
        .next()
        .ok_or(SdpError::BadSourceFilter)?
        .parse()
        .map_err(|_| SdpError::BadSourceFilter)?;
    if parts.next().is_some() {
        return Err(SdpError::BadSourceFilter);
    }
    Ok(SourceFilter {
        destination,
        source,
        include,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H16 官方风格 SDP（组播 + PTPv2 + L24/48000/8 + ptime 1）。
    pub(crate) const H16_SDP: &str = "v=0\r\n\
o=- 0 0 IN IP4 127.0.0.1\r\n\
s=NuPrime-a227-5240:16\r\n\
c=IN IP4 239.69.16.146/32\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 103\r\n\
a=rtpmap:103 L24/48000/8\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n\
a=mediaclk:direct=0\r\n";

    #[test]
    fn sdp_h16_flow_parses_full_subset() {
        let session = parse_sdp(H16_SDP).expect("H16 SDP must parse");
        assert_eq!(session.name, Some("NuPrime-a227-5240:16"));
        let media = &session.media;
        assert_eq!(media.media, "audio");
        assert_eq!(media.port, 5_004);
        assert_eq!(media.protocol, "RTP/AVP");
        assert_eq!(media.payload_type, 103);
        assert_eq!(media.rtpmap.encoding, Encoding::L24);
        assert_eq!(media.rtpmap.rate_hz, 48_000);
        assert_eq!(media.rtpmap.channels, 8);
        assert_eq!(media.ptime_us, Some(1_000));
        let RefClk::Ptp(refclk) = media.refclk.expect("ts-refclk") else {
            panic!("H16 SDP must parse ptp refclk");
        };
        assert_eq!(refclk.domain, 0);
        assert!(refclk.has_grandmaster);
        assert_eq!(refclk.grandmaster[0], 0x00);
        assert_eq!(refclk.grandmaster[7], 0x77);
        assert!(media.mediaclk.expect("mediaclk").direct);
        let connection = session.connection().expect("connection");
        assert_eq!(connection.ipv4(), Ipv4Addr::new(239, 69, 16, 146));
        assert_eq!(connection.ttl, Some(32));
        assert!(connection.is_multicast());
        session.validate_media_profile().expect("AES67 profile");
        assert_eq!(session.max_packet_frames(), 48);
    }

    #[test]
    fn sdp_media_connection_overrides_session_connection() {
        let text = "v=0\r\n\
s=t\r\n\
c=IN IP4 239.0.0.1/16\r\n\
m=audio 5004 RTP/AVP 96\r\n\
c=IN IP4 127.0.0.1\r\n\
a=rtpmap:96 L24/48000/2\r\n\
a=ptime:0.25\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";
        let session = parse_sdp(text).expect("parse");
        assert_eq!(session.connection().unwrap().ipv4(), Ipv4Addr::LOCALHOST);
        assert_eq!(session.media.ptime_us, Some(250));
        assert!(!session.connection().unwrap().is_multicast());
        // 无 gmid 的 ts-refclk（只有 domain）。
        let RefClk::Ptp(refclk) = session.media.refclk.unwrap() else {
            panic!("ptp refclk expected");
        };
        assert_eq!(refclk.domain, 0);
        assert!(!refclk.has_grandmaster);
        session
            .validate_media_profile()
            .expect("unicast loopback OK");
    }

    /// NSP 4.1.2 现场 SDP（单播 + L24/48000/7 + ptime 1 + `ts-refclk:localmac`）。
    pub(crate) const NSP_412_SDP: &str = "v=0\r\n\
o=- 1314082870 1314082870 IN IP4 0.0.0.0\r\n\
s=NativeSpatialPipe AES67 output\r\n\
c=IN IP4 192.0.2.222\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/7\r\n\
a=ptime:1\r\n\
a=sendonly\r\n\
a=ts-refclk:localmac=02-4E-53-50-00-01\r\n\
a=mediaclk:direct=0\r\n\
a=fmtp:96 channel-order=SMPTE2110.(41,2)\r\n\
a=comment:layout=4.1.2\r\n";

    #[test]
    fn sdp_nsp_412_localmac_unicast_parses() {
        let session = parse_sdp(NSP_412_SDP).expect("NSP 4.1.2 SDP must parse");
        assert_eq!(session.media.rtpmap.channels, 7);
        assert_eq!(session.media.port, 5_004);
        assert_eq!(session.media.ptime_us, Some(1_000));
        let RefClk::LocalMac(mac) = session.media.refclk.expect("localmac refclk") else {
            panic!("localmac expected");
        };
        assert_eq!(mac, [0x02, 0x4E, 0x53, 0x50, 0x00, 0x01]);
        let connection = session.connection().expect("connection");
        assert_eq!(connection.ipv4(), Ipv4Addr::new(192, 0, 2, 222));
        assert!(!connection.is_multicast());
        session
            .validate_media_profile()
            .expect("localmac must satisfy AES67 profile gate");
        assert_eq!(session.media.refclk.unwrap().name(), "localmac");
    }

    /// NSP 16ch Phase 1 每 flow SDP（单播 + L24/48000/8 + ptime 1 + RFC 7273
    /// `ts-refclk:local` + `mediaclk:sender`；一个 SDP 文件里两条 flow 各一段 `v=0`）。
    pub(crate) const NSP_16CH_FLOW_SDP: &str = "v=0\r\n\
o=- 1 1 IN IP4 0.0.0.0\r\n\
s=NativeSpatialPipe AES67 output flow0\r\n\
c=IN IP4 192.0.2.222\r\n\
t=0 0\r\n\
m=audio 5010 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/8\r\n\
a=ptime:1\r\n\
a=sendonly\r\n\
a=ts-refclk:local\r\n\
a=mediaclk:sender\r\n\
a=comment:flow=0\r\n\
a=comment:channel-offset=0\r\n";

    #[test]
    fn sdp_local_refclk_and_sender_mediaclk_parses() {
        let session = parse_sdp(NSP_16CH_FLOW_SDP).expect("NSP 16ch flow SDP must parse");
        assert_eq!(session.media.rtpmap.channels, 8);
        assert_eq!(session.media.port, 5_010);
        let refclk = session.media.refclk.expect("ts-refclk:local");
        assert_eq!(refclk, RefClk::Local);
        assert_eq!(refclk.name(), "local");
        let mediaclk = session.media.mediaclk.expect("mediaclk");
        assert!(!mediaclk.direct, "mediaclk:sender 不是 direct 媒体钟");
        session
            .validate_media_profile()
            .expect("local + sender 必须通过 Phase 1 媒体 profile 门（PTP 未接线由报告标注）");
        assert_eq!(session.max_packet_frames(), 48);

        // local + direct=0 也解析（语法合法；Phase 1 新流首选 sender）。
        let local_direct = NSP_16CH_FLOW_SDP.replace("a=mediaclk:sender", "a=mediaclk:direct=0");
        let session = parse_sdp(&local_direct).expect("local + direct=0 parses");
        assert_eq!(session.media.refclk, Some(RefClk::Local));
        assert!(session.media.mediaclk.expect("mediaclk").direct);
        session
            .validate_media_profile()
            .expect("local + direct=0 profile");
    }

    #[test]
    fn sdp_rejects_malformed_localmac_refclk() {
        let text = "v=0\r\n\
c=IN IP4 127.0.0.1\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n\
a=ts-refclk:localmac=02-4E-53\r\n";
        assert_eq!(parse_sdp(text).unwrap_err(), SdpError::BadRefClk);
        // 没有 ts-refclk 仍然被 profile 门拒绝。
        let none = "v=0\r\n\
c=IN IP4 127.0.0.1\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n";
        assert_eq!(
            parse_sdp(none)
                .unwrap()
                .validate_media_profile()
                .unwrap_err(),
            SdpError::MissingRefClk
        );
    }

    #[test]
    fn sdp_rejects_bad_cases_with_stable_names() {
        let cases: [(&str, SdpError); 7] = [
            ("", SdpError::Empty),
            ("v=1\r\nm=audio 5004 RTP/AVP 96\r\n", SdpError::UnsupportedVersion),
            ("v=0\r\ns=t\r\n", SdpError::MissingMedia),
            (
                "v=0\r\nm=audio 5004 RTP/AVP 96\r\nm=audio 5006 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\n",
                SdpError::MultipleMedia,
            ),
            (
                "v=0\r\nc=IN IP4 239.0.0.1/16\r\nm=video 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\n",
                SdpError::UnsupportedMedia,
            ),
            // 缺 a=rtpmap:<pt> → BadRtpmap。
            (
                "v=0\r\nc=IN IP4 239.0.0.1/16\r\nm=audio 5004 RTP/AVP 96\r\n",
                SdpError::BadRtpmap,
            ),
            // 非 IPv4 连接行 → BadConnection。
            (
                "v=0\r\nc=IN IP6 ::1\r\nm=audio 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\n",
                SdpError::BadConnection,
            ),
        ];
        for (text, expected) in cases {
            let result = parse_sdp(text);
            assert_eq!(
                result.err(),
                Some(expected),
                "must reject with {expected:?}: {text:?}"
            );
        }
        assert_eq!(SdpError::BadConnection.name(), "ERR_SDP_BAD_CONNECTION");
    }

    #[test]
    fn sdp_profile_rejects_non_l24_and_bad_ptime() {
        let base = "v=0\r\n\
c=IN IP4 239.0.0.1/16\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L16/48000/2\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";
        let session = parse_sdp(base).expect("L16 parses");
        assert_eq!(
            session.validate_media_profile().unwrap_err(),
            SdpError::UnsupportedEncoding
        );
        let bad_ptime = "v=0\r\n\
c=IN IP4 239.0.0.1/16\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n\
a=ptime:2\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";
        assert_eq!(
            parse_sdp(bad_ptime)
                .unwrap()
                .validate_media_profile()
                .unwrap_err(),
            SdpError::BadPtime
        );
        let no_refclk = "v=0\r\n\
c=IN IP4 239.0.0.1/16\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n";
        assert_eq!(
            parse_sdp(no_refclk)
                .unwrap()
                .validate_media_profile()
                .unwrap_err(),
            SdpError::MissingRefClk
        );
    }

    #[test]
    fn sdp_document_roundtrip_and_file_io() {
        let document = SdpDocument::from_text(H16_SDP);
        assert!(document.source().is_none());
        let session = document.parse_validated().expect("valid");
        assert_eq!(session.media.rtpmap.channels, 8);
        let missing = SdpDocument::from_file("Z:\\definitely-missing-aes67.sdp");
        assert_eq!(missing.unwrap_err(), SdpError::Io);
    }

    #[test]
    fn sdp_parses_origin_framecount_channel_order_source_filter_and_direction() {
        let text = "v=0\r\n\
o=nsp 1311738121 7 IN IP4 192.0.2.222\r\n\
s=flow\r\n\
c=IN IP4 239.69.1.1/32\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/8\r\n\
a=fmtp:96 channel-order=SMPTE2110.(71)\r\n\
a=source-filter: incl IN IP4 239.69.1.1 192.0.2.222\r\n\
a=sendonly\r\n\
a=ptime:1\r\n\
a=framecount:48\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n\
a=mediaclk:direct=0\r\n";
        let session = parse_sdp(text).expect("extended SDP must parse");
        let origin = session.origin.expect("o=");
        assert_eq!(origin.username, "nsp");
        assert_eq!(origin.session_id, 1_311_738_121);
        assert_eq!(origin.session_version, 7);
        assert_eq!(origin.ipv4(), Ipv4Addr::new(192, 0, 2, 222));
        assert_eq!(session.media.framecount, Some(48));
        assert_eq!(session.media.channel_order, Some("SMPTE2110.(71)"));
        let filter = session.media.source_filter.expect("source-filter");
        assert!(filter.include);
        assert_eq!(filter.destination, Ipv4Addr::new(239, 69, 1, 1));
        assert_eq!(filter.source, Ipv4Addr::new(192, 0, 2, 222));
        assert_eq!(session.media.direction, Direction::SendOnly);
        assert_eq!(session.direction(), Direction::SendOnly);
        assert_eq!(
            session.origin_address(),
            Some(Ipv4Addr::new(192, 0, 2, 222))
        );
        session
            .validate_media_profile()
            .expect("extended fields must satisfy AES67 profile");
    }

    #[test]
    fn sdp_rejects_bad_extended_fields() {
        let base = "v=0\r\n\
c=IN IP4 239.69.1.1/32\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/8\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";
        // 坏 o=（会话 id 非数字）→ Malformed。
        let bad_origin = base.replace("v=0\r\n", "v=0\r\no=- x 7 IN IP4 192.168.1.1\r\n");
        assert_eq!(parse_sdp(&bad_origin).unwrap_err(), SdpError::Malformed);
        // 重复 o= → Malformed。
        let duplicate_origin = base.replace(
            "v=0\r\n",
            "v=0\r\no=- 1 1 IN IP4 192.168.1.1\r\no=- 2 2 IN IP4 192.168.1.2\r\n",
        );
        assert_eq!(
            parse_sdp(&duplicate_origin).unwrap_err(),
            SdpError::Malformed
        );
        // framecount 0 / 超上限 → BadFramecount。
        let bad_framecount = format!("{base}a=framecount:0\r\n");
        assert_eq!(
            parse_sdp(&bad_framecount).unwrap_err(),
            SdpError::BadFramecount
        );
        let too_many_frames = format!("{base}a=framecount:193\r\n");
        assert_eq!(
            parse_sdp(&too_many_frames).unwrap_err(),
            SdpError::BadFramecount
        );
        // 重复方向属性 → Malformed。
        let duplicate_direction = format!("{base}a=sendonly\r\na=recvonly\r\n");
        assert_eq!(
            parse_sdp(&duplicate_direction).unwrap_err(),
            SdpError::Malformed
        );
        // channel-order 分组总数与通道数不符 → profile BadChannelOrder。
        let bad_order = format!("{base}a=fmtp:96 channel-order=SMPTE2110.(51)\r\n");
        assert_eq!(
            parse_sdp(&bad_order)
                .unwrap()
                .validate_media_profile()
                .unwrap_err(),
            SdpError::BadChannelOrder
        );
        // source-filter 目的地址与 c= 不一致 → profile BadSourceFilter。
        let bad_filter = format!("{base}a=source-filter: incl IN IP4 239.9.9.9 192.168.1.1\r\n");
        assert_eq!(
            parse_sdp(&bad_filter)
                .unwrap()
                .validate_media_profile()
                .unwrap_err(),
            SdpError::BadSourceFilter
        );
        // recvonly 不能作为接收源 → profile BadDirection。
        let recvonly = format!("{base}a=recvonly\r\n");
        assert_eq!(
            parse_sdp(&recvonly)
                .unwrap()
                .validate_media_profile()
                .unwrap_err(),
            SdpError::BadDirection
        );
    }
}
