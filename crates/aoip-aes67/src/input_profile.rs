//! AES67 输入描述符（SDP/SAP → 接收端可验收的输入描述）——纯逻辑层。
//!
//! 输出布局（MOTU 22ch、lane→DAC）是本地固定配置；**输入布局由远端声明决定**，接收端
//! 不能靠 env 写死 2ch/map 0,1。本模块把 `crate::sdp` 的解析结果与 `crate::sap` 的
//! 会话快照收成接收端可直接消费的 `InputProfile`：
//! group/port/PT/编码/采样率/声道数/ptime/framecount/channel-order/refclk/mediaclk。
//!
//! 提供：
//! - `InputProfile::from_session` / `from_sdp_text` / `from_sap_session`：SDP → 描述符，
//!   只做结构校验（端口/PT/方向/声道数/framecount 一致性/channel-order 分组和），不做策略；
//! - `validate_receiver`：接收端验收，限制可注入；默认 48k、L16|L24、1..=64ch、要求
//!   `a=ts-refclk`、ptime ∈ {125 µs, 1 ms}；
//! - `select_session` / `select_session_report`：从 SAP 快照做确定性选择（name 子串 /
//!   group:port / `*` 全选），被拒条目带原因返回，不静默跳过；
//! - `canonical_positions`：channel-order 分组 → 规范通道位置序列（ST 2110-30 表 1 的
//!   命名通道 + `U01`..`U64`；没有标准名字的分组按流内顺序编号 `CH01`..，不猜名字）；
//! - `InputProfile::to_json` / `ResolvedInput::to_json`：确定性 JSON（字段顺序固定、
//!   无时间戳/绝对路径），供 daemon `/api/status` 的 input 段与 `reasons`。
//!
//! 边界：本模块**不做 socket/线程/RTP/时钟**，也不做媒体面接线（后续任务）；未知字段与
//! 未知符号一律返回稳定错误码 `E_INPUT_PROFILE_*`，不猜默认值。SDP 中本层不收录的属性
//! （`a=source-filter`、`a=comment`、未知 `a=` 行）按 RFC 8866 忽略，由上层决定。
//!
//! `sdp_sha256` 用本文件内嵌的最小 SHA-256（与 `crates/core/src/sha256.rs` 同实现；
//! 本 crate 保持纯 std 零第三方依赖，故不新增 path 依赖）。错误消息可直接进
//! `status.reasons`（英文、带具体数值；错误码稳定）。

use std::fmt::Write as _;
use std::net::Ipv4Addr;

use crate::sap::{SapKey, SapSession};
use crate::sdp::{self, Direction, Encoding, MediaClk, RefClk, SdpError, Session};

/// 接收端默认最大通道数（输入端比 AES67 基础 profile 的 8ch 宽：ST 2110-30 允许
/// 单流 1..=64ch，输入布局必须灵活）。
pub const DEFAULT_MAX_CHANNELS: u16 = 64;
/// AES67 媒体面采样率（默认接收集合只有 48k）。
pub const AES67_RATE_HZ: u32 = sdp::AES67_RATE_HZ;
/// 默认允许的 packet time（µs）：ST 2110-30 conformance level A = 1 ms、B = 1 ms/125 µs；
/// AES67 media profile 要求 ptime ≤ 1 ms。
pub const DEFAULT_PTIMES_US: [u32; 2] = [125, 1_000];
/// 单个 `channel-order` 最多分组数（与 `sdp::parse_channel_order` 一致）。
pub const MAX_CHANNEL_GROUPS: usize = 16;
/// 单流硬上限通道数（ST 2110-30 `channel-order` 上限；与 `sdp::parse_channel_order` 一致）。
pub const MAX_FLOW_CHANNELS: u16 = 64;

// ---------------------------------------------------------------------------
// 接收端限制（可注入；Default = 本层契约的验收默认值）
// ---------------------------------------------------------------------------

/// 接收端验收限制（纯数据；由部署/测试注入，不接受 env 隐式默认）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverLimits {
    /// 允许的最大每流通道数（默认 64；<1 视为非法配置，validate 会拒绝一切流）。
    pub max_channels: u16,
    /// 允许的采样率（默认 48000 Hz；顺序不敏感，仅用于描述）。
    pub allowed_rates_hz: Vec<u32>,
    /// 允许的线性编码（默认 [L16, L24]）。
    pub allowed_encodings: Vec<Encoding>,
    /// 是否要求 `a=ts-refclk`（默认 true：不猜时钟）。
    pub require_ts_refclk: bool,
    /// 允许的 packet time（µs；默认 125/1000）。未声明 `a=ptime` 时用 `a=framecount`
    /// 推导出的 ptime 参与检查。
    pub allowed_ptimes_us: Vec<u32>,
}

impl Default for ReceiverLimits {
    fn default() -> Self {
        Self {
            max_channels: DEFAULT_MAX_CHANNELS,
            allowed_rates_hz: vec![AES67_RATE_HZ],
            allowed_encodings: vec![Encoding::L16, Encoding::L24],
            require_ts_refclk: true,
            allowed_ptimes_us: DEFAULT_PTIMES_US.to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// 输入描述符
// ---------------------------------------------------------------------------

/// 接收端输入描述符：一条 SDP flow 的可验收字段（拥有所有权，可跨线程/跨状态文件保存）。
///
/// 字段来自 SDP `m=`/`c=`/`a=rtpmap`/`a=ptime`/`a=framecount`/`a=fmtp channel-order`/
/// `a=ts-refclk`/`a=mediaclk`，不做任何"缺省猜值"：SDP 未声明的字段为 `None`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputProfile {
    /// `s=` 会话名（缺失为 None；选择/排序/展示用）。
    pub session_name: Option<String>,
    /// `m=` 媒体类型（结构校验已保证为 `audio`；保留原值便于诊断）。
    pub media: String,
    /// `c=` 目的 IPv4 地址（媒体级优先，会话级兜底）。
    pub destination: Ipv4Addr,
    /// 目的地址是否组播（224.0.0.0/4）。SAP 通告的输入必须是组播。
    pub multicast: bool,
    /// RTP 端口。
    pub port: u16,
    /// 动态 payload type（AES67 要求 96..=127）。
    pub payload_type: u8,
    /// 线性编码（L16/L24）。
    pub encoding: Encoding,
    /// RTP 时钟率（AES67 48k）。
    pub sample_rate_hz: u32,
    /// 每包通道数。
    pub channels: u16,
    /// `a=ptime`（µs；缺失为 None，不猜 RTP 默认 20 ms）。
    pub ptime_us: Option<u32>,
    /// `a=framecount`（每包媒体帧数；缺失为 None）。
    pub framecount: Option<u32>,
    /// 结构化 `channel-order`（缺失为 None）。
    pub channel_order: Option<ChannelOrder>,
    /// `a=ts-refclk`（缺失为 None；是否可接受由 ReceiverLimits 决定）。
    pub refclk: Option<RefClk>,
    /// `a=mediaclk`（仅记录；AES67 常用 `direct=0`）。
    pub mediaclk: Option<MediaClk>,
    /// 媒体方向（`recvonly`/`inactive` 在结构校验即拒收）。
    pub direction: Direction,
    /// `o=` 源地址（若可解析；SAP 场景可用于对账 originating source）。
    pub origin_address: Option<Ipv4Addr>,
}

impl InputProfile {
    /// 从已解析的 `Session` 构造（只做结构校验，不做接收端策略）。
    pub fn from_session(session: &Session<'_>) -> Result<Self, InputProfileError> {
        let media = &session.media;
        if media.media != "audio" {
            return Err(InputProfileError::NotAudioMedia {
                media: Some(media.media.to_string()),
            });
        }
        if media.protocol != "RTP/AVP" {
            return Err(InputProfileError::BadMedia { port: media.port });
        }
        let connection = session
            .connection()
            .ok_or(InputProfileError::MissingConnection)?;
        if media.port == 0 {
            return Err(InputProfileError::BadMedia { port: media.port });
        }
        if media.payload_type < 96 {
            return Err(InputProfileError::BadPayloadType {
                payload_type: media.payload_type,
            });
        }
        if media.direction == Direction::RecvOnly || media.direction == Direction::Inactive {
            return Err(InputProfileError::BadDirection {
                direction: media.direction.name(),
            });
        }
        if media.rtpmap.channels == 0 {
            return Err(InputProfileError::BadChannels { channels: 0 });
        }
        if let Some(ptime_us) = media.ptime_us {
            if ptime_us == 0 {
                return Err(InputProfileError::BadPtime { ptime_us });
            }
        }
        if let Some(framecount) = media.framecount {
            if framecount == 0 {
                return Err(InputProfileError::BadFramecount {
                    ptime_us: media.ptime_us,
                    framecount,
                });
            }
            if let Some(ptime_us) = media.ptime_us {
                if !framing_consistent(ptime_us, framecount, media.rtpmap.rate_hz) {
                    return Err(InputProfileError::BadFramecount {
                        ptime_us: media.ptime_us,
                        framecount,
                    });
                }
            }
        }
        let channel_order = match media.channel_order {
            Some(raw) => {
                let order = ChannelOrder::parse(raw)?;
                let groups_total = order.total_channels();
                if groups_total != media.rtpmap.channels {
                    return Err(InputProfileError::BadChannelOrderCount {
                        channels: media.rtpmap.channels,
                        groups_total: groups_total as u32,
                    });
                }
                Some(order)
            }
            None => None,
        };
        Ok(Self {
            session_name: session.name.map(str::to_string),
            media: media.media.to_string(),
            destination: connection.ipv4(),
            multicast: connection.is_multicast(),
            port: media.port,
            payload_type: media.payload_type,
            encoding: media.rtpmap.encoding,
            sample_rate_hz: media.rtpmap.rate_hz,
            channels: media.rtpmap.channels,
            ptime_us: media.ptime_us,
            framecount: media.framecount,
            channel_order,
            refclk: media.refclk,
            mediaclk: media.mediaclk,
            direction: media.direction,
            origin_address: session.origin_address(),
        })
    }

    /// 从 SDP 文本构造（解析 + 结构校验；解析失败映射到稳定输入错误码）。
    pub fn from_sdp_text(text: &str) -> Result<Self, InputProfileError> {
        let session = sdp::parse_sdp(text).map_err(map_sdp_error)?;
        Self::from_session(&session)
    }

    /// 从 SAP 会话表条目构造（announcement 必须是**组播**输入；unicast 只能由直连 SDP 提供）。
    ///
    /// SAP 表里的 `profile_error` 是 sdp 层"单 flow 8ch/L24"媒体门；输入端验收比它宽
    /// （L16、最多 64ch、ptime 集合可注入），因此这里按输入描述符规则重新判定，不沿用。
    pub fn from_sap_session(session: &SapSession) -> Result<Self, InputProfileError> {
        let profile = Self::from_sdp_text(&session.sdp)?;
        if !profile.multicast {
            return Err(InputProfileError::NotMulticast {
                destination: profile.destination,
            });
        }
        Ok(profile)
    }

    /// 生效 packet time（µs）：优先 `a=ptime`；否则由 `a=framecount` 按采样率整数换算
    /// （48k 下 6 帧 = 125 µs、48 帧 = 1000 µs）。两者都缺失为 None（不猜）。
    pub fn effective_ptime_us(&self) -> Option<u32> {
        if let Some(ptime_us) = self.ptime_us {
            return Some(ptime_us);
        }
        let framecount = self.framecount?;
        if self.sample_rate_hz == 0 {
            return None;
        }
        let micros = (framecount as u64 * 1_000_000) / self.sample_rate_hz as u64;
        Some(micros as u32)
    }

    /// 每包媒体帧数：优先 `a=framecount`；否则由 `a=ptime` 按采样率整数换算（向下取整）。
    /// 两者都缺失为 None（不猜）。
    pub fn packet_frames(&self) -> Option<u32> {
        if let Some(framecount) = self.framecount {
            return Some(framecount);
        }
        let ptime_us = self.ptime_us?;
        if self.sample_rate_hz == 0 {
            return None;
        }
        let frames = (ptime_us as u64 * self.sample_rate_hz as u64) / 1_000_000;
        Some(frames as u32)
    }

    /// 确定性 JSON（字段顺序固定、无时间戳/绝对路径），供 `/api/status` 的 input 段。
    pub fn to_json(&self) -> String {
        let mut object = JsonObject::new();
        object.key("session_name");
        object.opt_string(self.session_name.as_deref());
        object.key("media");
        object.string(&self.media);
        object.key("destination");
        object.string(&self.destination.to_string());
        object.key("multicast");
        object.boolean(self.multicast);
        object.key("port");
        object.number(u64::from(self.port));
        object.key("payload_type");
        object.number(u64::from(self.payload_type));
        object.key("encoding");
        object.string(self.encoding.as_str());
        object.key("sample_rate_hz");
        object.number(u64::from(self.sample_rate_hz));
        object.key("channels");
        object.number(u64::from(self.channels));
        object.key("ptime_us");
        object.opt_number(self.ptime_us.map(u64::from));
        object.key("framecount");
        object.opt_number(self.framecount.map(u64::from));
        object.key("effective_ptime_us");
        object.opt_number(self.effective_ptime_us().map(u64::from));
        object.key("packet_frames");
        object.opt_number(self.packet_frames().map(u64::from));
        object.key("channel_order");
        match &self.channel_order {
            Some(order) => object.raw(&channel_order_json(order)),
            None => object.raw("null"),
        }
        object.key("refclk");
        match self.refclk {
            Some(refclk) => object.raw(&refclk_json(refclk)),
            None => object.raw("null"),
        }
        object.key("mediaclk");
        match self.mediaclk {
            Some(mediaclk) => object.raw(&mediaclk_json(mediaclk)),
            None => object.raw("null"),
        }
        object.key("direction");
        object.string(self.direction.name());
        object.key("origin_address");
        object.opt_string(
            self.origin_address
                .map(|address| address.to_string())
                .as_deref(),
        );
        object.finish()
    }
}

/// ptime（µs）与 framecount（帧）一致性：允许 ±1 帧量化（与 `Session::validate_media_profile` 同规则）。
fn framing_consistent(ptime_us: u32, framecount: u32, rate_hz: u32) -> bool {
    if rate_hz == 0 {
        return false;
    }
    let expected_us = framecount as u64 * 1_000_000 / rate_hz as u64;
    let tolerance_us = 1_000_000 / rate_hz as u64;
    let actual_us = ptime_us as u64;
    actual_us + tolerance_us >= expected_us && actual_us <= expected_us + tolerance_us
}

/// SDP 解析错误 → 输入描述符稳定错误码。
fn map_sdp_error(error: SdpError) -> InputProfileError {
    match error {
        SdpError::UnsupportedMedia => InputProfileError::NotAudioMedia { media: None },
        SdpError::UnsupportedEncoding => InputProfileError::UnsupportedEncoding {
            declared: None,
            allowed: vec![Encoding::L16, Encoding::L24],
        },
        SdpError::MissingConnection | SdpError::BadConnection => {
            InputProfileError::MissingConnection
        }
        other => InputProfileError::Sdp(other),
    }
}

// ---------------------------------------------------------------------------
// channel-order：分组符号 → 结构 + 规范通道位置
// ---------------------------------------------------------------------------

/// ST 2110-30 分组符号的已知规格（表 1 子集 + 本 crate 的数字扩展约定）。
enum SymbolSpec {
    /// 表 1 命名分组：组内规范通道名。
    Named(&'static [&'static str]),
    /// `U01`..`U64` 未定义通道：单通道，编号取自符号。
    Undefined(u8),
    /// 标准没有逐通道名字的分组（`222`/`SGRP`/数字扩展）：只知道数量，不猜名字。
    Unnamed(u16),
}

impl SymbolSpec {
    fn channels(&self) -> u16 {
        match self {
            SymbolSpec::Named(names) => names.len() as u16,
            SymbolSpec::Undefined(_) => 1,
            SymbolSpec::Unnamed(channels) => *channels,
        }
    }

