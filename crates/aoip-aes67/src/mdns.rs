//! mDNS / DNS-SD 子集（RAVENNA 设备/流发现面，P1）。
//!
//! 规范依据：
//! - AES67-2023 Annex E.6：RAVENNA 用
//!   DNS-SD over mDNS 广告/发现设备与流；发送端发布 RTSP 服务，接收端 browse 后经 RTSP 取 SDP。
//! - RAVENNA Operating Principles Draft 1.0：
//!   - DNS-SD + mDNS（无 DNS 服务器时 SHOULD 用 mDNS）：§3.5 L585-602；
//!   - 源节点 MUST 发布 RTSP 服务、sink 节点 MUST 发布 HTTP 配置服务：§3.5 L603-606；
//!   - 节点服务名 = 4 个标准类型实例 + ravenna 子类型：
//!     <vendor node id>._rtsp._tcp / _http._tcp 与 _ravenna._sub._http._tcp / _rtsp._tcp：§3.5.1 L607-623；
//!   - TXT 可选，SHOULD NOT 含服务协议拿不到的信息，MAY 放节点类型：§3.5.1.1 L628-632；
//!   - 会话以独立服务 <session name>._rtsp._tcp 发布，MUST 带 ravenna_session 子服务：§3.5.2 L640-649；
//!   - 名称冲突不应静默改名：§3.5.1.2 L633-639。
//! - RFC 6762 / RFC 6763 / RFC 1035：
//!   - mDNS 组地址 224.0.0.251、UDP 5353、.local. 域：RFC 6762 §3 L288-311、§19 L2824-2827；
//!   - 报文 = DNS 报文格式；QR/AA/OPCODE/RCODE 约束：RFC 6762 §18 L2603-2728；
//!   - 缓存冲洗位（rrclass 最高位）与应答附加记录：RFC 6762 §8.3 L1640-1665、§18.13 L2736-2742；
//!   - 记录 TTL：主机名记录 120 s、其它 75 min：RFC 6762 §10 L1816-1825；
//!   - goodbye（TTL 0）语义与 1 s 宽限：RFC 6762 §10.1 L1842-1867；
//!   - DNS-SD：PTR 枚举实例、SRV+TXT 解析、TXT 必需（可为单个空串）、key=value 语法：
//!     RFC 6763 §4-§6 L287-650（TXT 必需 L586-592，空串等价 L637-649，key/value L685-708）；
//!   - 子类型：_sub 机制与基础类型 PTR 并存：RFC 6763 §7.1 L1141-1256；
//!   - 附加记录生成建议：RFC 6763 §12 L1631-1695；
//!   - DNS 报文/名字/压缩语法：RFC 1035 §4（本实现按 1 字节标签 + 0 结尾 + 14 位压缩指针）。
//!
//! 实现口径：
//! - 纯 std、零外部依赖；DNS 报文编解码（A/PTR/SRV/TXT，名字压缩）+ DNS-SD 服务模型 +
//!   mDNS 通告/响应线程 + 浏览/缓存（TTL 老化、goodbye 宽限）；
//! - 运行时可按 MdnsEndpoint 配置为真实组播（224.0.0.251:5353）或单播（离线测试/本机联调）；
//! - 不实现：探测/冲突改名（RFC 6762 §8.1/§9）、已知答案抑制（§7.1）、重复问题抑制（§7.3）、
//!   NSEC 否定应答、IPv6/AAAA、单播遗留应答（§6.7）。这些列为 NOT_RUN/EXTENSION，见 crate README。

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// mDNS 端口（RFC 6762 §19 L2826）。
pub const MDNS_PORT: u16 = 5353;
/// mDNS IPv4 链路本地组播地址（RFC 6762 §3 L310-312）。
pub const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// .local. 域（RFC 6762 §3 L302-310）。
pub const MDNS_DOMAIN: &str = "local.";
/// 主机名类记录 TTL（A/SRV：RFC 6762 §10 L1818-1822）。
pub const MDNS_TTL_HOST: u32 = 120;
/// 其它记录 TTL（PTR/TXT：RFC 6762 §10 L1824-1825，75 分钟）。
pub const MDNS_TTL_OTHER: u32 = 4500;
/// goodbye 宽限（RFC 6762 §10.1 L1860-1867：收到 TTL 0 后按 1 s 宽限删除）。
pub const MDNS_GOODBYE_GRACE_MS: u64 = 1_000;
/// mDNS 报文上限（RFC 6763 §6.1 L609-611：包括 IP/UDP/DNS 头约 9000 字节）。
pub const MDNS_MAX_MESSAGE_BYTES: usize = 9_000;
/// RAVENNA 节点子类型标签（OP §3.5.1 L618-621）。
pub const RAVENNA_SUBTYPE_NODE: &str = "_ravenna";
/// RAVENNA 会话子类型标签（OP §3.5.2.1 L649）。
pub const RAVENNA_SUBTYPE_SESSION: &str = "_ravenna_session";
/// RTSP 基础服务类型（OP §3.5.1 L614-617）。
pub const SERVICE_RTSP_TCP: &str = "_rtsp._tcp.local.";
/// HTTP 基础服务类型（OP §3.5.1 L614-617）。
pub const SERVICE_HTTP_TCP: &str = "_http._tcp.local.";
/// DNS 类型 A（RFC 1035 §3.2.2）。
pub const TYPE_A: u16 = 1;
/// DNS 类型 PTR。
pub const TYPE_PTR: u16 = 12;
/// DNS 类型 TXT。
pub const TYPE_TXT: u16 = 16;
/// DNS 类型 SRV（RFC 2782）。
pub const TYPE_SRV: u16 = 33;
/// DNS 类型 ANY（RFC 1035 §3.2.3）。
pub const TYPE_ANY: u16 = 255;
/// DNS class IN。
pub const CLASS_IN: u16 = 1;
/// 缓存冲洗位（mDNS 应答 rrclass 最高位，RFC 6762 §18.13 L2736-2742）。
pub const CLASS_CACHE_FLUSH: u16 = 0x8000;
/// 单播应答请求位（mDNS 问题 qclass 最高位，RFC 6762 §5.4/§18.12 L2730-2734）。
pub const CLASS_UNICAST_RESPONSE: u16 = 0x8000;

/// mDNS 错误（稳定错误名）。
#[derive(Debug)]
pub enum MdnsError {
    /// 报文过短/截断。
    Truncated,
    /// 名字非法（标签越界/超长/非法转义）。
    BadName,
    /// 压缩指针环/越界。
    BadPointer,
    /// 记录 rdata 非法。
    BadRecord,
    /// TXT 非法。
    BadTxt,
    /// SRV 非法。
    BadSrv,
    /// 报文过大。
    TooLarge,
    /// I/O 错误。
    Io(io::Error),
    /// 配置错误。
    Config(&'static str),
    /// 线程已停止。
    Stopped,
}

impl MdnsError {
    /// 稳定错误名。
    pub const fn name(&self) -> &'static str {
        match self {
            MdnsError::Truncated => "ERR_MDNS_TRUNCATED",
            MdnsError::BadName => "ERR_MDNS_BAD_NAME",
            MdnsError::BadPointer => "ERR_MDNS_BAD_POINTER",
            MdnsError::BadRecord => "ERR_MDNS_BAD_RECORD",
            MdnsError::BadTxt => "ERR_MDNS_BAD_TXT",
            MdnsError::BadSrv => "ERR_MDNS_BAD_SRV",
            MdnsError::TooLarge => "ERR_MDNS_TOO_LARGE",
            MdnsError::Io(_) => "ERR_MDNS_IO",
            MdnsError::Config(_) => "ERR_MDNS_CONFIG",
            MdnsError::Stopped => "ERR_MDNS_STOPPED",
        }
    }
}

impl fmt::Display for MdnsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MdnsError::Truncated => f.write_str("mdns message truncated"),
            MdnsError::BadName => f.write_str("mdns bad name"),
            MdnsError::BadPointer => f.write_str("mdns bad compression pointer"),
            MdnsError::BadRecord => f.write_str("mdns bad record"),
            MdnsError::BadTxt => f.write_str("mdns bad txt"),
            MdnsError::BadSrv => f.write_str("mdns bad srv"),
            MdnsError::TooLarge => f.write_str("mdns message too large"),
            MdnsError::Io(e) => write!(f, "mdns io: {e}"),
            MdnsError::Config(what) => write!(f, "mdns config: {what}"),
            MdnsError::Stopped => f.write_str("mdns stopped"),
        }
    }
}

impl std::error::Error for MdnsError {}

impl From<io::Error> for MdnsError {
    fn from(error: io::Error) -> Self {
        MdnsError::Io(error)
    }
}

type MdnsResult<T> = Result<T, MdnsError>;

fn to_lower_ascii(label: &[u8]) -> Vec<u8> {
    label
        .iter()
        .map(|byte| {
            if byte.is_ascii_uppercase() {
                byte + 32
            } else {
                *byte
            }
        })
        .collect()
}