    fn names(&self) -> Vec<String> {
        match self {
            SymbolSpec::Named(names) => names.iter().map(|name| (*name).to_string()).collect(),
            SymbolSpec::Undefined(number) => vec![format!("U{number:02}")],
            SymbolSpec::Unnamed(_) => Vec::new(),
        }
    }
}

const NAMES_M: &[&str] = &["M1"];
const NAMES_DM: &[&str] = &["M1", "M2"];
const NAMES_ST: &[&str] = &["L", "R"];
const NAMES_LTRT: &[&str] = &["Lt", "Rt"];
const NAMES_51: &[&str] = &["L", "R", "C", "LFE", "Ls", "Rs"];
const NAMES_71: &[&str] = &["L", "R", "C", "LFE", "Lss", "Rss", "Lrs", "Rrs"];

/// 分组符号 → 规格（SMPTE ST 2110-30:2017 Table 1 子集；未知返回 None，调用方报稳定错误）。
///
/// 表 1 之外保留本 crate `sdp::parse_channel_order` 已接受的两种数字扩展（现场 NSP
/// `channel-order=SMPTE2110.(41,2)`）：两位数字按 x.y 求和（41→5 ch）、一位数字按通道数。
/// `222`（22.2）的逐通道顺序在 ST 2036-2 表 1，`SGRP` 是一个 SDI audio group，
/// 两者都没有本层可复用的逐通道名字 → 只报数量，位置按流内顺序编号（不猜）。
fn channel_symbol_spec(symbol: &str) -> Option<SymbolSpec> {
    match symbol {
        "M" => Some(SymbolSpec::Named(NAMES_M)),
        "DM" => Some(SymbolSpec::Named(NAMES_DM)),
        "ST" => Some(SymbolSpec::Named(NAMES_ST)),
        "LtRt" => Some(SymbolSpec::Named(NAMES_LTRT)),
        "51" => Some(SymbolSpec::Named(NAMES_51)),
        "71" => Some(SymbolSpec::Named(NAMES_71)),
        "222" => Some(SymbolSpec::Unnamed(24)),
        "SGRP" => Some(SymbolSpec::Unnamed(4)),
        _ => {
            if let Some(digits) = symbol.strip_prefix('U') {
                if digits.len() != 2 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                let number: u8 = digits.parse().ok()?;
                if number == 0 || number > MAX_FLOW_CHANNELS as u8 {
                    return None;
                }
                return Some(SymbolSpec::Undefined(number));
            }
            let bytes = symbol.as_bytes();
            if bytes.len() == 2 && bytes.iter().all(u8::is_ascii_digit) {
                let channels = u16::from(bytes[0] - b'0') + u16::from(bytes[1] - b'0');
                return Some(SymbolSpec::Unnamed(channels));
            }
            if bytes.len() == 1 && bytes[0].is_ascii_digit() {
                return Some(SymbolSpec::Unnamed(u16::from(bytes[0] - b'0')));
            }
            None
        }
    }
}

/// `channel-order` 里的一组通道。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelGroup {
    /// SDP 原始分组符号（如 `71`、`M`、`U05`、dd 数字扩展 `41`）。
    pub symbol: String,
    /// 组内通道数（表 1 或本 crate 数字扩展约定）。
    pub channels: u16,
    /// 组内规范通道名（表 1；`U##` 组为 `U##` 本身）；空 = 标准没有逐通道名字（不猜）。
    pub names: Vec<String>,
}

impl ChannelGroup {
    /// 组内通道标签：有标准名字则原样返回；没有则返回空（不猜）。
    pub fn labels(&self) -> &[String] {
        &self.names
    }
}

/// 结构化 `a=fmtp channel-order=SMPTE2110.(...)`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOrder {
    /// 原始值（如 `SMPTE2110.(41,2)`），便于对账/展示。
    pub raw: String,
    /// 分组列表（顺序 = SDP 出现顺序 = 线上通道顺序）。
    pub groups: Vec<ChannelGroup>,
}

impl ChannelOrder {
    /// 解析 `SMPTE2110.(...)` 值（未知符号/语法错误返回稳定错误码，不猜分组）。
    pub fn parse(raw: &str) -> Result<Self, InputProfileError> {
        let inner = raw
            .strip_prefix("SMPTE2110.")
            .and_then(|rest| rest.strip_prefix('('))
            .and_then(|rest| rest.strip_suffix(')'))
            .filter(|inner| !inner.is_empty())
            .ok_or(InputProfileError::BadChannelOrderSyntax)?;
        let symbols: Vec<&str> = inner.split(',').collect();
        if symbols.len() > MAX_CHANNEL_GROUPS {
            return Err(InputProfileError::BadChannelOrderSyntax);
        }
        let mut groups = Vec::with_capacity(symbols.len());
        let mut total: u32 = 0;
        for symbol in symbols {
            if symbol.is_empty() {
                return Err(InputProfileError::BadChannelOrderSyntax);
            }
            let spec = channel_symbol_spec(symbol).ok_or_else(|| {
                InputProfileError::UnknownChannelSymbol {
                    symbol: symbol.to_string(),
                }
            })?;
            let channels = spec.channels();
            if channels == 0 || channels > MAX_FLOW_CHANNELS {
                return Err(InputProfileError::BadChannelOrderSyntax);
            }
            total = total.saturating_add(u32::from(channels));
            groups.push(ChannelGroup {
                symbol: symbol.to_string(),
                channels,
                names: spec.names(),
            });
        }
        if total > u32::from(MAX_FLOW_CHANNELS) {
            return Err(InputProfileError::BadChannelOrderSyntax);
        }
        Ok(Self {
            raw: raw.to_string(),
            groups,
        })
    }

    /// 分组通道数之和。
    pub fn total_channels(&self) -> u16 {
        self.groups
            .iter()
            .map(|group| group.channels)
            .fold(0, u16::saturating_add)
    }
}

/// 规范通道位置（供上层映射到内部 lane）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelPosition {
    /// ST 2110-30 表 1 的规范通道名（`L`/`R`/`C`/`LFE`/…、`U01`..`U64`）。
    Named(String),
    /// 标准没有逐通道名字的分组（`222`/`SGRP`/数字扩展）：按流内出现顺序编号，
    /// 标签 `CH01`..（从 1 起，全局不重复）。不猜 FL/FR。
    Numbered(u16),
}

impl ChannelPosition {
    /// 展示/JSON 标签。
    pub fn label(&self) -> String {
        match self {
            ChannelPosition::Named(name) => name.clone(),
            ChannelPosition::Numbered(number) => format!("CH{number:02}"),
        }
    }
}

/// `channel-order` → 规范通道位置序列（顺序 = 线上通道顺序）。
///
/// 抛出：未知 SMPTE2110 符号 `E_INPUT_PROFILE_UNKNOWN_CHANNEL_SYMBOL`；分组数量与符号定义
/// 不一致 `E_INPUT_PROFILE_CHANNEL_GROUP_MISMATCH`（手工构造的坏数据也不静默）。
pub fn canonical_positions(
    order: &ChannelOrder,
) -> Result<Vec<ChannelPosition>, InputProfileError> {
    let mut positions = Vec::new();
    let mut numbered: u16 = 0;
    for group in &order.groups {
        let spec = channel_symbol_spec(&group.symbol).ok_or_else(|| {
            InputProfileError::UnknownChannelSymbol {
                symbol: group.symbol.clone(),
            }
        })?;
        let spec_channels = spec.channels();
        if spec_channels == 0 || spec_channels > MAX_FLOW_CHANNELS {
            return Err(InputProfileError::BadChannelOrderSyntax);
        }
        if group.channels != spec_channels {
            return Err(InputProfileError::ChannelGroupMismatch {
                symbol: group.symbol.clone(),
                channels: group.channels,
                expected_channels: spec_channels,
            });
        }
        match spec {
            SymbolSpec::Named(names) => positions.extend(
                names
                    .iter()
                    .map(|name| ChannelPosition::Named((*name).to_string())),
            ),
            SymbolSpec::Undefined(number) => {
                positions.push(ChannelPosition::Named(format!("U{number:02}")));
            }
            SymbolSpec::Unnamed(_) => {
                for _ in 0..spec_channels {
                    numbered = numbered.saturating_add(1);
                    positions.push(ChannelPosition::Numbered(numbered));
                }
            }
        }
    }
    Ok(positions)
}

// ---------------------------------------------------------------------------
// 接收端验收
// ---------------------------------------------------------------------------