/// 一个 DNS 名字（标签序列；显示为以 . 结尾的绝对名）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DnsName {
    labels: Vec<Vec<u8>>,
}

impl DnsName {
    /// 从文本构造（允许结尾 .；根为 "." 或 ""）。
    ///
    /// 有意不用 std::str::FromStr：本解析器返回 MdnsError（DNS 报错口径）而非通用错误类型；
    /// 名字只用于 DNS-SD，不接受 Punycode/IDNA 等扩展。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> MdnsResult<Self> {
        if text.is_empty() || text == "." {
            return Ok(Self { labels: Vec::new() });
        }
        let trimmed = text.strip_suffix('.').unwrap_or(text);
        if trimmed.is_empty() {
            return Ok(Self { labels: Vec::new() });
        }
        let mut labels = Vec::new();
        let mut total = 1;
        for label in trimmed.split('.') {
            if label.is_empty() {
                return Err(MdnsError::BadName);
            }
            if label.len() > 63 {
                return Err(MdnsError::BadName);
            }
            total += label.len() + 1;
            labels.push(label.as_bytes().to_vec());
        }
        if total > 255 {
            return Err(MdnsError::BadName);
        }
        Ok(Self { labels })
    }

    /// 标签视图。
    pub fn labels(&self) -> &[Vec<u8>] {
        &self.labels
    }

    /// 绝对名文本（以 . 结尾）。
    pub fn to_absolute(&self) -> String {
        let mut out = String::new();
        for label in &self.labels {
            out.push_str(&String::from_utf8_lossy(label));
            out.push('.');
        }
        if out.is_empty() {
            out.push('.');
        }
        out
    }

    /// 第一个标签（DNS-SD 实例名部分）。
    pub fn first_label(&self) -> Option<String> {
        self.labels
            .first()
            .map(|label| String::from_utf8_lossy(label).into_owned())
    }

    /// 大小写不敏感比较（RFC 6762 §16 L2532-2536）。
    pub fn eq_ignore_case(&self, other: &DnsName) -> bool {
        if self.labels.len() != other.labels.len() {
            return false;
        }
        self.labels
            .iter()
            .zip(other.labels.iter())
            .all(|(left, right)| to_lower_ascii(left) == to_lower_ascii(right))
    }

    /// 以基础服务类型构造子类型名：<subtype>._sub.<service_type>（RFC 6763 §7.1 L1156-1157）。
    pub fn subtype(subtype: &str, service_type: &DnsName) -> MdnsResult<Self> {
        let subtype_trimmed = subtype.strip_suffix('.').unwrap_or(subtype);
        let mut text = String::new();
        text.push_str(subtype_trimmed);
        text.push_str("._sub.");
        text.push_str(&service_type.to_absolute());
        Self::from_str(&text)
    }

    /// 把实例名拼到服务类型前：<instance>.<service_type>（RFC 6763 §4.1 L343-347）。
    pub fn with_instance(instance: &str, service_type: &DnsName) -> MdnsResult<Self> {
        let mut text = String::new();
        text.push_str(instance);
        text.push('.');
        text.push_str(&service_type.to_absolute());
        Self::from_str(&text)
    }
}

impl fmt::Display for DnsName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_absolute())
    }
}

fn decode_name(buffer: &[u8], mut offset: usize) -> MdnsResult<(DnsName, usize)> {
    let mut labels: Vec<Vec<u8>> = Vec::new();
    let mut jumps = 0usize;
    let mut total = 1usize;
    let mut end_offset: Option<usize> = None;
    loop {
        if offset >= buffer.len() {
            return Err(MdnsError::Truncated);
        }
        let length = buffer[offset];
        if length & 0xC0 == 0xC0 {
            if offset + 1 >= buffer.len() {
                return Err(MdnsError::Truncated);
            }
            let target = (((length & 0x3F) as usize) << 8) | buffer[offset + 1] as usize;
            if end_offset.is_none() {
                end_offset = Some(offset + 2);
            }
            jumps += 1;
            if jumps > 128 {
                return Err(MdnsError::BadPointer);
            }
            offset = target;
            continue;
        }
        if length & 0xC0 != 0 {
            return Err(MdnsError::BadName);
        }
        offset += 1;
        let length = length as usize;
        if length == 0 {
            break;
        }
        if offset + length > buffer.len() {
            return Err(MdnsError::Truncated);
        }
        total += length + 1;
        if total > 255 {
            return Err(MdnsError::BadName);
        }
        labels.push(buffer[offset..offset + length].to_vec());
        offset += length;
    }
    Ok((DnsName { labels }, end_offset.unwrap_or(offset)))
}

fn encode_name(out: &mut Vec<u8>, name: &DnsName, compression: &mut HashMap<String, u16>) {
    for index in 0..name.labels.len() {
        let key: String = name.labels[index..]
            .iter()
            .map(|label| String::from_utf8_lossy(&to_lower_ascii(label)).into_owned())
            .collect::<Vec<_>>()
            .join(".");
        if let Some(position) = compression.get(&key).copied() {
            out.push(0xC0 | ((position >> 8) as u8));
            out.push((position & 0xFF) as u8);
            return;
        }
        if out.len() < 0x4000 {
            compression.insert(key, out.len() as u16);
        }
        let label = &name.labels[index];
        out.push(label.len() as u8);
        out.extend_from_slice(label);
    }
    out.push(0);
}

/// 一条 DNS 问题。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsQuestion {
    /// 名字。
    pub name: DnsName,
    /// 类型（A/PTR/SRV/TXT/ANY）。
    pub qtype: u16,
    /// class 原值（可能含单播应答位）。
    pub qclass: u16,
}

impl DnsQuestion {
    /// 构造 IN 问题。
    pub fn in_class(name: DnsName, qtype: u16) -> Self {
        Self {
            name,
            qtype,
            qclass: CLASS_IN,
        }
    }

    /// class 去掉高位。
    pub const fn class(&self) -> u16 {
        self.qclass & !CLASS_UNICAST_RESPONSE
    }

    /// 是否请求单播应答（RFC 6762 §5.4）。
    pub const fn unicast_response(&self) -> bool {
        self.qclass & CLASS_UNICAST_RESPONSE != 0
    }
}

/// 记录 rdata（本子集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RData {
    /// A 记录 IPv4 地址。
    A(Ipv4Addr),
    /// PTR 记录目标名。
    Ptr(DnsName),
    /// TXT 记录字符串序列（每个 ≤255，可空）。
    Txt(Vec<Vec<u8>>),
    /// SRV 记录。
    Srv {
        /// 优先级。
        priority: u16,
        /// 权重。
        weight: u16,
        /// 端口。
        port: u16,
        /// 目标主机名。
        target: DnsName,
    },
    /// 其它类型原始 rdata。
    Unknown(Vec<u8>),
}

/// 一条 DNS 资源记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsRecord {
    /// 名字。
    pub name: DnsName,
    /// 类型。
    pub rtype: u16,
    /// class 原值（可能含缓存冲洗位）。
    pub rclass: u16,
    /// TTL（秒；0 = goodbye）。
    pub ttl: u32,
    /// rdata。
    pub rdata: RData,
}

impl DnsRecord {
    /// class 去掉高位。
    pub const fn class(&self) -> u16 {
        self.rclass & !CLASS_CACHE_FLUSH
    }

    /// 是否带缓存冲洗位（RFC 6762 §18.13）。
    pub const fn cache_flush(&self) -> bool {
        self.rclass & CLASS_CACHE_FLUSH != 0
    }

    /// 是否为 goodbye（TTL 0，RFC 6762 §10.1）。
    pub const fn is_goodbye(&self) -> bool {
        self.ttl == 0
    }
}

fn read_u16(buffer: &[u8], offset: usize) -> MdnsResult<u16> {
    if offset + 2 > buffer.len() {
        return Err(MdnsError::Truncated);
    }
    Ok(u16::from_be_bytes([buffer[offset], buffer[offset + 1]]))
}

fn read_u32(buffer: &[u8], offset: usize) -> MdnsResult<u32> {
    if offset + 4 > buffer.len() {
        return Err(MdnsError::Truncated);
    }
    Ok(u32::from_be_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ]))
}

/// 一条 mDNS/DNS 报文（mDNS 子集：QUESTION + ANSWER/AUTHORITY/ADDITIONAL）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsMessage {
    /// 报文 id（mDNS 查询 SHOULD 0，应答 MUST 0，RFC 6762 §18.1 L2628-2645）。
    pub id: u16,
    /// 标志位原值（QR/OPCODE/AA/TC/RD/RA/Z/RCODE）。
    pub flags: u16,
    /// 问题。
    pub questions: Vec<DnsQuestion>,
    /// 应答。
    pub answers: Vec<DnsRecord>,
    /// 权威。
    pub authorities: Vec<DnsRecord>,
    /// 附加。
    pub additionals: Vec<DnsRecord>,
}