/// 接收端验收：把描述符与可注入限制比对。
///
/// 检查顺序固定（错误码稳定、可进 `status.reasons`）：媒体类型 → 声道数 → 采样率 →
/// 编码 → `a=ts-refclk` → packet framing/ptime。只做策略判定，不修改描述符。
pub fn validate_receiver(
    profile: &InputProfile,
    limits: &ReceiverLimits,
) -> Result<(), InputProfileError> {
    if profile.media != "audio" {
        return Err(InputProfileError::NotAudioMedia {
            media: Some(profile.media.clone()),
        });
    }
    if profile.channels == 0 {
        return Err(InputProfileError::BadChannels { channels: 0 });
    }
    if profile.channels > limits.max_channels {
        return Err(InputProfileError::ChannelsExceedLimit {
            channels: profile.channels,
            max_channels: limits.max_channels,
        });
    }
    if !limits.allowed_rates_hz.contains(&profile.sample_rate_hz) {
        return Err(InputProfileError::RateNotAllowed {
            rate_hz: profile.sample_rate_hz,
            allowed_hz: limits.allowed_rates_hz.clone(),
        });
    }
    if !limits.allowed_encodings.contains(&profile.encoding) {
        return Err(InputProfileError::UnsupportedEncoding {
            declared: Some(profile.encoding.as_str().to_string()),
            allowed: limits.allowed_encodings.clone(),
        });
    }
    if limits.require_ts_refclk && profile.refclk.is_none() {
        return Err(InputProfileError::MissingRefClk);
    }
    let Some(ptime_us) = profile.effective_ptime_us() else {
        return Err(InputProfileError::MissingFraming);
    };
    if !limits.allowed_ptimes_us.contains(&ptime_us) {
        return Err(InputProfileError::PtimeNotAllowed {
            ptime_us,
            allowed_us: limits.allowed_ptimes_us.clone(),
            derived: profile.ptime_us.is_none(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SAP 快照选择
// ---------------------------------------------------------------------------

/// 会话名过滤。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum NameFilter {
    /// 不按名过滤（`*` 全选）。
    #[default]
    Any,
    /// `s=` 包含该子串（区分大小写）。
    Contains(String),
}

/// 选择条件（纯数据；`Default` = 全选 + `ReceiverLimits::default()`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionCriteria {
    /// 会话名过滤。
    pub name: NameFilter,
    /// 精确目的地址（None = 不按地址过滤）。
    pub destination: Option<Ipv4Addr>,
    /// 精确 RTP 端口（None = 不按端口过滤）。
    pub port: Option<u16>,
    /// 接收端验收限制。
    pub limits: ReceiverLimits,
}

impl Default for SelectionCriteria {
    fn default() -> Self {
        Self {
            name: NameFilter::Any,
            destination: None,
            port: None,
            limits: ReceiverLimits::default(),
        }
    }
}

impl SelectionCriteria {
    /// 全选（`*`）。
    pub fn all() -> Self {
        Self::default()
    }

    /// 解析选择器文本：`*` 全选、`name:<substring>` 按名、`<ipv4>:<port>` 按 group:port。
    /// 其它形式返回 `E_INPUT_PROFILE_BAD_SELECTOR`（不猜意图）。
    pub fn parse(spec: &str) -> Result<Self, InputProfileError> {
        let spec = spec.trim();
        if spec == "*" {
            return Ok(Self::all());
        }
        if let Some(needle) = spec.strip_prefix("name:") {
            if needle.is_empty() {
                return Err(InputProfileError::BadSelector {
                    spec: spec.to_string(),
                });
            }
            return Ok(Self::all().with_name_contains(needle));
        }
        if let Some((address, port)) = spec.rsplit_once(':') {
            let Ok(address) = address.parse::<Ipv4Addr>() else {
                return Err(InputProfileError::BadSelector {
                    spec: spec.to_string(),
                });
            };
            let Ok(port) = port.parse::<u16>() else {
                return Err(InputProfileError::BadSelector {
                    spec: spec.to_string(),
                });
            };
            return Ok(Self::all().with_destination(address).with_port(port));
        }
        Err(InputProfileError::BadSelector {
            spec: spec.to_string(),
        })
    }

    /// 设置会话名子串过滤。
    pub fn with_name_contains(mut self, needle: impl Into<String>) -> Self {
        self.name = NameFilter::Contains(needle.into());
        self
    }

    /// 设置目的地址过滤。
    pub fn with_destination(mut self, destination: Ipv4Addr) -> Self {
        self.destination = Some(destination);
        self
    }

    /// 设置端口过滤。
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// 设置接收端限制。
    pub fn with_limits(mut self, limits: ReceiverLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 选择器文本描述（进 `status.reasons`；确定性）。
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        match &self.name {
            NameFilter::Any => parts.push("session_name=*".to_string()),
            NameFilter::Contains(needle) => {
                parts.push(format!("session_name contains \"{needle}\""));
            }
        }
        if let Some(destination) = self.destination {
            parts.push(format!("destination={destination}"));
        }
        if let Some(port) = self.port {
            parts.push(format!("port={port}"));
        }
        parts.join(" ")
    }

    /// 与一条已解析 SDP 的元数据比对（SDP 不可解析时无法评估 → 不匹配）。
    fn matches(&self, session: Option<&Session<'_>>) -> bool {
        if self.name == NameFilter::Any && self.destination.is_none() && self.port.is_none() {
            return true;
        }
        let Some(session) = session else {
            return false;
        };
        if let NameFilter::Contains(needle) = &self.name {
            let Some(name) = session.name else {
                return false;
            };
            if !name.contains(needle.as_str()) {
                return false;
            }
        }
        if let Some(destination) = self.destination {
            let Some(connection) = session.connection() else {
                return false;
            };
            if connection.ipv4() != destination {
                return false;
            }
        }
        if let Some(port) = self.port {
            if session.media.port != port {
                return false;
            }
        }
        true
    }
}

/// 一条 SAP 会话的选择/验收结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedInput {
    /// SAP 会话键（originating source + msg id hash）。
    pub key: SapKey,
    /// 通告 SDP 文本的 SHA-256（小写 hex；确定性）。
    pub sdp_sha256: String,
    /// 结构解析结果；None = SDP 无法成为输入描述符（原因在 `rejection`）。
    pub profile: Option<InputProfile>,
    /// 拒绝原因（结构错误或接收端验收错误）；None = 可接收。
    pub rejection: Option<InputProfileError>,
}

impl ResolvedInput {
    /// 是否可接收（结构 + 验收均通过）。
    pub fn is_accepted(&self) -> bool {
        self.rejection.is_none()
    }

    /// 会话名（结构解析失败时为 None）。
    pub fn session_name(&self) -> Option<&str> {
        self.profile
            .as_ref()
            .and_then(|profile| profile.session_name.as_deref())
    }

    /// 目的地址（结构解析失败时为 None）。
    pub fn destination(&self) -> Option<Ipv4Addr> {
        self.profile.as_ref().map(|profile| profile.destination)
    }

    /// RTP 端口（结构解析失败时为 None）。
    pub fn port(&self) -> Option<u16> {
        self.profile.as_ref().map(|profile| profile.port)
    }

    /// 确定性 JSON（key/sha/验收结论/拒绝码/完整 profile）。
    pub fn to_json(&self) -> String {
        let mut object = JsonObject::new();
        object.key("key");
        object.string(&self.key.text());
        object.key("originator");
        object.string(&self.key.originator.to_string());
        object.key("msg_id_hash");
        object.number(u64::from(self.key.msg_id_hash));
        object.key("sdp_sha256");
        object.string(&self.sdp_sha256);
        object.key("session_name");
        object.opt_string(self.session_name());
        object.key("destination");
        object.opt_string(
            self.destination()
                .map(|address| address.to_string())
                .as_deref(),
        );
        object.key("port");
        object.opt_number(self.port().map(u64::from));
        object.key("accepted");
        object.boolean(self.is_accepted());
        object.key("rejection");
        match &self.rejection {
            Some(error) => {
                let mut rejection = JsonObject::new();
                rejection.key("code");
                rejection.string(error.name());
                rejection.key("message");
                rejection.string(&error.message());
                object.raw(&rejection.finish());
            }
            None => object.raw("null"),
        }
        object.key("profile");
        match &self.profile {
            Some(profile) => object.raw(&profile.to_json()),
            None => object.raw("null"),
        }
        object.finish()
    }
}

/// 选择结果 + 诊断（`considered`/`unmatched`/`reasons` 让"没匹配到"也有明确原因）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionOutcome {
    /// 与选择条件匹配的条目（含被拒条目及其原因；按 name, group, port 确定性排序）。
    pub inputs: Vec<ResolvedInput>,
    /// 快照条目总数。
    pub considered: usize,
    /// 未匹配选择条件而被排除的条目数。
    pub unmatched: usize,
    /// 诊断原因（无匹配、被排除、被拒计数、错误码集合；确定性顺序）。
    pub reasons: Vec<String>,
}

impl SelectionOutcome {
    /// 可接收入口数。
    pub fn accepted(&self) -> usize {
        self.inputs
            .iter()
            .filter(|input| input.is_accepted())
            .count()
    }

    /// 被拒条目数。
    pub fn rejected(&self) -> usize {
        self.inputs.len() - self.accepted()
    }
}

/// 从 SAP 快照选择输入（`select_session_report(...).inputs`）。
pub fn select_session(snapshot: &[SapSession], criteria: &SelectionCriteria) -> Vec<ResolvedInput> {
    select_session_report(snapshot, criteria).inputs
}

/// 从 SAP 快照选择输入并返回诊断（不静默跳过：匹配到的条目逐个带原因，未匹配/不可评估也计数）。
pub fn select_session_report(
    snapshot: &[SapSession],
    criteria: &SelectionCriteria,
) -> SelectionOutcome {
    let mut inputs = Vec::new();
    let mut unmatched = 0usize;
    let mut unparsable_unmatched = 0usize;
    for session in snapshot {
        let parsed = sdp::parse_sdp(&session.sdp).ok();
        if !criteria.matches(parsed.as_ref()) {
            unmatched += 1;
            if parsed.is_none() {
                unparsable_unmatched += 1;
            }
            continue;
        }
        let sdp_sha256 = sha256::hex(session.sdp.as_bytes());
        // 结构错误 → profile None（原因在 rejection）；策略拒绝（含 SAP 组播要求）→ 保留
        // profile，daemon 仍能看到该流的字段并展示拒绝原因。
        match InputProfile::from_sdp_text(&session.sdp) {
            Ok(profile) => {
                let rejection = if profile.multicast {
                    validate_receiver(&profile, &criteria.limits).err()
                } else {
                    Some(InputProfileError::NotMulticast {
                        destination: profile.destination,
                    })
                };
                inputs.push(ResolvedInput {
                    key: session.key,
                    sdp_sha256,
                    profile: Some(profile),
                    rejection,
                });
            }
            Err(error) => {
                inputs.push(ResolvedInput {
                    key: session.key,
                    sdp_sha256,
                    profile: None,
                    rejection: Some(error),
                });
            }
        }
    }
    inputs.sort_by(|left, right| resolved_sort_key(left).cmp(&resolved_sort_key(right)));
    let mut reasons = Vec::new();
    if snapshot.is_empty() {
        reasons.push("SAP snapshot is empty".to_string());
    }
    if inputs.is_empty() && !snapshot.is_empty() {
        reasons.push(format!(
            "no SAP session matched selector {}",
            criteria.describe()
        ));
    }
    if unmatched > 0 {
        reasons.push(format!(
            "{unmatched} SAP session(s) excluded by selector {}",
            criteria.describe()
        ));
    }
    if unparsable_unmatched > 0 {
        reasons.push(format!(
            "{unparsable_unmatched} excluded session(s) have unparsable SDP, so the selector could not be evaluated"
        ));
    }
    let rejected = inputs.iter().filter(|input| !input.is_accepted()).count();
    if rejected > 0 {
        let mut codes: Vec<&'static str> = Vec::new();
        for input in &inputs {
            if let Some(error) = &input.rejection {
                if !codes.contains(&error.name()) {
                    codes.push(error.name());
                }
            }
        }
        reasons.push(format!(
            "{rejected} matched session(s) rejected: {}",
            codes.join(", ")
        ));
    }
    SelectionOutcome {
        inputs,
        considered: snapshot.len(),
        unmatched,
        reasons,
    }
}

/// 排序键：(session name, group, port, sdp_sha256, SAP key)。
fn resolved_sort_key(input: &ResolvedInput) -> (Option<&str>, [u8; 4], u16, &str, [u8; 4], u16) {
    (
        input.session_name(),
        input
            .destination()
            .map(|address| address.octets())
            .unwrap_or([0; 4]),
        input.port().unwrap_or(0),
        input.sdp_sha256.as_str(),
        input.key.originator.octets(),
        input.key.msg_id_hash,
    )
}

// ---------------------------------------------------------------------------
// 稳定错误码
// ---------------------------------------------------------------------------

/// 输入描述符错误（Clone + 稳定错误码；消息带具体数值，可直接进 `status.reasons`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputProfileError {
    /// SDP 解析失败（底层稳定码在 `sdp_error()` 里；code = E_INPUT_PROFILE_BAD_SDP）。
    Sdp(SdpError),
    /// `m=` 非 audio / 传输非 RTP/AVP。
    NotAudioMedia { media: Option<String> },
    /// 缺生效 `c=`（或 `c=` 非法，底层已映射）。
    MissingConnection,
    /// 端口非法 / 传输协议非法。
    BadMedia { port: u16 },
    /// 静态/保留 payload type（AES67 要求 96..=127）。
    BadPayloadType { payload_type: u8 },
    /// 方向 `recvonly`/`inactive`（不能作为接收源）。
    BadDirection { direction: &'static str },
    /// 结构上非法声道数（0）。
    BadChannels { channels: u16 },
    /// 声道数超过接收端限制。
    ChannelsExceedLimit { channels: u16, max_channels: u16 },
    /// 采样率不在接收集合。
    RateNotAllowed { rate_hz: u32, allowed_hz: Vec<u32> },
    /// 编码不可接受：SDP 层声明的编码不是 L16/L24（declared=None），或编码不在接收集合。
    UnsupportedEncoding {
        declared: Option<String>,
        allowed: Vec<Encoding>,
    },
    /// 缺 `a=ts-refclk` 且接收端要求声明参考钟。
    MissingRefClk,
    /// 结构上非法 ptime（0）。
    BadPtime { ptime_us: u32 },
    /// ptime 不在允许集合（`derived` = 由 `a=framecount` 推导而来）。
    PtimeNotAllowed {
        ptime_us: u32,
        allowed_us: Vec<u32>,
        derived: bool,
    },
    /// framecount 非法，或与 ptime 不一致。
    BadFramecount {
        ptime_us: Option<u32>,
        framecount: u32,
    },
    /// `a=ptime`/`a=framecount` 都缺失，packet framing 无法确定（不猜）。
    MissingFraming,
    /// `channel-order` 语法/数量上限非法。
    BadChannelOrderSyntax,
    /// `channel-order` 分组通道数之和 ≠ 媒体通道数。
    BadChannelOrderCount { channels: u16, groups_total: u32 },
    /// 未知 SMPTE2110 分组符号（不猜分组）。
    UnknownChannelSymbol { symbol: String },
    /// 分组符号定义的通道数与分组记录不一致（手工构造的坏数据）。
    ChannelGroupMismatch {
        symbol: String,
        channels: u16,
        expected_channels: u16,
    },
    /// 目的地址非 IPv4 组播（SAP 通告输入必须组播）。
    NotMulticast { destination: Ipv4Addr },
    /// 选择器文本非法。
    BadSelector { spec: String },
}

impl InputProfileError {
    /// 稳定错误码（进 `status.reasons` / 证据 JSON；不随消息变化）。
    pub const fn name(&self) -> &'static str {
        match self {
            InputProfileError::Sdp(_) => "E_INPUT_PROFILE_BAD_SDP",
            InputProfileError::NotAudioMedia { .. } => "E_INPUT_PROFILE_NOT_AUDIO",
            InputProfileError::MissingConnection => "E_INPUT_PROFILE_MISSING_CONNECTION",
            InputProfileError::BadMedia { .. } => "E_INPUT_PROFILE_BAD_MEDIA",
            InputProfileError::BadPayloadType { .. } => "E_INPUT_PROFILE_BAD_PAYLOAD",
            InputProfileError::BadDirection { .. } => "E_INPUT_PROFILE_BAD_DIRECTION",
            InputProfileError::BadChannels { .. } => "E_INPUT_PROFILE_BAD_CHANNELS",
            InputProfileError::ChannelsExceedLimit { .. } => {
                "E_INPUT_PROFILE_CHANNELS_EXCEED_LIMIT"
            }
            InputProfileError::RateNotAllowed { .. } => "E_INPUT_PROFILE_UNSUPPORTED_RATE",
            InputProfileError::UnsupportedEncoding { .. } => "E_INPUT_PROFILE_UNSUPPORTED_ENCODING",
            InputProfileError::MissingRefClk => "E_INPUT_PROFILE_MISSING_REFCLK",
            InputProfileError::BadPtime { .. } => "E_INPUT_PROFILE_BAD_PTIME",
            InputProfileError::PtimeNotAllowed { .. } => "E_INPUT_PROFILE_PTIME_NOT_ALLOWED",
            InputProfileError::BadFramecount { .. } => "E_INPUT_PROFILE_BAD_FRAMECOUNT",
            InputProfileError::MissingFraming => "E_INPUT_PROFILE_MISSING_FRAMING",
            InputProfileError::BadChannelOrderSyntax
            | InputProfileError::BadChannelOrderCount { .. } => "E_INPUT_PROFILE_BAD_CHANNEL_ORDER",
            InputProfileError::UnknownChannelSymbol { .. } => {
                "E_INPUT_PROFILE_UNKNOWN_CHANNEL_SYMBOL"
            }
            InputProfileError::ChannelGroupMismatch { .. } => {
                "E_INPUT_PROFILE_CHANNEL_GROUP_MISMATCH"
            }
            InputProfileError::NotMulticast { .. } => "E_INPUT_PROFILE_NOT_MULTICAST",
            InputProfileError::BadSelector { .. } => "E_INPUT_PROFILE_BAD_SELECTOR",
        }
    }

    /// 人类可读消息（英文、含具体数值；确定性、无时间戳/绝对路径）。
    pub fn message(&self) -> String {
        match self {
            InputProfileError::Sdp(error) => format!("SDP parse failed: {}", error.name()),
            InputProfileError::NotAudioMedia { media: Some(media) } => {
                format!("media type \"{media}\" is not audio (AES67 input requires m=audio RTP/AVP)")
            }
            InputProfileError::NotAudioMedia { media: None } => {
                "SDP m= is not audio or its transport is not RTP/AVP".to_string()
            }
            InputProfileError::MissingConnection => {
                "SDP has no usable c= destination address".to_string()
            }
            InputProfileError::BadMedia { port } => {
                format!("invalid RTP media port {port} or transport protocol")
            }
            InputProfileError::BadPayloadType { payload_type } => {
                format!(
                    "payload type {payload_type} is not a dynamic RTP payload type (AES67 requires 96..=127)"
                )
            }
            InputProfileError::BadDirection { direction } => {
                format!("media direction \"{direction}\" cannot be received as an input")
            }
            InputProfileError::BadChannels { channels } => {
                format!("channel count {channels} is invalid (must be >= 1)")
            }
            InputProfileError::ChannelsExceedLimit {
                channels,
                max_channels,
            } => format!("channel count {channels} exceeds receiver limit {max_channels}"),
            InputProfileError::RateNotAllowed {
                rate_hz,
                allowed_hz,
            } => format!("sample rate {rate_hz} Hz is not in the receiver set {allowed_hz:?}"),
            InputProfileError::UnsupportedEncoding { declared, allowed } => match declared {
                Some(declared) => format!(
                    "encoding {declared} is not in the receiver set {allowed:?}"
                ),
                None => format!(
                    "declared SDP encoding is not L16/L24 (AES67 linear PCM only; allowed {allowed:?})"
                ),
            },
            InputProfileError::MissingRefClk => {
                "a=ts-refclk is missing but the receiver requires a declared reference clock"
                    .to_string()
            }
            InputProfileError::BadPtime { ptime_us } => format!("invalid ptime {ptime_us} us"),
            InputProfileError::PtimeNotAllowed {
                ptime_us,
                allowed_us,
                derived,
            } => {
                let source = if *derived {
                    "derived from a=framecount"
                } else {
                    "declared by a=ptime"
                };
                format!("packet time {ptime_us} us ({source}) is not in the receiver set {allowed_us:?}")
            }
            InputProfileError::BadFramecount {
                ptime_us,
                framecount,
            } => match ptime_us {
                Some(ptime_us) => format!(
                    "framecount {framecount} is inconsistent with ptime {ptime_us} us"
                ),
                None => format!("framecount {framecount} is invalid"),
            },
            InputProfileError::MissingFraming => {
                "neither a=ptime nor a=framecount is declared, so packet framing cannot be derived"
                    .to_string()
            }
            InputProfileError::BadChannelOrderSyntax => {
                "malformed SMPTE2110 channel-order value".to_string()
            }
            InputProfileError::BadChannelOrderCount {
                channels,
                groups_total,
            } => format!("channel-order groups total {groups_total} != media channels {channels}"),
            InputProfileError::UnknownChannelSymbol { symbol } => {
                format!("unknown SMPTE2110 channel-order symbol \"{symbol}\"")
            }
            InputProfileError::ChannelGroupMismatch {
                symbol,
                channels,
                expected_channels,
            } => format!(
                "channel group \"{symbol}\" declares {channels} channels but the symbol defines {expected_channels}"
            ),
            InputProfileError::NotMulticast { destination } => format!(
                "destination {destination} is not IPv4 multicast; SAP-announced inputs must be multicast"
            ),
            InputProfileError::BadSelector { spec } => format!(
                "invalid input selector \"{spec}\" (expected \"*\", \"name:<substring>\" or \"<ipv4>:<port>\")"
            ),
        }
    }

    /// 底层 SDP 错误（仅 `Sdp` 变体；其余为输入层判定）。
    pub const fn sdp_error(&self) -> Option<SdpError> {
        match self {
            InputProfileError::Sdp(error) => Some(*error),
            _ => None,
        }
    }
}