/// 报文标志位常量（RFC 1035 §4.1.1）。
pub const FLAG_QR: u16 = 0x8000;
/// 权威应答位。
pub const FLAG_AA: u16 = 0x0400;

impl DnsMessage {
    /// 空报文。
    pub fn new() -> Self {
        Self {
            id: 0,
            flags: 0,
            questions: Vec::new(),
            answers: Vec::new(),
            authorities: Vec::new(),
            additionals: Vec::new(),
        }
    }

    /// mDNS 查询（QR=0，ID=0，RFC 6762 §18.1/§18.2）。
    pub fn query(questions: Vec<DnsQuestion>) -> Self {
        Self {
            id: 0,
            flags: 0,
            questions,
            ..Self::new()
        }
    }

    /// mDNS 应答（QR=1、AA=1、ID=0：RFC 6762 §18.1/§18.2/§18.4）。
    pub fn response(answers: Vec<DnsRecord>, additionals: Vec<DnsRecord>) -> Self {
        Self {
            id: 0,
            flags: FLAG_QR | FLAG_AA,
            questions: Vec::new(),
            answers,
            authorities: Vec::new(),
            additionals,
        }
    }

    /// 是否应答。
    pub const fn is_response(&self) -> bool {
        self.flags & FLAG_QR != 0
    }

    /// 是否权威应答。
    pub const fn is_authoritative(&self) -> bool {
        self.flags & FLAG_AA != 0
    }

    /// opcode（mDNS MUST 0）。
    pub const fn opcode(&self) -> u8 {
        ((self.flags >> 11) & 0x0F) as u8
    }

    /// rcode（mDNS MUST 0）。
    pub const fn rcode(&self) -> u8 {
        (self.flags & 0x0F) as u8
    }

    /// 解析报文（mDNS 约束：opcode/rcode 非 0 视为非法；长度 ≤ 9000）。
    pub fn parse(buffer: &[u8]) -> MdnsResult<Self> {
        if buffer.len() < 12 {
            return Err(MdnsError::Truncated);
        }
        if buffer.len() > MDNS_MAX_MESSAGE_BYTES {
            return Err(MdnsError::TooLarge);
        }
        let id = read_u16(buffer, 0)?;
        let flags = read_u16(buffer, 2)?;
        let qdcount = read_u16(buffer, 4)? as usize;
        let ancount = read_u16(buffer, 6)? as usize;
        let nscount = read_u16(buffer, 8)? as usize;
        let arcount = read_u16(buffer, 10)? as usize;
        let mut offset = 12usize;
        let mut questions = Vec::with_capacity(qdcount.min(64));
        for _ in 0..qdcount {
            let (name, next) = decode_name(buffer, offset)?;
            let qtype = read_u16(buffer, next)?;
            let qclass = read_u16(buffer, next + 2)?;
            offset = next + 4;
            questions.push(DnsQuestion {
                name,
                qtype,
                qclass,
            });
        }
        let mut sections: [Vec<DnsRecord>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (index, count) in [ancount, nscount, arcount].into_iter().enumerate() {
            for _ in 0..count {
                let (name, next) = decode_name(buffer, offset)?;
                let rtype = read_u16(buffer, next)?;
                let rclass = read_u16(buffer, next + 2)?;
                let ttl = read_u32(buffer, next + 4)?;
                let rdlength = read_u16(buffer, next + 8)? as usize;
                let rdata_start = next + 10;
                if rdata_start + rdlength > buffer.len() {
                    return Err(MdnsError::Truncated);
                }
                let rdata = match rtype {
                    TYPE_A => {
                        if rdlength != 4 {
                            return Err(MdnsError::BadRecord);
                        }
                        RData::A(Ipv4Addr::new(
                            buffer[rdata_start],
                            buffer[rdata_start + 1],
                            buffer[rdata_start + 2],
                            buffer[rdata_start + 3],
                        ))
                    }
                    TYPE_PTR => {
                        let (target, _) = decode_name(buffer, rdata_start)?;
                        RData::Ptr(target)
                    }
                    TYPE_TXT => {
                        let mut strings = Vec::new();
                        let mut cursor = rdata_start;
                        while cursor < rdata_start + rdlength {
                            let length = buffer[cursor] as usize;
                            cursor += 1;
                            if cursor + length > rdata_start + rdlength {
                                return Err(MdnsError::BadTxt);
                            }
                            strings.push(buffer[cursor..cursor + length].to_vec());
                            cursor += length;
                        }
                        RData::Txt(strings)
                    }
                    TYPE_SRV => {
                        if rdlength < 7 {
                            return Err(MdnsError::BadSrv);
                        }
                        let priority = read_u16(buffer, rdata_start)?;
                        let weight = read_u16(buffer, rdata_start + 2)?;
                        let port = read_u16(buffer, rdata_start + 4)?;
                        let (target, _) = decode_name(buffer, rdata_start + 6)?;
                        RData::Srv {
                            priority,
                            weight,
                            port,
                            target,
                        }
                    }
                    _ => RData::Unknown(buffer[rdata_start..rdata_start + rdlength].to_vec()),
                };
                offset = rdata_start + rdlength;
                sections[index].push(DnsRecord {
                    name,
                    rtype,
                    rclass,
                    ttl,
                    rdata,
                });
            }
        }
        let [answers, authorities, additionals] = sections;
        let message = Self {
            id,
            flags,
            questions,
            answers,
            authorities,
            additionals,
        };
        if message.opcode() != 0 || message.rcode() != 0 {
            return Err(MdnsError::BadRecord);
        }
        Ok(message)
    }

    /// 编码报文（名字压缩；A/PTR/SRV/TXT rdata）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(512);
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&(self.questions.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.answers.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.authorities.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.additionals.len() as u16).to_be_bytes());
        let mut compression: HashMap<String, u16> = HashMap::new();
        for question in &self.questions {
            encode_name(&mut out, &question.name, &mut compression);
            out.extend_from_slice(&question.qtype.to_be_bytes());
            out.extend_from_slice(&question.qclass.to_be_bytes());
        }
        for record in self
            .answers
            .iter()
            .chain(self.authorities.iter())
            .chain(self.additionals.iter())
        {
            encode_name(&mut out, &record.name, &mut compression);
            out.extend_from_slice(&record.rtype.to_be_bytes());
            out.extend_from_slice(&record.rclass.to_be_bytes());
            out.extend_from_slice(&record.ttl.to_be_bytes());
            let length_at = out.len();
            out.extend_from_slice(&0u16.to_be_bytes());
            let rdata_start = out.len();
            match &record.rdata {
                RData::A(address) => out.extend_from_slice(&address.octets()),
                RData::Ptr(target) => encode_name(&mut out, target, &mut compression),
                RData::Txt(strings) => {
                    for string in strings {
                        let clamped = &string[..string.len().min(255)];
                        out.push(clamped.len() as u8);
                        out.extend_from_slice(clamped);
                    }
                }
                RData::Srv {
                    priority,
                    weight,
                    port,
                    target,
                } => {
                    out.extend_from_slice(&priority.to_be_bytes());
                    out.extend_from_slice(&weight.to_be_bytes());
                    out.extend_from_slice(&port.to_be_bytes());
                    encode_name(&mut out, target, &mut compression);
                }
                RData::Unknown(bytes) => out.extend_from_slice(bytes),
            }
            let rdlength = (out.len() - rdata_start) as u16;
            out[length_at..length_at + 2].copy_from_slice(&rdlength.to_be_bytes());
        }
        out
    }
}

impl Default for DnsMessage {
    fn default() -> Self {
        Self::new()
    }
}

/// 构造 PTR 记录（共享记录，不带缓存冲洗位，RFC 6762 §8.3 L1649-1651）。
pub fn ptr_record(name: &DnsName, target: &DnsName, ttl: u32) -> DnsRecord {
    DnsRecord {
        name: name.clone(),
        rtype: TYPE_PTR,
        rclass: CLASS_IN,
        ttl,
        rdata: RData::Ptr(target.clone()),
    }
}

/// 构造 SRV 记录（唯一记录，带缓存冲洗位）。
pub fn srv_record(
    name: &DnsName,
    priority: u16,
    weight: u16,
    port: u16,
    target: &DnsName,
    ttl: u32,
) -> DnsRecord {
    DnsRecord {
        name: name.clone(),
        rtype: TYPE_SRV,
        rclass: CLASS_IN | CLASS_CACHE_FLUSH,
        ttl,
        rdata: RData::Srv {
            priority,
            weight,
            port,
            target: target.clone(),
        },
    }
}

/// 构造 TXT 记录（唯一记录，带缓存冲洗位）。
pub fn txt_record(name: &DnsName, strings: Vec<Vec<u8>>, ttl: u32) -> DnsRecord {
    DnsRecord {
        name: name.clone(),
        rtype: TYPE_TXT,
        rclass: CLASS_IN | CLASS_CACHE_FLUSH,
        ttl,
        rdata: RData::Txt(strings),
    }
}

/// 构造 A 记录（唯一记录，带缓存冲洗位）。
pub fn a_record(name: &DnsName, address: Ipv4Addr, ttl: u32) -> DnsRecord {
    DnsRecord {
        name: name.clone(),
        rtype: TYPE_A,
        rclass: CLASS_IN | CLASS_CACHE_FLUSH,
        ttl,
        rdata: RData::A(address),
    }
}

/// 一条 DNS-SD 服务（实例 + 服务类型 + 子类型 + SRV/TXT/A 数据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdnsService {
    /// 实例名（用户可见名；可含空格，RFC 6763 §4.1.1 L352-363）。
    pub instance: String,
    /// 基础服务类型（如 _rtsp._tcp.local.）。
    pub service_type: DnsName,
    /// 子类型全名（如 _ravenna_session._sub._rtsp._tcp.local.）。
    pub subtypes: Vec<DnsName>,
    /// 目标主机名（如 khaen-aes67-1.local.）。
    pub host: DnsName,
    /// 端口。
    pub port: u16,
    /// IPv4 地址。
    pub addresses: Vec<Ipv4Addr>,
    /// TXT 字符串（key=value；空 = 单个空串，RFC 6763 §6/§6.1）。
    pub txt: Vec<String>,
    /// 主机类记录 TTL。
    pub ttl_host: u32,
    /// 其它记录 TTL。
    pub ttl_other: u32,
}

impl MdnsService {
    /// 构造（默认 TTL：主机 120 s / 其它 75 min，RFC 6762 §10）。
    pub fn new(
        instance: impl Into<String>,
        service_type: DnsName,
        host: DnsName,
        port: u16,
        address: Ipv4Addr,
    ) -> Self {
        Self {
            instance: instance.into(),
            service_type,
            subtypes: Vec::new(),
            host,
            port,
            addresses: vec![address],
            txt: Vec::new(),
            ttl_host: MDNS_TTL_HOST,
            ttl_other: MDNS_TTL_OTHER,
        }
    }

    /// 追加子类型（标签形式，如 _ravenna 或 _ravenna_session）。
    pub fn with_subtype(mut self, subtype: &str) -> MdnsResult<Self> {
        self.subtypes
            .push(DnsName::subtype(subtype, &self.service_type)?);
        Ok(self)
    }

    /// 追加附加 IPv4 地址。
    pub fn with_address(mut self, address: Ipv4Addr) -> Self {
        if !self.addresses.contains(&address) {
            self.addresses.push(address);
        }
        self
    }

    /// 设置 TXT key=value 列表（RAVENNA 中 TXT 可选；OP §3.5.1.1 L628-632）。
    pub fn with_txt(mut self, entries: Vec<String>) -> Self {
        self.txt = entries;
        self
    }

    /// DNS-SD 实例名：<instance>.<service_type>。
    pub fn instance_name(&self) -> MdnsResult<DnsName> {
        DnsName::with_instance(&self.instance, &self.service_type)
    }

    /// TXT rdata 字符串（空 TXT 仍需一个空串，RFC 6763 §6 L586-592、§6.1 L637-649）。
    fn txt_strings(&self) -> Vec<Vec<u8>> {
        if self.txt.is_empty() {
            vec![Vec::new()]
        } else {
            self.txt
                .iter()
                .map(|entry| entry.as_bytes().to_vec())
                .collect()
        }
    }

    /// 通告应答（answers = PTR 基础/子类型；additionals = SRV/TXT/A）。
    ///
    /// 记录语义按 RFC 6762 §8.3（共享 PTR 无冲洗位、唯一记录带冲洗位）与
    /// RFC 6763 §12.1（PTR 附带 SRV/TXT/A）构造。
    pub fn announcement(&self) -> MdnsResult<(Vec<DnsRecord>, Vec<DnsRecord>)> {
        let instance_name = self.instance_name()?;
        let mut answers = vec![ptr_record(
            &self.service_type,
            &instance_name,
            self.ttl_other,
        )];
        for subtype in &self.subtypes {
            answers.push(ptr_record(subtype, &instance_name, self.ttl_other));
        }
        let additionals = self.resolve_records()?;
        Ok((answers, additionals))
    }

    /// 解析记录组（SRV + TXT + A），即 PTR/SRV 应答的附加记录。
    pub fn resolve_records(&self) -> MdnsResult<Vec<DnsRecord>> {
        let instance_name = self.instance_name()?;
        let mut records = vec![
            srv_record(&instance_name, 0, 0, self.port, &self.host, self.ttl_host),
            txt_record(&instance_name, self.txt_strings(), self.ttl_other),
        ];
        for address in &self.addresses {
            records.push(a_record(&self.host, *address, self.ttl_host));
        }
        Ok(records)
    }

    /// 全部记录（PTR + SRV + TXT + A；用于 goodbye 包）。
    pub fn all_records(&self) -> MdnsResult<Vec<DnsRecord>> {
        let (mut answers, additionals) = self.announcement()?;
        answers.extend(additionals);
        Ok(answers)
    }

    /// 应答一条查询（返回 answers 与 additionals；无匹配返回 None）。
    ///
    /// ANY 查询按 RFC 6762 §6.5 返回所有匹配记录。
    pub fn answer(
        &self,
        question: &DnsQuestion,
    ) -> MdnsResult<Option<(Vec<DnsRecord>, Vec<DnsRecord>)>> {
        if self.instance.trim().is_empty() {
            return Err(MdnsError::Config("empty instance"));
        }
        let instance_name = self.instance_name()?;
        let mut answers: Vec<DnsRecord> = Vec::new();
        let mut additionals: Vec<DnsRecord> = Vec::new();
        let want = |qtype: u16| question.qtype == qtype || question.qtype == TYPE_ANY;
        // 基础服务类型 PTR。
        if question.name.eq_ignore_case(&self.service_type) && want(TYPE_PTR) {
            answers.push(ptr_record(
                &self.service_type,
                &instance_name,
                self.ttl_other,
            ));
            additionals.extend(self.resolve_records()?);
        }
        // 子类型 PTR。
        for subtype in &self.subtypes {
            if question.name.eq_ignore_case(subtype) && want(TYPE_PTR) {
                answers.push(ptr_record(subtype, &instance_name, self.ttl_other));
                additionals.extend(self.resolve_records()?);
            }
        }
        // 实例名 SRV/TXT。
        if question.name.eq_ignore_case(&instance_name) {
            if want(TYPE_SRV) {
                answers.push(srv_record(
                    &instance_name,
                    0,
                    0,
                    self.port,
                    &self.host,
                    self.ttl_host,
                ));
                for address in &self.addresses {
                    additionals.push(a_record(&self.host, *address, self.ttl_host));
                }
            }
            if want(TYPE_TXT) {
                answers.push(txt_record(
                    &instance_name,
                    self.txt_strings(),
                    self.ttl_other,
                ));
            }
        }
        // 主机名 A。
        if question.name.eq_ignore_case(&self.host) && want(TYPE_A) {
            for address in &self.addresses {
                answers.push(a_record(&self.host, *address, self.ttl_host));
            }
        }
        if answers.is_empty() {
            return Ok(None);
        }
        // 附加记录去重（name,rtype）。
        let mut seen: Vec<(String, u16)> = Vec::new();
        additionals.retain(|record| {
            let key = (record.name.to_absolute().to_ascii_lowercase(), record.rtype);
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });
        Ok(Some((answers, additionals)))
    }
}

/// RAVENNA 节点服务种类（OP §3.5.1：源节点 RTSP、sink 节点 HTTP）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RavennaNodeService {
    /// 发送端 RTSP 服务（MUST 发布，OP §3.5 L603-604）。
    Rtsp,
    /// 接收端 HTTP 配置服务（MUST 发布，OP §3.5 L603-604）。
    Http,
}

/// 构造 RAVENNA 节点服务（基础类型 + _ravenna 子类型，OP §3.5.1 L607-623）。
///
/// 实例名 = vendor node id（OP §3.5.1 L610-612，可含空格）；TXT 默认空
/// （RAVENNA 中 TXT 可选且参考实现不发，OP §3.5.1.1 L628-632）。
pub fn ravenna_node_service(
    vendor_node_id: impl Into<String>,
    kind: RavennaNodeService,
    host: DnsName,
    port: u16,
    address: Ipv4Addr,
) -> MdnsResult<MdnsService> {
    let service_type = match kind {
        RavennaNodeService::Rtsp => DnsName::from_str(SERVICE_RTSP_TCP)?,
        RavennaNodeService::Http => DnsName::from_str(SERVICE_HTTP_TCP)?,
    };
    MdnsService::new(vendor_node_id, service_type, host, port, address)
        .with_subtype(RAVENNA_SUBTYPE_NODE)
}