impl std::fmt::Display for InputProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.name(), self.message())
    }
}

impl std::error::Error for InputProfileError {}

// ---------------------------------------------------------------------------
// 确定性 JSON 输出
// ---------------------------------------------------------------------------

/// 极简确定性 JSON 对象写入器（固定键顺序；字符串按 JSON 规则转义）。
struct JsonObject {
    buffer: String,
    empty: bool,
}

impl JsonObject {
    fn new() -> Self {
        Self {
            buffer: String::from("{"),
            empty: true,
        }
    }

    fn key(&mut self, key: &str) {
        if !self.empty {
            self.buffer.push(',');
        }
        json_escape_into(&mut self.buffer, key);
        self.buffer.push(':');
        self.empty = false;
    }

    fn raw(&mut self, value: &str) {
        self.buffer.push_str(value);
    }

    fn string(&mut self, value: &str) {
        json_escape_into(&mut self.buffer, value);
    }

    fn opt_string(&mut self, value: Option<&str>) {
        match value {
            Some(value) => json_escape_into(&mut self.buffer, value),
            None => self.buffer.push_str("null"),
        }
    }

    fn number(&mut self, value: u64) {
        let _ = write!(self.buffer, "{value}");
    }

    fn opt_number(&mut self, value: Option<u64>) {
        match value {
            Some(value) => self.number(value),
            None => self.buffer.push_str("null"),
        }
    }

    fn boolean(&mut self, value: bool) {
        self.buffer.push_str(if value { "true" } else { "false" });
    }

    fn finish(mut self) -> String {
        self.buffer.push('}');
        self.buffer
    }
}

/// JSON 字符串转义（UTF-8 直通；控制字符 `\u00XX`）。
fn json_escape_into(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// 小写 hex、用 `-` 分组的字节串（EUI-64 / MAC）。
fn format_hex_dashed(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            text.push('-');
        }
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn refclk_json(refclk: RefClk) -> String {
    let mut object = JsonObject::new();
    object.key("kind");
    match refclk {
        RefClk::Ptp(ptp) => {
            object.string("ptp");
            object.key("domain");
            object.number(u64::from(ptp.domain));
            object.key("has_grandmaster");
            object.boolean(ptp.has_grandmaster);
            object.key("grandmaster");
            if ptp.has_grandmaster {
                object.string(&format_hex_dashed(&ptp.grandmaster));
            } else {
                object.raw("null");
            }
        }
        RefClk::Local => object.string("local"),
        RefClk::LocalMac(mac) => {
            object.string("localmac");
            object.key("mac");
            object.string(&format_hex_dashed(&mac));
        }
    }
    object.finish()
}

fn mediaclk_json(mediaclk: MediaClk) -> String {
    let mut object = JsonObject::new();
    object.key("direct");
    object.boolean(mediaclk.direct);
    object.key("offset_sec");
    object.number(u64::from(mediaclk.offset_sec));
    object.finish()
}

fn groups_json(groups: &[ChannelGroup]) -> String {
    let mut out = String::from("[");
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let mut object = JsonObject::new();
        object.key("symbol");
        object.string(&group.symbol);
        object.key("channels");
        object.number(u64::from(group.channels));
        object.key("names");
        if group.names.is_empty() {
            object.raw("null");
        } else {
            let mut names = String::from("[");
            for (name_index, name) in group.names.iter().enumerate() {
                if name_index > 0 {
                    names.push(',');
                }
                json_escape_into(&mut names, name);
            }
            names.push(']');
            object.raw(&names);
        }
        out.push_str(&object.finish());
    }
    out.push(']');
    out
}

fn positions_json(positions: &[ChannelPosition]) -> String {
    let mut out = String::from("[");
    for (index, position) in positions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        json_escape_into(&mut out, &position.label());
    }
    out.push(']');
    out
}

fn channel_order_json(order: &ChannelOrder) -> String {
    let mut object = JsonObject::new();
    object.key("raw");
    object.string(&order.raw);
    object.key("groups");
    object.raw(&groups_json(&order.groups));
    object.key("total_channels");
    object.number(u64::from(order.total_channels()));
    object.key("positions");
    match canonical_positions(order) {
        Ok(positions) => object.raw(&positions_json(&positions)),
        Err(error) => {
            object.raw("null");
            object.key("positions_error");
            object.string(error.name());
            object.key("positions_error_message");
            object.string(&error.message());
        }
    }
    object.finish()
}

// ---------------------------------------------------------------------------
// 内嵌最小 SHA-256（AES67 通告对账用，确定性 sdp_sha256）
// ---------------------------------------------------------------------------