/// 构造 RAVENNA 会话服务（<session name>._rtsp._tcp + _ravenna_session 子类型，OP §3.5.2 L640-649）。
pub fn ravenna_session_service(
    session_name: impl Into<String>,
    host: DnsName,
    port: u16,
    address: Ipv4Addr,
) -> MdnsResult<MdnsService> {
    MdnsService::new(
        session_name,
        DnsName::from_str(SERVICE_RTSP_TCP)?,
        host,
        port,
        address,
    )
    .with_subtype(RAVENNA_SUBTYPE_SESSION)
}

#[derive(Debug)]
struct CacheEntry {
    record: DnsRecord,
    expires_at_ms: u64,
}

/// DNS-SD 浏览缓存（纯逻辑；时间由调用方注入，便于确定性测试）。
#[derive(Debug, Default)]
pub struct MdnsCache {
    entries: HashMap<(String, u16), CacheEntry>,
    applied: u64,
    expired: u64,
}

impl MdnsCache {
    /// 空缓存。
    pub fn new() -> Self {
        Self::default()
    }

    /// 应用一条 mDNS 应答（只收 answers/additionals；TTL 0 按 goodbye 宽限 1 s）。
    pub fn apply_response(&mut self, message: &DnsMessage, now_ms: u64) -> usize {
        let mut applied = 0usize;
        for record in message.answers.iter().chain(message.additionals.iter()) {
            let key = (record.name.to_absolute().to_ascii_lowercase(), record.rtype);
            let lifetime_ms = if record.ttl == 0 {
                MDNS_GOODBYE_GRACE_MS
            } else {
                record.ttl as u64 * 1_000
            };
            self.entries.insert(
                key,
                CacheEntry {
                    record: record.clone(),
                    expires_at_ms: now_ms.saturating_add(lifetime_ms),
                },
            );
            applied += 1;
        }
        self.applied += applied as u64;
        applied
    }

    /// 清理过期记录，返回删除条数。
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.expires_at_ms > now_ms);
        let removed = before - self.entries.len();
        self.expired += removed as u64;
        removed
    }

    /// 当前记录数（未清理过期前）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 已应用记录数。
    pub const fn applied(&self) -> u64 {
        self.applied
    }

    /// 已过期删除记录数。
    pub const fn expired(&self) -> u64 {
        self.expired
    }

    fn get(&self, name: &DnsName, rtype: u16) -> Option<&CacheEntry> {
        let key = (name.to_absolute().to_ascii_lowercase(), rtype);
        self.entries.get(&key)
    }

    /// 枚举某服务类型的实例（PTR → SRV/TXT/A 组合；RFC 6763 §4/§5）。
    pub fn instances(&self, service_type: &DnsName, now_ms: u64) -> Vec<ServiceInstance> {
        let mut out: Vec<ServiceInstance> = Vec::new();
        for entry in self.entries.values() {
            if entry.record.rtype != TYPE_PTR || entry.expires_at_ms <= now_ms {
                continue;
            }
            if !entry.record.name.eq_ignore_case(service_type) {
                continue;
            }
            let RData::Ptr(target) = &entry.record.rdata else {
                continue;
            };
            let Some(srv_entry) = self
                .get(target, TYPE_SRV)
                .filter(|e| e.expires_at_ms > now_ms)
            else {
                continue;
            };
            let RData::Srv {
                priority,
                weight,
                port,
                target: host,
            } = &srv_entry.record.rdata
            else {
                continue;
            };
            let txt = self
                .get(target, TYPE_TXT)
                .filter(|e| e.expires_at_ms > now_ms)
                .and_then(|entry| match &entry.record.rdata {
                    RData::Txt(strings) => Some(
                        strings
                            .iter()
                            .filter(|string| !string.is_empty())
                            .map(|string| String::from_utf8_lossy(string).into_owned())
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            let mut addresses = Vec::new();
            let mut host_name = host.to_absolute();
            for record in self.entries.values() {
                if record.expires_at_ms <= now_ms || record.record.rtype != TYPE_A {
                    continue;
                }
                if record.record.name.eq_ignore_case(host) {
                    if let RData::A(address) = &record.record.rdata {
                        addresses.push(*address);
                        host_name = record.record.name.to_absolute();
                    }
                }
            }
            let ttl_remaining_s = entry.expires_at_ms.saturating_sub(now_ms).div_ceil(1_000) as u32;
            out.push(ServiceInstance {
                instance: target.first_label().unwrap_or_default(),
                service_type: entry.record.name.to_absolute(),
                host: host_name,
                port: *port,
                priority: *priority,
                weight: *weight,
                txt,
                addresses,
                ttl_remaining_s,
            });
        }
        out.sort_by(|left, right| left.instance.cmp(&right.instance));
        out
    }
}

/// 一个已发现的 DNS-SD 服务实例。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceInstance {
    /// 实例名（用户可见部分）。
    pub instance: String,
    /// 服务类型全名（browse 用的基础类型）。
    pub service_type: String,
    /// 目标主机名。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// SRV 优先级（RFC 2782）。
    pub priority: u16,
    /// SRV 权重（RFC 2782）。
    pub weight: u16,
    /// TXT 非空字符串。
    pub txt: Vec<String>,
    /// A 记录地址。
    pub addresses: Vec<Ipv4Addr>,
    /// TTL 剩余秒数。
    pub ttl_remaining_s: u32,
}

/// mDNS 端点（绑定地址、发送目的地址、可选组播加入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MdnsEndpoint {
    /// 本地绑定地址。
    pub bind: SocketAddr,
    /// 发送目的地址（真实部署 = 组播组:5353；测试 = 对端单播地址）。
    pub dest: SocketAddr,
    /// 加入的组播组（None = 不加入，单播模式）。
    pub join_group: Option<Ipv4Addr>,
}

impl MdnsEndpoint {
    /// 真实 mDNS 默认端点（0.0.0.0:5353 ↔ 224.0.0.251:5353，加入组播组）。
    pub fn default_v4() -> Self {
        Self {
            bind: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT)),
            dest: SocketAddr::V4(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT)),
            join_group: Some(MDNS_GROUP),
        }
    }

    /// 单播端点（离线测试/本机联调；不加入组播组）。
    pub fn unicast(bind: SocketAddr, dest: SocketAddr) -> Self {
        Self {
            bind,
            dest,
            join_group: None,
        }
    }
}

fn bind_socket(endpoint: &MdnsEndpoint) -> MdnsResult<UdpSocket> {
    let socket = UdpSocket::bind(endpoint.bind)?;
    socket.set_read_timeout(Some(Duration::from_millis(20)))?;
    if let Some(group) = endpoint.join_group {
        // 加入组播组（多网卡严格出接口未接线，见 README NOT_RUN）。
        socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
        let _ = socket.set_multicast_ttl_v4(255);
        let _ = socket.set_multicast_loop_v4(true);
    }
    Ok(socket)
}

fn send_message(socket: &UdpSocket, endpoint: &MdnsEndpoint, bytes: &[u8]) -> MdnsResult<usize> {
    if bytes.len() > MDNS_MAX_MESSAGE_BYTES {
        return Err(MdnsError::TooLarge);
    }
    socket.send_to(bytes, endpoint.dest).map_err(MdnsError::Io)
}

fn now_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// mDNS 通告器配置。
#[derive(Clone, Debug)]
pub struct MdnsAdvertiserConfig {
    /// 端点。
    pub endpoint: MdnsEndpoint,
    /// 启动通告间隔（RFC 6762 §8.3 L1661-1665：至少两次、间隔 ≥1 s；测试可缩短）。
    pub announce_interval: Duration,
    /// 启动通告次数（≥2；RFC 6762 §8.3）。
    pub announce_count: u8,
}

impl MdnsAdvertiserConfig {
    /// 以端点构造（默认 2 次、间隔 1 s）。
    pub fn new(endpoint: MdnsEndpoint) -> Self {
        Self {
            endpoint,
            announce_interval: Duration::from_secs(1),
            announce_count: 2,
        }
    }

    /// 单播测试快捷构造。
    pub fn unicast(bind: SocketAddr, dest: SocketAddr) -> Self {
        Self::new(MdnsEndpoint::unicast(bind, dest))
    }
}

/// mDNS 通告器（启动通告 + 查询应答；控制面线程）。
#[derive(Debug)]
pub struct MdnsAdvertiser {
    socket: Arc<UdpSocket>,
    endpoint: MdnsEndpoint,
    services: Vec<MdnsService>,
    local_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    announces: Arc<AtomicU64>,
    responses: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl MdnsAdvertiser {
    /// 启动通告器（绑定端点、启动通告线程）。
    pub fn start(config: MdnsAdvertiserConfig, services: Vec<MdnsService>) -> MdnsResult<Self> {
        if services.is_empty() {
            return Err(MdnsError::Config("no services"));
        }
        if config.announce_count == 0 {
            return Err(MdnsError::Config("announce_count must be > 0"));
        }
        let socket = Arc::new(bind_socket(&config.endpoint)?);
        let local_addr = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let announces = Arc::new(AtomicU64::new(0));
        let responses = Arc::new(AtomicU64::new(0));
        let thread_socket = Arc::clone(&socket);
        let thread_endpoint = config.endpoint;
        let thread_services = services.clone();
        let thread_stop = Arc::clone(&stop);
        let thread_announces = Arc::clone(&announces);
        let thread_responses = Arc::clone(&responses);
        let interval = config.announce_interval;
        let count = config.announce_count;
        let thread = thread::Builder::new()
            .name("mdns-adv".to_string())
            .spawn(move || {
                let start = Instant::now();
                // 启动通告（RFC 6762 §8.3）。
                for index in 0..count {
                    let mut answers = Vec::new();
                    let mut additionals = Vec::new();
                    for service in &thread_services {
                        if let Ok((mut service_answers, service_additionals)) =
                            service.announcement()
                        {
                            answers.append(&mut service_answers);
                            additionals.extend(service_additionals);
                        }
                    }
                    let message = DnsMessage::response(answers, additionals);
                    let _ = send_message(&thread_socket, &thread_endpoint, &message.to_bytes());
                    thread_announces.fetch_add(1, Ordering::Relaxed);
                    if index + 1 < count {
                        thread::sleep(interval);
                    }
                }
                // 查询应答循环（已知答案抑制等未实现，见 README NOT_RUN）。
                let mut buffer = [0u8; MDNS_MAX_MESSAGE_BYTES];
                while !thread_stop.load(Ordering::Relaxed) {
                    match thread_socket.recv_from(&mut buffer) {
                        Ok((read, _peer)) => {
                            let Ok(message) = DnsMessage::parse(&buffer[..read]) else {
                                continue;
                            };
                            if message.is_response() {
                                continue;
                            }
                            let mut answers = Vec::new();
                            let mut additionals = Vec::new();
                            for question in &message.questions {
                                for service in &thread_services {
                                    if let Ok(Some((mut service_answers, service_additionals))) =
                                        service.answer(question)
                                    {
                                        answers.append(&mut service_answers);
                                        additionals.extend(service_additionals);
                                    }
                                }
                            }
                            if answers.is_empty() {
                                continue;
                            }
                            let mut seen: Vec<(String, u16)> = Vec::new();
                            additionals.retain(|record| {
                                let key =
                                    (record.name.to_absolute().to_ascii_lowercase(), record.rtype);
                                if seen.contains(&key) {
                                    false
                                } else {
                                    seen.push(key);
                                    true
                                }
                            });
                            let response = DnsMessage::response(answers, additionals);
                            let _ = send_message(
                                &thread_socket,
                                &thread_endpoint,
                                &response.to_bytes(),
                            );
                            thread_responses.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                || error.kind() == io::ErrorKind::TimedOut => {}
                        Err(_) => break,
                    }
                    let _ = now_ms(start);
                }
            })
            .map_err(MdnsError::Io)?;
        Ok(Self {
            socket,
            endpoint: config.endpoint,
            services,
            local_addr,
            stop,
            announces,
            responses,
            thread: Some(thread),
        })
    }

    /// 本地绑定地址。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 已发送启动通告数。
    pub fn announces_sent(&self) -> u64 {
        self.announces.load(Ordering::Relaxed)
    }

    /// 已应答查询数。
    pub fn responses_sent(&self) -> u64 {
        self.responses.load(Ordering::Relaxed)
    }

    /// 当前服务列表。
    pub fn services(&self) -> &[MdnsService] {
        &self.services
    }

    /// 停止：先发 goodbye（TTL 0，RFC 6762 §10.1 L1842-1867），再停线程。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // 先停线程（避免与新 send 竞争），再发 goodbye。
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        let mut goodbye_records = Vec::new();
        for service in &self.services {
            if let Ok(records) = service.all_records() {
                for record in records {
                    let mut record = record;
                    record.ttl = 0;
                    goodbye_records.push(record);
                }
            }
        }
        if !goodbye_records.is_empty() {
            let message = DnsMessage::response(goodbye_records, Vec::new());
            let _ = send_message(&self.socket, &self.endpoint, &message.to_bytes());
        }
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// mDNS 浏览器配置。
#[derive(Clone, Debug)]
pub struct MdnsBrowserConfig {
    /// 端点。
    pub endpoint: MdnsEndpoint,
    /// 要浏览的服务类型（基础类型，如 _rtsp._tcp.local. 或 _ravenna_session._sub._rtsp._tcp.local.）。
    pub service_type: DnsName,
}

impl MdnsBrowserConfig {
    /// 以端点 + 服务类型构造。
    pub fn new(endpoint: MdnsEndpoint, service_type: DnsName) -> Self {
        Self {
            endpoint,
            service_type,
        }
    }

    /// 单播测试快捷构造。
    pub fn unicast(bind: SocketAddr, dest: SocketAddr, service_type: DnsName) -> Self {
        Self::new(MdnsEndpoint::unicast(bind, dest), service_type)
    }
}

/// mDNS 浏览器（发送 PTR 查询 + 接收缓存；控制面线程）。
#[derive(Debug)]
pub struct MdnsBrowser {
    socket: Arc<UdpSocket>,
    endpoint: MdnsEndpoint,
    service_type: DnsName,
    local_addr: SocketAddr,
    cache: Arc<Mutex<MdnsCache>>,
    stop: Arc<AtomicBool>,
    start: Instant,
    thread: Option<JoinHandle<()>>,
}

impl MdnsBrowser {
    /// 启动浏览器（绑定端点、发送初始 PTR 查询、启动接收线程）。
    pub fn start(config: MdnsBrowserConfig) -> MdnsResult<Self> {
        let socket = Arc::new(bind_socket(&config.endpoint)?);
        let local_addr = socket.local_addr()?;
        let cache = Arc::new(Mutex::new(MdnsCache::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        let thread_socket = Arc::clone(&socket);
        let thread_cache = Arc::clone(&cache);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("mdns-browse".to_string())
            .spawn(move || {
                let mut buffer = [0u8; MDNS_MAX_MESSAGE_BYTES];
                while !thread_stop.load(Ordering::Relaxed) {
                    match thread_socket.recv_from(&mut buffer) {
                        Ok((read, _peer)) => {
                            let Ok(message) = DnsMessage::parse(&buffer[..read]) else {
                                continue;
                            };
                            if !message.is_response() {
                                continue;
                            }
                            let now = now_ms(start);
                            if let Ok(mut cache) = thread_cache.lock() {
                                cache.apply_response(&message, now);
                                cache.expire(now);
                            }
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                || error.kind() == io::ErrorKind::TimedOut => {}
                        Err(_) => break,
                    }
                }
            })
            .map_err(MdnsError::Io)?;
        let browser = Self {
            socket,
            endpoint: config.endpoint,
            service_type: config.service_type,
            local_addr,
            cache,
            stop,
            start,
            thread: Some(thread),
        };
        browser.query()?;
        Ok(browser)
    }

    /// 发送 PTR 浏览查询（RFC 6763 §4：PTR 查询服务类型）。
    pub fn query(&self) -> MdnsResult<()> {
        let question = DnsQuestion::in_class(self.service_type.clone(), TYPE_PTR);
        let message = DnsMessage::query(vec![question]);
        send_message(&self.socket, &self.endpoint, &message.to_bytes())?;
        Ok(())
    }

    /// 一组 PTR 查询（含基础类型与子类型一次发出；可选）。
    pub fn query_types(&self, types: &[DnsName]) -> MdnsResult<()> {
        let questions = types
            .iter()
            .map(|name| DnsQuestion::in_class(name.clone(), TYPE_PTR))
            .collect();
        let message = DnsMessage::query(questions);
        send_message(&self.socket, &self.endpoint, &message.to_bytes())?;
        Ok(())
    }

    /// 当前已发现实例（自动清理过期记录）。
    pub fn instances(&self) -> Vec<ServiceInstance> {
        let now = now_ms(self.start);
        match self.cache.lock() {
            Ok(mut cache) => {
                cache.expire(now);
                cache.instances(&self.service_type, now)
            }
            Err(_) => Vec::new(),
        }
    }

    /// 本地绑定地址。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 停止浏览器。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MdnsBrowser {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddrV4;

    fn name(text: &str) -> DnsName {
        DnsName::from_str(text).expect("name")
    }

    fn local() -> MdnsEndpoint {
        MdnsEndpoint::unicast(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5353)),
        )
    }

    fn free_udp_addr() -> SocketAddr {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = socket.local_addr().expect("addr");
        drop(socket);
        addr
    }

    fn wait_until(deadline: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    fn test_service() -> MdnsService {
        ravenna_session_service(
            "Line 1",
            name("khaen-aes67-1.local."),
            8854,
            Ipv4Addr::new(192, 0, 2, 94),
        )
        .expect("service")
    }

    #[test]
    fn dns_name_encoding_compression_and_pointer_guard() {
        let service = test_service();
        let instance = service.instance_name().expect("instance");
        let message = DnsMessage::response(
            vec![ptr_record(&service.service_type, &instance, MDNS_TTL_OTHER)],
            service.resolve_records().expect("records"),
        );
        let bytes = message.to_bytes();
        // 有压缩指针（0xC0 前缀出现在 rdata 名字处）。
        assert!(bytes.iter().any(|byte| byte & 0xC0 == 0xC0));
        let parsed = DnsMessage::parse(&bytes).expect("parse");
        assert_eq!(parsed, message);
        // 不同大小写视为同名（RFC 6762 §16 L2532-2536）。
        assert!(name("LINE 1._RTSP._TCP.LOCAL.").eq_ignore_case(&instance));
        // 非法名字。
        assert!(DnsName::from_str("a..b").is_err());
        assert!(DnsName::from_str(&"x".repeat(64)).is_err());
        // 指针环必须被拒绝（防无限循环）：构造自指指针。
        let mut looped = vec![0u8; 12];
        looped[4] = 0;
        looped[5] = 1; // QDCOUNT = 1
        looped.extend_from_slice(&[0xC0, 0x0C]); // 指向自身
        looped.extend_from_slice(&[0x00, 0x0C, 0x00, 0x01]);
        assert!(matches!(
            DnsMessage::parse(&looped),
            Err(MdnsError::BadPointer)
        ));
    }

    #[test]
    fn dns_message_known_vector_query_fields() {
        // 手工构造 mDNS PTR 查询 _rtsp._tcp.local（DNS 线上格式，RFC 1035 §4.1）。
        let raw: Vec<u8> = vec![
            0x00, 0x00, // ID = 0（RFC 6762 §18.1）
            0x00, 0x00, // flags: QR=0
            0x00, 0x01, // QDCOUNT = 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // AN/NS/AR = 0
            0x05, b'_', b'r', b't', b's', b'p', // _rtsp
            0x04, b'_', b't', b'c', b'p', // _tcp
            0x05, b'l', b'o', b'c', b'a', b'l', // local
            0x00, // 名字结束
            0x00, 0x0C, // QTYPE = PTR
            0x00, 0x01, // QCLASS = IN
        ];
        let message = DnsMessage::parse(&raw).expect("parse");
        assert!(!message.is_response());
        assert_eq!(message.questions.len(), 1);
        assert_eq!(message.questions[0].name.to_absolute(), "_rtsp._tcp.local.");
        assert_eq!(message.questions[0].qtype, TYPE_PTR);
        assert_eq!(message.questions[0].qclass, CLASS_IN);
        // 重新编码：除压缩表示差异外解析结果一致。
        let reencoded = message.to_bytes();
        let reparsed = DnsMessage::parse(&reencoded).expect("reparse");
        assert_eq!(reparsed.questions, message.questions);
        // 应答标志位。
        let response = DnsMessage::response(Vec::new(), Vec::new());
        let parsed = DnsMessage::parse(&response.to_bytes()).expect("response");
        assert!(parsed.is_response());
        assert!(parsed.is_authoritative());
        assert_eq!(parsed.id, 0);
        assert_eq!(parsed.opcode(), 0);
        assert_eq!(parsed.rcode(), 0);
        // opcode 非 0 → 拒绝。
        let mut bad = raw.clone();
        bad[2] = 0x08; // opcode = 1
        assert!(DnsMessage::parse(&bad).is_err());
    }

    #[test]
    fn mdns_announcement_records_follow_ravenna_and_dns_sd() {
        let service = test_service();
        let (answers, additionals) = service.announcement().expect("announce");
        // 基础类型 PTR + _ravenna_session 子类型 PTR（OP §3.5.2.1）。
        assert_eq!(answers.len(), 2);
        assert!(answers[0].name.eq_ignore_case(&name(SERVICE_RTSP_TCP)));
        assert_eq!(answers[0].rtype, TYPE_PTR);
        assert_eq!(answers[0].class(), CLASS_IN);
        assert!(
            !answers[0].cache_flush(),
            "shared PTR must not set cache-flush"
        );
        let subtype = name("_ravenna_session._sub._rtsp._tcp.local.");
        assert!(answers[1].name.eq_ignore_case(&subtype));
        // SRV/TXT/A 作为附加记录（RFC 6763 §12.1），且唯一记录带冲洗位。
        let srv = additionals
            .iter()
            .find(|r| r.rtype == TYPE_SRV)
            .expect("srv");
        assert!(srv.cache_flush());
        let RData::Srv { port, target, .. } = &srv.rdata else {
            panic!("srv rdata");
        };
        assert_eq!(*port, 8854);
        assert_eq!(target.to_absolute(), "khaen-aes67-1.local.");
        let txt = additionals
            .iter()
            .find(|r| r.rtype == TYPE_TXT)
            .expect("txt");
        assert!(txt.cache_flush());
        let RData::Txt(strings) = &txt.rdata else {
            panic!("txt rdata");
        };
        // 空 TXT = 单个空串（RFC 6763 §6.1 L637-649）。
        assert_eq!(strings, &vec![Vec::<u8>::new()]);
        let a = additionals.iter().find(|r| r.rtype == TYPE_A).expect("a");
        assert_eq!(a.rdata, RData::A(Ipv4Addr::new(192, 0, 2, 94)));
        // 名字与实例名（OP §3.5.2.1：<session name>._rtsp._tcp）。
        let instance = service.instance_name().expect("instance");
        assert_eq!(instance.to_absolute(), "Line 1._rtsp._tcp.local.");
        // TXT 可选字段可用（EXTENSION：RAVENNA 未规定 key，仅 MAY 放节点信息）。
        let with_txt = service.clone().with_txt(vec!["node=source".to_string()]);
        let records = with_txt.resolve_records().expect("records");
        let RData::Txt(strings) = &records[1].rdata else {
            panic!("txt");
        };
        assert_eq!(strings[0], b"node=source");
        // 节点服务：RTSP/HTTP 各自带 _ravenna 子类型（OP §3.5.1 L618-621）。
        let node = ravenna_node_service(
            "My-Corp devicex 1234-5678",
            RavennaNodeService::Rtsp,
            name("khaen-aes67-1.local."),
            8854,
            Ipv4Addr::new(192, 0, 2, 94),
        )
        .expect("node");
        assert!(node
            .subtypes
            .iter()
            .any(|subtype| subtype.to_absolute() == "_ravenna._sub._rtsp._tcp.local."));
        let http = ravenna_node_service(
            "My-Corp devicex 1234-5678",
            RavennaNodeService::Http,
            name("khaen-aes67-1.local."),
            8080,
            Ipv4Addr::new(192, 0, 2, 94),
        )
        .expect("http node");
        assert_eq!(http.service_type.to_absolute(), SERVICE_HTTP_TCP);
        assert!(http
            .subtypes
            .iter()
            .any(|subtype| subtype.to_absolute() == "_ravenna._sub._http._tcp.local."));
    }

    #[test]
    fn mdns_answers_questions_and_builds_browse_response() {
        let service = test_service();
        // PTR 查询基础类型 → PTR + 附加 SRV/TXT/A。
        let question = DnsQuestion::in_class(name(SERVICE_RTSP_TCP), TYPE_PTR);
        let (answers, additionals) = service.answer(&question).expect("answer").expect("some");
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].rtype, TYPE_PTR);
        assert!(additionals.iter().any(|r| r.rtype == TYPE_SRV));
        assert!(additionals.iter().any(|r| r.rtype == TYPE_TXT));
        assert!(additionals.iter().any(|r| r.rtype == TYPE_A));
        // 子类型查询。
        let question =
            DnsQuestion::in_class(name("_ravenna_session._sub._rtsp._tcp.local."), TYPE_PTR);
        assert!(service.answer(&question).expect("answer").is_some());
        // SRV 查询实例名。
        let instance = service.instance_name().expect("instance");
        let question = DnsQuestion::in_class(instance.clone(), TYPE_SRV);
        let (answers, additionals) = service.answer(&question).expect("answer").expect("some");
        assert_eq!(answers[0].rtype, TYPE_SRV);
        assert!(additionals.iter().all(|r| r.rtype == TYPE_A));
        // ANY 查询。
        let question = DnsQuestion::in_class(instance, TYPE_ANY);
        let (answers, _) = service.answer(&question).expect("answer").expect("some");
        assert!(answers.iter().any(|r| r.rtype == TYPE_SRV));
        assert!(answers.iter().any(|r| r.rtype == TYPE_TXT));
        // 不匹配 → None。
        let question = DnsQuestion::in_class(name("other._rtsp._tcp.local."), TYPE_PTR);
        assert!(service.answer(&question).expect("answer").is_none());
    }

    #[test]
    fn mdns_cache_builds_instances_and_expires_by_ttl() {
        let service = test_service();
        let (answers, additionals) = service.announcement().expect("announce");
        let response = DnsMessage::response(answers, additionals);
        let mut cache = MdnsCache::new();
        let applied = cache.apply_response(&response, 0);
        assert!(applied >= 5);
        let instances = cache.instances(&name(SERVICE_RTSP_TCP), 0);
        assert_eq!(instances.len(), 1);
        let instance = &instances[0];
        assert_eq!(instance.instance, "Line 1");
        assert_eq!(instance.service_type, "_rtsp._tcp.local.");
        assert_eq!(instance.host, "khaen-aes67-1.local.");
        assert_eq!(instance.port, 8854);
        assert_eq!(instance.addresses, vec![Ipv4Addr::new(192, 0, 2, 94)]);
        assert!(instance.ttl_remaining_s > 0);
        // 过期：PTR TTL = 4500 s → 4500_000 ms 后消失；主机记录 120 s 先过期不影响 PTR 枚举。
        assert!(cache
            .instances(&name(SERVICE_RTSP_TCP), 4_500_001)
            .is_empty());
        cache.expire(4_500_001);
        assert!(cache.is_empty());
    }

    #[test]
    fn mdns_cache_goodbye_uses_one_second_grace() {
        let service = test_service();
        let (answers, additionals) = service.announcement().expect("announce");
        let response = DnsMessage::response(answers, additionals);
        let mut cache = MdnsCache::new();
        cache.apply_response(&response, 0);
        assert_eq!(cache.instances(&name(SERVICE_RTSP_TCP), 0).len(), 1);
        // goodbye：所有记录 TTL 0（RFC 6762 §10.1）。
        let mut goodbye = Vec::new();
        for record in service.all_records().expect("records") {
            let mut record = record;
            record.ttl = 0;
            goodbye.push(record);
        }
        cache.apply_response(&DnsMessage::response(goodbye, Vec::new()), 1_000);
        // 1 s 宽限内仍可见（RFC 6762 §10.1 L1860-1867）。
        assert_eq!(cache.instances(&name(SERVICE_RTSP_TCP), 1_500).len(), 1);
        // 宽限后消失。
        assert!(cache.instances(&name(SERVICE_RTSP_TCP), 2_100).is_empty());
    }

    #[test]
    fn mdns_parse_rejects_malformed_messages() {
        assert!(matches!(DnsMessage::parse(&[]), Err(MdnsError::Truncated)));
        assert!(matches!(
            DnsMessage::parse(&[0u8; 11]),
            Err(MdnsError::Truncated)
        ));
        // ANCOUNT 大于实际记录 → 截断。
        let mut raw = vec![0u8; 12];
        raw[6] = 0x00;
        raw[7] = 0x01;
        assert!(DnsMessage::parse(&raw).is_err());
        // TXT 长度越界。
        let mut message = DnsMessage::new();
        message.answers.push(DnsRecord {
            name: name("x.local."),
            rtype: TYPE_TXT,
            rclass: CLASS_IN,
            ttl: 120,
            rdata: RData::Txt(vec![b"ok".to_vec()]),
        });
        let mut bytes = message.to_bytes();
        // 找到 TXT 的 [0x02, b'o', b'k']，把长度字节改成 255（声明 255 字节但只给 2 字节）。
        let position = bytes
            .windows(3)
            .position(|window| window == [0x02, b'o', b'k'])
            .expect("txt bytes");
        bytes[position] = 0xFF;
        assert!(matches!(DnsMessage::parse(&bytes), Err(MdnsError::BadTxt)));
        // A 记录 rdata 长度非法。
        let mut message = DnsMessage::new();
        message.answers.push(DnsRecord {
            name: name("x.local."),
            rtype: TYPE_A,
            rclass: CLASS_IN,
            ttl: 120,
            rdata: RData::Unknown(vec![1, 2]),
        });
        let bytes = message.to_bytes();
        assert!(matches!(
            DnsMessage::parse(&bytes),
            Err(MdnsError::BadRecord)
        ));
    }

    #[test]
    fn mdns_advertiser_and_browser_unicast_roundtrip() {
        // 真实网络测试：无网/受限环境显式跳过（见 crate::testnet）。
        if crate::testnet::skip_if_unavailable("mdns_advertiser_and_browser_unicast_roundtrip") {
            return;
        }
        let browser_addr = free_udp_addr();
        let advertiser = MdnsAdvertiser::start(
            MdnsAdvertiserConfig {
                endpoint: MdnsEndpoint::unicast(
                    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
                    browser_addr,
                ),
                announce_interval: Duration::from_millis(20),
                announce_count: 2,
            },
            vec![test_service()],
        )
        .expect("advertiser");
        let browser = MdnsBrowser::start(MdnsBrowserConfig::unicast(
            browser_addr,
            advertiser.local_addr(),
            name(SERVICE_RTSP_TCP),
        ))
        .expect("browser");
        // 初始通告可能早于浏览器启动；浏览器启动时发送 PTR 查询，通告器应答。
        assert!(wait_until(Duration::from_secs(2), || !browser
            .instances()
            .is_empty()));
        // 再查一次，确保查询/应答路径也走通。
        browser.query().expect("query");
        assert!(wait_until(Duration::from_secs(2), || {
            advertiser.responses_sent() > 0 && !browser.instances().is_empty()
        }));
        let instances = browser.instances();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance, "Line 1");
        assert_eq!(instances[0].port, 8854);
        assert_eq!(instances[0].addresses, vec![Ipv4Addr::new(192, 0, 2, 94)]);
        assert!(advertiser.announces_sent() >= 2);
        browser.stop();
        advertiser.stop();
    }

    #[test]
    fn mdns_goodbye_removes_discovered_instance() {
        // 真实网络测试：无网/受限环境显式跳过（见 crate::testnet）。
        if crate::testnet::skip_if_unavailable("mdns_goodbye_removes_discovered_instance") {
            return;
        }
        let browser_addr = free_udp_addr();
        let advertiser = MdnsAdvertiser::start(
            MdnsAdvertiserConfig::unicast(
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
                browser_addr,
            ),
            vec![test_service()],
        )
        .expect("advertiser");
        let browser = MdnsBrowser::start(MdnsBrowserConfig::unicast(
            browser_addr,
            advertiser.local_addr(),
            name(SERVICE_RTSP_TCP),
        ))
        .expect("browser");
        browser.query().expect("query");
        assert!(wait_until(Duration::from_secs(2), || !browser
            .instances()
            .is_empty()));
        // 退出注销：stop 发 TTL=0 goodbye，浏览器 1 s 宽限后移除。
        advertiser.stop();
        assert!(wait_until(Duration::from_secs(4), || browser
            .instances()
            .is_empty()));
        browser.stop();
    }

    #[test]
    fn mdns_endpoint_defaults_and_service_helpers() {
        let endpoint = MdnsEndpoint::default_v4();
        assert_eq!(endpoint.bind.port(), MDNS_PORT);
        assert_eq!(
            endpoint.dest,
            SocketAddr::V4(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT))
        );
        assert_eq!(endpoint.join_group, Some(MDNS_GROUP));
        assert_eq!(MDNS_GROUP, Ipv4Addr::new(224, 0, 0, 251));
        assert_eq!(MDNS_TTL_HOST, 120);
        assert_eq!(MDNS_TTL_OTHER, 4500);
        let subtype =
            DnsName::subtype(RAVENNA_SUBTYPE_NODE, &name(SERVICE_RTSP_TCP)).expect("subtype");
        assert_eq!(subtype.to_absolute(), "_ravenna._sub._rtsp._tcp.local.");
        let session =
            DnsName::subtype(RAVENNA_SUBTYPE_SESSION, &name(SERVICE_RTSP_TCP)).expect("subtype");
        assert_eq!(
            session.to_absolute(),
            "_ravenna_session._sub._rtsp._tcp.local."
        );
        // 空实例订阅配置错误。
        let empty = MdnsService::new(
            "",
            name(SERVICE_RTSP_TCP),
            name("h.local."),
            1,
            Ipv4Addr::LOCALHOST,
        );
        assert!(empty
            .answer(&DnsQuestion::in_class(name(SERVICE_RTSP_TCP), TYPE_PTR))
            .is_err());
        // 非法配置。
        assert!(MdnsAdvertiser::start(
            MdnsAdvertiserConfig::unicast(local().bind, local().dest),
            Vec::new()
        )
        .is_err());
        // DnsName 文本格式。
        assert_eq!(name("_rtsp._tcp.local.").to_absolute(), "_rtsp._tcp.local.");
        assert_eq!(name("_rtsp._tcp.local").to_absolute(), "_rtsp._tcp.local.");
        assert_eq!(name(".").to_absolute(), ".");
    }
}