/// 最小 SHA-256（单向散列 + 小写 hex；与 `crates/core/src/sha256.rs` 同实现）。
/// 本 crate 保持纯 std 零第三方依赖，故内嵌而不新增 path 依赖。
mod sha256 {
    use std::fmt::Write as _;

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    fn compress(state: &mut [u32; 8], block: &[u8]) {
        let mut schedule = [0u32; 64];
        for index in 0..16 {
            schedule[index] = u32::from_be_bytes([
                block[index * 4],
                block[index * 4 + 1],
                block[index * 4 + 2],
                block[index * 4 + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    fn digest(data: &[u8]) -> [u8; 32] {
        let mut state = H0;
        let mut offset = 0usize;
        while offset + 64 <= data.len() {
            compress(&mut state, &data[offset..offset + 64]);
            offset += 64;
        }
        let mut tail = Vec::with_capacity(128);
        tail.extend_from_slice(&data[offset..]);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        let mut index = 0;
        while index < tail.len() {
            compress(&mut state, &tail[index..index + 64]);
            index += 64;
        }
        let mut out = [0u8; 32];
        for (word_index, word) in state.iter().enumerate() {
            out[word_index * 4..word_index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// 小写 hex 摘要。
    pub(super) fn hex(data: &[u8]) -> String {
        let digest = digest(data);
        let mut text = String::with_capacity(64);
        for byte in digest {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::Ipv4Addr;

    use crate::sap::{SapKey, SapSession};
    use crate::sdp::tests::{H16_SDP, NSP_16CH_FLOW_SDP, NSP_412_SDP};
    use crate::sdp::{self, Direction, Encoding, RefClk, SdpError};

    use super::sha256;

    /// 基线 SDP：组播 + L24/48000/2 + ptime 1 + PTP refclk。
    const STEREO_SDP: &str = "v=0\r\n\
o=- 1 1 IN IP4 192.0.2.10\r\n\
s=stereo\r\n\
c=IN IP4 239.69.1.1/32\r\n\
t=0 0\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/2\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";

    /// channel-order 分组和（8）≠ 媒体通道数（7）。
    const ORDER_MISMATCH_SDP: &str = "v=0\r\n\
c=IN IP4 239.69.1.1/32\r\n\
m=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 L24/48000/7\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n\
a=fmtp:96 channel-order=SMPTE2110.(71)\r\n";

    /// 静态 payload type（0）：AES67 要求动态 96..=127。
    const STATIC_PAYLOAD_SDP: &str = "v=0\r\n\
c=IN IP4 239.69.1.1/32\r\n\
m=audio 5004 RTP/AVP 0\r\n\
a=rtpmap:0 L16/48000/2\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:0\r\n";

    fn sap_session(originator: [u8; 4], msg_id_hash: u16, sdp: &str) -> SapSession {
        SapSession {
            key: SapKey {
                originator: Ipv4Addr::from(originator),
                msg_id_hash,
            },
            sdp: sdp.to_string(),
            origin_address: None,
            profile_error: None,
            received_ms: 0,
            announce_period_ms: 30_000,
        }
    }

    fn three_session_snapshot() -> Vec<SapSession> {
        vec![
            sap_session([192, 0, 2, 10], 0x0010, NSP_412_SDP),
            sap_session([192, 0, 2, 20], 0x0020, NSP_16CH_FLOW_SDP),
            sap_session([192, 0, 2, 30], 0x0030, H16_SDP),
        ]
    }

    fn assert_order_labels(raw: &str, expected: &[&str]) {
        let order = ChannelOrder::parse(raw).expect("channel-order parses");
        let actual: Vec<String> = canonical_positions(&order)
            .expect("canonical positions")
            .iter()
            .map(ChannelPosition::label)
            .collect();
        let expected: Vec<String> = expected.iter().map(|label| (*label).to_string()).collect();
        assert_eq!(actual, expected, "channel-order {raw}");
    }

    // -----------------------------------------------------------------------
    // SDP → InputProfile（全字段 / 组播单播 / ptime 1ms 125us / framecount）
    // -----------------------------------------------------------------------

    #[test]
    fn h16_flow_profile_carries_every_descriptor_field() {
        let session = sdp::parse_sdp(H16_SDP).expect("H16 SDP parses");
        let profile = InputProfile::from_session(&session).expect("H16 profile");
        assert_eq!(
            profile.session_name.as_deref(),
            Some("NuPrime-a227-5240:16")
        );
        assert_eq!(profile.media, "audio");
        assert_eq!(profile.destination, Ipv4Addr::new(239, 69, 16, 146));
        assert!(profile.multicast);
        assert_eq!(profile.port, 5_004);
        assert_eq!(profile.payload_type, 103);
        assert_eq!(profile.encoding, Encoding::L24);
        assert_eq!(profile.sample_rate_hz, 48_000);
        assert_eq!(profile.channels, 8);
        assert_eq!(profile.ptime_us, Some(1_000));
        assert_eq!(profile.framecount, None);
        assert_eq!(profile.channel_order, None);
        match profile.refclk.expect("ptp refclk") {
            RefClk::Ptp(ptp) => {
                assert_eq!(ptp.domain, 0);
                assert!(ptp.has_grandmaster);
                assert_eq!(ptp.grandmaster[0], 0x00);
                assert_eq!(ptp.grandmaster[7], 0x77);
            }
            other => panic!("expected ptp refclk, got {other:?}"),
        }
        let mediaclk = profile.mediaclk.expect("mediaclk");
        assert!(mediaclk.direct);
        assert_eq!(mediaclk.offset_sec, 0);
        assert_eq!(profile.direction, Direction::SendRecv);
        assert_eq!(profile.origin_address, Some(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(profile.effective_ptime_us(), Some(1_000));
        assert_eq!(profile.packet_frames(), Some(48));
        validate_receiver(&profile, &ReceiverLimits::default()).expect("default limits accept H16");
        // from_session 与 from_sdp_text 两条入口必须得到同一描述符。
        assert_eq!(
            InputProfile::from_sdp_text(H16_SDP).expect("text entry"),
            profile
        );
    }

    #[test]
    fn nsp_412_unicast_profile_keeps_structured_channel_order() {
        let profile = InputProfile::from_sdp_text(NSP_412_SDP).expect("NSP 4.1.2 profile");
        assert_eq!(
            profile.session_name.as_deref(),
            Some("NativeSpatialPipe AES67 output")
        );
        assert_eq!(profile.destination, Ipv4Addr::new(192, 0, 2, 222));
        assert!(!profile.multicast);
        assert_eq!(profile.port, 5_004);
        assert_eq!(profile.payload_type, 96);
        assert_eq!(profile.encoding, Encoding::L24);
        assert_eq!(profile.sample_rate_hz, 48_000);
        assert_eq!(profile.channels, 7);
        assert_eq!(profile.ptime_us, Some(1_000));
        assert_eq!(profile.framecount, None);
        assert_eq!(profile.direction, Direction::SendOnly);
        assert_eq!(profile.origin_address, Some(Ipv4Addr::UNSPECIFIED));
        let order = profile.channel_order.as_ref().expect("channel-order");
        assert_eq!(order.raw, "SMPTE2110.(41,2)");
        let symbols: Vec<&str> = order
            .groups
            .iter()
            .map(|group| group.symbol.as_str())
            .collect();
        assert_eq!(symbols, ["41", "2"]);
        assert_eq!(order.groups[0].channels, 5);
        assert_eq!(order.groups[1].channels, 2);
        assert!(order.groups.iter().all(|group| group.names.is_empty()));
        assert!(order.groups.iter().all(|group| group.labels().is_empty()));
        assert_eq!(order.total_channels(), 7);
        let labels: Vec<String> = canonical_positions(order)
            .expect("positions")
            .iter()
            .map(ChannelPosition::label)
            .collect();
        assert_eq!(
            labels,
            ["CH01", "CH02", "CH03", "CH04", "CH05", "CH06", "CH07"]
        );
        assert_eq!(
            profile.refclk,
            Some(RefClk::LocalMac([0x02, 0x4e, 0x53, 0x50, 0x00, 0x01]))
        );
        assert!(profile.mediaclk.expect("mediaclk").direct);
        assert_eq!(profile.packet_frames(), Some(48));
        validate_receiver(&profile, &ReceiverLimits::default())
            .expect("unicast loopback profile accepted with default limits");
    }

    #[test]
    fn nsp_16ch_flow_local_refclk_and_sender_mediaclk() {
        let profile =
            InputProfile::from_sdp_text(NSP_16CH_FLOW_SDP).expect("NSP 16ch flow profile");
        assert_eq!(
            profile.session_name.as_deref(),
            Some("NativeSpatialPipe AES67 output flow0")
        );
        assert_eq!(profile.port, 5_010);
        assert_eq!(profile.channels, 8);
        assert!(!profile.multicast);
        assert_eq!(profile.refclk, Some(RefClk::Local));
        let mediaclk = profile.mediaclk.expect("mediaclk");
        assert!(
            !mediaclk.direct,
            "mediaclk:sender is not a direct media clock"
        );
        validate_receiver(&profile, &ReceiverLimits::default())
            .expect("local refclk + sender mediaclk accepted");
    }

    #[test]
    fn ptime_125us_and_framecount_derive_packet_framing() {
        let both = STEREO_SDP.replace("a=ptime:1", "a=ptime:0.125\r\na=framecount:6");
        let profile = InputProfile::from_sdp_text(&both).expect("125us profile");
        assert_eq!(profile.ptime_us, Some(125));
        assert_eq!(profile.framecount, Some(6));
        assert_eq!(profile.effective_ptime_us(), Some(125));
        assert_eq!(profile.packet_frames(), Some(6));
        validate_receiver(&profile, &ReceiverLimits::default())
            .expect("default limits accept 125us");

        let framecount_only = STEREO_SDP.replace("a=ptime:1\r\n", "a=framecount:6\r\n");
        let profile =
            InputProfile::from_sdp_text(&framecount_only).expect("framecount-only profile");
        assert_eq!(profile.ptime_us, None);
        assert_eq!(profile.effective_ptime_us(), Some(125));
        assert_eq!(profile.packet_frames(), Some(6));
        validate_receiver(&profile, &ReceiverLimits::default())
            .expect("framecount-only framing resolves to 125us");
    }

    #[test]
    fn inconsistent_ptime_and_framecount_has_stable_code() {
        let sdp = STEREO_SDP.replace("a=ptime:1", "a=ptime:1\r\na=framecount:6");
        let error = InputProfile::from_sdp_text(&sdp).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_BAD_FRAMECOUNT");
        assert!(error.message().contains("framecount 6"), "{error}");
        assert!(error.message().contains("ptime 1000"), "{error}");
        assert_eq!(error.sdp_error(), None);
    }

    #[test]
    fn structural_errors_have_stable_codes() {
        let missing_connection = STEREO_SDP.replace("c=IN IP4 239.69.1.1/32\r\n", "");
        let port_zero = STEREO_SDP.replace("audio 5004", "audio 0");
        let channels_zero = STEREO_SDP.replace("48000/2", "48000/0");
        let non_audio = STEREO_SDP.replace("m=audio", "m=video");
        let recvonly = format!("{STEREO_SDP}a=recvonly\r\n");
        let unparsable = "v=0\r\ns=no-media\r\n".to_string();
        // SDP 层只接受 L16/L24；OPUS 等编码在解析期被拒，映射到输入层稳定码（不猜）。
        let non_linear_encoding = STEREO_SDP.replace("L24", "OPUS");
        let cases: [(&str, String); 9] = [
            ("E_INPUT_PROFILE_MISSING_CONNECTION", missing_connection),
            ("E_INPUT_PROFILE_BAD_MEDIA", port_zero),
            ("E_INPUT_PROFILE_BAD_CHANNELS", channels_zero),
            ("E_INPUT_PROFILE_NOT_AUDIO", non_audio),
            ("E_INPUT_PROFILE_BAD_DIRECTION", recvonly),
            ("E_INPUT_PROFILE_UNSUPPORTED_ENCODING", non_linear_encoding),
            (
                "E_INPUT_PROFILE_BAD_PAYLOAD",
                STATIC_PAYLOAD_SDP.to_string(),
            ),
            (
                "E_INPUT_PROFILE_BAD_CHANNEL_ORDER",
                ORDER_MISMATCH_SDP.to_string(),
            ),
            ("E_INPUT_PROFILE_BAD_SDP", unparsable),
        ];
        for (code, sdp) in cases {
            let error = InputProfile::from_sdp_text(&sdp).unwrap_err();
            assert_eq!(error.name(), code, "unexpected code for input {sdp:?}");
            assert!(!error.message().is_empty());
        }
        // 不可接受编码的消息必须说明 L16/L24 约束。
        let error = InputProfile::from_sdp_text(&STEREO_SDP.replace("L24", "OPUS")).unwrap_err();
        assert!(error.message().contains("L16/L24"), "{error}");
    }

    #[test]
    fn unparsable_sdp_maps_to_bad_sdp_with_source_code() {
        let error = InputProfile::from_sdp_text("v=0\r\n").unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_BAD_SDP");
        assert_eq!(error.sdp_error(), Some(SdpError::MissingMedia));
        assert_eq!(
            error.to_string(),
            "E_INPUT_PROFILE_BAD_SDP: SDP parse failed: ERR_SDP_NO_MEDIA"
        );
    }

    // -----------------------------------------------------------------------
    // channel-order：符号映射 / 数字扩展 / 未知符号
    // -----------------------------------------------------------------------

    #[test]
    fn channel_order_known_symbols_map_to_canonical_positions() {
        assert_order_labels("SMPTE2110.(ST)", &["L", "R"]);
        assert_order_labels("SMPTE2110.(51)", &["L", "R", "C", "LFE", "Ls", "Rs"]);
        assert_order_labels(
            "SMPTE2110.(71)",
            &["L", "R", "C", "LFE", "Lss", "Rss", "Lrs", "Rrs"],
        );
        assert_order_labels("SMPTE2110.(M,DM,LtRt)", &["M1", "M1", "M2", "Lt", "Rt"]);
        assert_order_labels("SMPTE2110.(U05)", &["U05"]);
        assert_order_labels("SMPTE2110.(SGRP)", &["CH01", "CH02", "CH03", "CH04"]);
        assert_order_labels(
            "SMPTE2110.(41,2)",
            &["CH01", "CH02", "CH03", "CH04", "CH05", "CH06", "CH07"],
        );
        let positions = canonical_positions(&ChannelOrder::parse("SMPTE2110.(222)").unwrap())
            .expect("positions");
        assert_eq!(positions.len(), 24);
        assert_eq!(
            positions.first().map(ChannelPosition::label).as_deref(),
            Some("CH01")
        );
        assert_eq!(
            positions.last().map(ChannelPosition::label).as_deref(),
            Some("CH24")
        );
    }

    #[test]
    fn channel_order_parse_tracks_group_structure() {
        let order = ChannelOrder::parse("SMPTE2110.(41,2)").expect("numeric extension parses");
        assert_eq!(order.raw, "SMPTE2110.(41,2)");
        assert_eq!(order.total_channels(), 7);
        let groups: Vec<(&str, u16)> = order
            .groups
            .iter()
            .map(|group| (group.symbol.as_str(), group.channels))
            .collect();
        assert_eq!(groups, [("41", 5), ("2", 2)]);
        assert!(order.groups.iter().all(|group| group.names.is_empty()));

        let order = ChannelOrder::parse("SMPTE2110.(71,ST)").expect("named groups parse");
        assert_eq!(order.total_channels(), 10);
        let names: Vec<&str> = order.groups[0].names.iter().map(String::as_str).collect();
        assert_eq!(names, ["L", "R", "C", "LFE", "Lss", "Rss", "Lrs", "Rrs"]);
        let stereo_names: Vec<&str> = order.groups[1].names.iter().map(String::as_str).collect();
        assert_eq!(stereo_names, ["L", "R"]);
    }

    #[test]
    fn channel_order_parse_rejects_bad_values_with_stable_codes() {
        let syntax_cases = [
            "",
            "71",
            "SMPTE2110.71",
            "SMPTE2110.()",
            "SMPTE2110.(71,)",
            "SMPTE2110.(00)",
            "SMPTE2110.(222,222,222)",
        ];
        for raw in syntax_cases {
            let error = ChannelOrder::parse(raw).unwrap_err();
            assert_eq!(
                error.name(),
                "E_INPUT_PROFILE_BAD_CHANNEL_ORDER",
                "raw: {raw}"
            );
        }
        let too_many_groups = format!("SMPTE2110.({})", ["M"; 17].join(","));
        assert_eq!(
            ChannelOrder::parse(&too_many_groups).unwrap_err().name(),
            "E_INPUT_PROFILE_BAD_CHANNEL_ORDER"
        );
        let unknown = ChannelOrder::parse("SMPTE2110.(X9)").unwrap_err();
        assert_eq!(unknown.name(), "E_INPUT_PROFILE_UNKNOWN_CHANNEL_SYMBOL");
        assert_eq!(
            unknown.message(),
            "unknown SMPTE2110 channel-order symbol \"X9\""
        );
    }

    #[test]
    fn canonical_positions_rejects_unknown_symbol_and_group_mismatch() {
        let unknown = ChannelOrder {
            raw: "SMPTE2110.(X9)".to_string(),
            groups: vec![ChannelGroup {
                symbol: "X9".to_string(),
                channels: 2,
                names: Vec::new(),
            }],
        };
        assert_eq!(
            canonical_positions(&unknown).unwrap_err().name(),
            "E_INPUT_PROFILE_UNKNOWN_CHANNEL_SYMBOL"
        );
        let mismatch = ChannelOrder {
            raw: "SMPTE2110.(71)".to_string(),
            groups: vec![ChannelGroup {
                symbol: "71".to_string(),
                channels: 2,
                names: Vec::new(),
            }],
        };
        let error = canonical_positions(&mismatch).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_CHANNEL_GROUP_MISMATCH");
        assert_eq!(
            error.message(),
            "channel group \"71\" declares 2 channels but the symbol defines 8"
        );
    }

    // -----------------------------------------------------------------------
    // 接收端验收（可注入限制）
    // -----------------------------------------------------------------------

    #[test]
    fn receiver_limits_defaults_match_contract() {
        let limits = ReceiverLimits::default();
        assert_eq!(limits.max_channels, 64);
        assert_eq!(limits.allowed_rates_hz, [48_000]);
        assert_eq!(limits.allowed_encodings, [Encoding::L16, Encoding::L24]);
        assert!(limits.require_ts_refclk);
        assert_eq!(limits.allowed_ptimes_us, [125, 1_000]);
    }

    #[test]
    fn missing_refclk_is_policy_error() {
        let sdp = STEREO_SDP.replace("a=ts-refclk:ptp=IEEE1588-2008:0\r\n", "");
        let profile = InputProfile::from_sdp_text(&sdp).expect("structure without refclk is valid");
        assert_eq!(profile.refclk, None);
        let error = validate_receiver(&profile, &ReceiverLimits::default()).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_MISSING_REFCLK");
        let relaxed = ReceiverLimits {
            require_ts_refclk: false,
            ..ReceiverLimits::default()
        };
        validate_receiver(&profile, &relaxed).expect("relaxed limits accept missing refclk");
    }

    #[test]
    fn validate_receiver_enforces_injectable_limits() {
        let h16 = InputProfile::from_sdp_text(H16_SDP).expect("H16 profile");
        let narrow_channels = ReceiverLimits {
            max_channels: 4,
            ..ReceiverLimits::default()
        };
        let error = validate_receiver(&h16, &narrow_channels).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_CHANNELS_EXCEED_LIMIT");
        assert!(error.message().contains('8'), "{error}");
        assert!(error.message().contains('4'), "{error}");

        let l16 =
            InputProfile::from_sdp_text(&STEREO_SDP.replace("L24", "L16")).expect("L16 profile");
        validate_receiver(&l16, &ReceiverLimits::default()).expect("L16 accepted by default");
        let l24_only = ReceiverLimits {
            allowed_encodings: vec![Encoding::L24],
            ..ReceiverLimits::default()
        };
        assert_eq!(
            validate_receiver(&l16, &l24_only).unwrap_err().name(),
            "E_INPUT_PROFILE_UNSUPPORTED_ENCODING"
        );

        let wide = InputProfile::from_sdp_text(&STEREO_SDP.replace("48000/2", "48000/65"))
            .expect("65ch parses");
        assert_eq!(wide.channels, 65);
        assert_eq!(
            validate_receiver(&wide, &ReceiverLimits::default())
                .unwrap_err()
                .name(),
            "E_INPUT_PROFILE_CHANNELS_EXCEED_LIMIT"
        );

        let hz_44100 = InputProfile::from_sdp_text(&STEREO_SDP.replace("48000", "44100"))
            .expect("44.1k parses structurally");
        let error = validate_receiver(&hz_44100, &ReceiverLimits::default()).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_UNSUPPORTED_RATE");
        assert!(error.message().contains("44100"), "{error}");
        let extended = ReceiverLimits {
            allowed_rates_hz: vec![44_100, 48_000],
            ..ReceiverLimits::default()
        };
        validate_receiver(&hz_44100, &extended).expect("injected rate set accepts 44.1k");
    }

    #[test]
    fn validate_receiver_enforces_ptime_set_and_framing() {
        let ptime_125 =
            InputProfile::from_sdp_text(&STEREO_SDP.replace("a=ptime:1", "a=ptime:0.125"))
                .expect("125us profile");
        let one_ms_only = ReceiverLimits {
            allowed_ptimes_us: vec![1_000],
            ..ReceiverLimits::default()
        };
        let error = validate_receiver(&ptime_125, &one_ms_only).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_PTIME_NOT_ALLOWED");
        assert!(error.message().contains("(declared by a=ptime)"), "{error}");

        let framecount_only =
            InputProfile::from_sdp_text(&STEREO_SDP.replace("a=ptime:1\r\n", "a=framecount:6\r\n"))
                .expect("framecount-only profile");
        let error = validate_receiver(&framecount_only, &one_ms_only).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_PTIME_NOT_ALLOWED");
        assert!(
            error.message().contains("(derived from a=framecount)"),
            "{error}"
        );

        let no_framing = InputProfile::from_sdp_text(&STEREO_SDP.replace("a=ptime:1\r\n", ""))
            .expect("no framing profile");
        assert_eq!(
            validate_receiver(&no_framing, &ReceiverLimits::default())
                .unwrap_err()
                .name(),
            "E_INPUT_PROFILE_MISSING_FRAMING"
        );
    }

    #[test]
    fn sap_requires_multicast_but_direct_sdp_allows_unicast() {
        let unicast = sap_session([192, 0, 2, 222], 0x1234, NSP_412_SDP);
        let error = InputProfile::from_sap_session(&unicast).unwrap_err();
        assert_eq!(error.name(), "E_INPUT_PROFILE_NOT_MULTICAST");
        assert!(error.message().contains("192.0.2.222"), "{error}");
        InputProfile::from_sdp_text(NSP_412_SDP).expect("direct SDP may be unicast");

        let multicast = sap_session([127, 0, 0, 1], 0x0001, H16_SDP);
        let profile = InputProfile::from_sap_session(&multicast).expect("multicast SAP profile");
        assert_eq!(profile.destination, Ipv4Addr::new(239, 69, 16, 146));
    }

    // -----------------------------------------------------------------------
    // SAP 快照选择：确定性 / 按名 / 按组 / 无匹配原因 / 不静默跳过
    // -----------------------------------------------------------------------

    #[test]
    fn selection_orders_entries_by_name_group_port() {
        let snapshot = three_session_snapshot();
        let outcome = select_session_report(&snapshot, &SelectionCriteria::all());
        assert_eq!(outcome.considered, 3);
        assert_eq!(outcome.unmatched, 0);
        assert_eq!(outcome.accepted(), 1);
        assert_eq!(outcome.rejected(), 2);
        let names: Vec<Option<&str>> = outcome
            .inputs
            .iter()
            .map(ResolvedInput::session_name)
            .collect();
        assert_eq!(
            names,
            [
                Some("NativeSpatialPipe AES67 output"),
                Some("NativeSpatialPipe AES67 output flow0"),
                Some("NuPrime-a227-5240:16"),
            ]
        );
        let accepted: Vec<bool> = outcome
            .inputs
            .iter()
            .map(ResolvedInput::is_accepted)
            .collect();
        assert_eq!(accepted, [false, false, true]);
        assert_eq!(
            outcome.reasons,
            ["2 matched session(s) rejected: E_INPUT_PROFILE_NOT_MULTICAST"]
        );

        // 快照输入顺序不影响结果（按 name/group/port 排序 + 稳定 tie-break）。
        let forward: Vec<String> = select_session(&snapshot, &SelectionCriteria::all())
            .iter()
            .map(ResolvedInput::to_json)
            .collect();
        let mut reversed = snapshot.clone();
        reversed.reverse();
        let backward: Vec<String> = select_session(&reversed, &SelectionCriteria::all())
            .iter()
            .map(ResolvedInput::to_json)
            .collect();
        assert_eq!(forward, backward);
    }

    #[test]
    fn selection_by_name_substring_and_group_port() {
        let snapshot = three_session_snapshot();
        let by_name = select_session(&snapshot, &SelectionCriteria::parse("name:flow0").unwrap());
        assert_eq!(by_name.len(), 1);
        assert_eq!(
            by_name[0].session_name(),
            Some("NativeSpatialPipe AES67 output flow0")
        );
        assert_eq!(
            by_name[0].rejection.as_ref().map(InputProfileError::name),
            Some("E_INPUT_PROFILE_NOT_MULTICAST")
        );

        let by_group = select_session(
            &snapshot,
            &SelectionCriteria::parse("239.69.16.146:5004").unwrap(),
        );
        assert_eq!(by_group.len(), 1);
        assert!(by_group[0].is_accepted());
        assert_eq!(
            by_group[0].destination(),
            Some(Ipv4Addr::new(239, 69, 16, 146))
        );
        assert_eq!(by_group[0].port(), Some(5_004));

        let outcome =
            select_session_report(&snapshot, &SelectionCriteria::parse("name:flow0").unwrap());
        assert_eq!(outcome.considered, 3);
        assert_eq!(outcome.unmatched, 2);
        assert!(outcome
            .reasons
            .iter()
            .any(|reason| reason.contains("excluded by selector")));
    }

    #[test]
    fn selection_no_match_and_empty_snapshot_report_reasons() {
        let snapshot = three_session_snapshot();
        let outcome = select_session_report(
            &snapshot,
            &SelectionCriteria::parse("name:missing").unwrap(),
        );
        assert!(outcome.inputs.is_empty());
        assert_eq!(outcome.unmatched, 3);
        assert!(outcome
            .reasons
            .iter()
            .any(|reason| reason.contains("no SAP session matched selector")));
        assert!(outcome
            .reasons
            .iter()
            .any(|reason| reason.contains("3 SAP session(s) excluded")));

        let empty = select_session_report(&[], &SelectionCriteria::all());
        assert!(empty.inputs.is_empty());
        assert_eq!(empty.considered, 0);
        assert_eq!(empty.reasons, ["SAP snapshot is empty"]);
    }

    #[test]
    fn selection_reports_unparsable_sdp_instead_of_skipping() {
        let broken = sap_session([192, 0, 2, 9], 0x0009, "v=0\r\ns=broken\r\n");
        let snapshot = [broken];
        let outcome = select_session_report(&snapshot, &SelectionCriteria::all());
        assert_eq!(outcome.inputs.len(), 1);
        assert!(outcome.inputs[0].profile.is_none());
        assert!(!outcome.inputs[0].is_accepted());
        let error = outcome.inputs[0].rejection.as_ref().expect("rejection");
        assert_eq!(error.name(), "E_INPUT_PROFILE_BAD_SDP");
        assert_eq!(error.sdp_error(), Some(SdpError::MissingMedia));
        assert_eq!(
            outcome.reasons,
            ["1 matched session(s) rejected: E_INPUT_PROFILE_BAD_SDP"]
        );

        // 选择器需要 SDP 元数据才能评估：不可解析条目被计数并解释，不静默。
        let outcome =
            select_session_report(&snapshot, &SelectionCriteria::parse("name:broken").unwrap());
        assert!(outcome.inputs.is_empty());
        assert_eq!(outcome.unmatched, 1);
        assert!(outcome
            .reasons
            .iter()
            .any(|reason| reason.contains("unparsable SDP")));
    }

    #[test]
    fn criteria_parse_validates_selector_text() {
        assert_eq!(
            SelectionCriteria::parse("*").unwrap(),
            SelectionCriteria::all()
        );
        assert_eq!(
            SelectionCriteria::parse("name:abc").unwrap().name,
            NameFilter::Contains("abc".to_string())
        );
        let group = SelectionCriteria::parse("239.69.16.146:5004").unwrap();
        assert_eq!(group.destination, Some(Ipv4Addr::new(239, 69, 16, 146)));
        assert_eq!(group.port, Some(5_004));
        for spec in [
            "",
            "name:",
            "not-an-ip:5004",
            "239.69.16.146",
            "300.1.1.1:5004",
        ] {
            let error = SelectionCriteria::parse(spec).unwrap_err();
            assert_eq!(error.name(), "E_INPUT_PROFILE_BAD_SELECTOR", "spec: {spec}");
        }
    }

    // -----------------------------------------------------------------------
    // 确定性 JSON
    // -----------------------------------------------------------------------

    #[test]
    fn input_profile_json_is_deterministic_and_complete() {
        let profile = InputProfile::from_sdp_text(H16_SDP).expect("H16 profile");
        let first = profile.to_json();
        let second = profile.to_json();
        assert_eq!(first, second);
        assert!(
            first.starts_with(
                "{\"session_name\":\"NuPrime-a227-5240:16\",\"media\":\"audio\",\"destination\":\"239.69.16.146\""
            ),
            "{first}"
        );
        assert!(first.contains("\"multicast\":true"));
        assert!(first.contains("\"payload_type\":103"));
        assert!(first.contains("\"encoding\":\"L24\""));
        assert!(first.contains("\"sample_rate_hz\":48000"));
        assert!(first.contains("\"channels\":8"));
        assert!(first.contains("\"ptime_us\":1000"));
        assert!(first.contains("\"framecount\":null"));
        assert!(first.contains("\"packet_frames\":48"));
        assert!(first.contains(
            "\"refclk\":{\"kind\":\"ptp\",\"domain\":0,\"has_grandmaster\":true,\"grandmaster\":\"00-11-22-33-44-55-66-77\"}"
        ));
        assert!(first.contains("\"mediaclk\":{\"direct\":true,\"offset_sec\":0}"));
        assert!(first.contains("\"channel_order\":null"));
        assert!(first.contains("\"direction\":\"sendrecv\""));
        assert!(first.ends_with('}'));

        let nsp = InputProfile::from_sdp_text(NSP_412_SDP)
            .expect("NSP profile")
            .to_json();
        assert!(nsp.contains("\"channel_order\":{\"raw\":\"SMPTE2110.(41,2)\""));
        assert!(nsp.contains(
            "\"positions\":[\"CH01\",\"CH02\",\"CH03\",\"CH04\",\"CH05\",\"CH06\",\"CH07\"]"
        ));
        assert!(nsp.contains("\"names\":null"));
        assert!(nsp.contains("\"kind\":\"localmac\",\"mac\":\"02-4e-53-50-00-01\""));
    }

    #[test]
    fn json_escapes_session_names() {
        let mut profile = InputProfile::from_sdp_text(H16_SDP).expect("H16 profile");
        profile.session_name = Some("quote\" backslash\\ newline\n".to_string());
        let json = profile.to_json();
        assert!(
            json.contains("quote\\\" backslash\\\\ newline\\n"),
            "{json}"
        );
    }

    #[test]
    fn resolved_input_json_carries_sha_key_and_rejection() {
        let rejected = [sap_session([192, 0, 2, 78], 0xbeef, NSP_412_SDP)];
        let input = &select_session(&rejected, &SelectionCriteria::all())[0];
        assert!(!input.is_accepted());
        assert_eq!(input.sdp_sha256.len(), 64);
        assert!(input
            .sdp_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        let json = input.to_json();
        assert_eq!(json, input.to_json());
        assert!(json.contains("\"key\":\"192.0.2.78:beef\""), "{json}");
        assert!(json.contains("\"accepted\":false"));
        assert!(json.contains("\"code\":\"E_INPUT_PROFILE_NOT_MULTICAST\""));
        assert!(json.contains("\"message\":\"destination 192.0.2.222 is not IPv4 multicast"));
        assert!(json.contains("\"sdp_sha256\":\""));

        let accepted = [sap_session([127, 0, 0, 1], 0x0001, H16_SDP)];
        let input = &select_session(&accepted, &SelectionCriteria::all())[0];
        assert!(input.is_accepted());
        let json = input.to_json();
        assert!(json.contains("\"accepted\":true"));
        assert!(json.contains("\"rejection\":null"));
        assert!(json.contains("\"profile\":{"));
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256::hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256::hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256::hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
