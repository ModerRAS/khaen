//! 组播监听面配置类型（task-320）：接口列表、组白名单、RTP 来源白名单（IPv4 CIDR）。
//!
//! 全部为 **Copy 定长小容器**（`ReceiverConfig` 保持 `Copy`；启动期构造一次，收包热路径只读）：
//! - `ReceiverInterfaces`：0..=8 个 IPv4（空 = 旧行为 0.0.0.0）；
//! - `GroupAllow`：0..=16 个 IPv4 组播组（空 = 旧行为：接受 SDP 声明的组）；
//! - `SourceAllow`：0..=16 个 IPv4/CIDR（空 = 旧行为：接受任意来源）。
//!
//! 语义纪律（fail closed）：
//! - 组白名单非空时，SDP 声明的组必须命中，否则**拒绝启动**（不 join、不收包）；
//! - 来源白名单非空时，收包线程在 RTP 解析/Jitter 之前丢弃非白名单来源并计数
//!   （不污染 jitter/underrun；旧行为 = 空列表）。

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

/// 最多可配置的接收接口数（多网卡部署：每个接口一条 IP_ADD_MEMBERSHIP）。
pub const MAX_RECEIVER_INTERFACES: usize = 8;
/// 最多可配置的组白名单条目数。
pub const MAX_ALLOWED_GROUPS: usize = 16;
/// 最多可配置的来源白名单 CIDR 数。
pub const MAX_SOURCE_CIDRS: usize = 16;

/// 监听面配置错误（启动期；文案可直接进日志/错误 JSON）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenConfigError {
    /// 接口数超过上限。
    TooManyInterfaces {
        /// 上限。
        max: usize,
    },
    /// 组白名单条目超过上限。
    TooManyGroups {
        /// 上限。
        max: usize,
    },
    /// 来源白名单条目超过上限。
    TooManySources {
        /// 上限。
        max: usize,
    },
    /// 接口地址重复。
    DuplicateInterface {
        /// 重复地址。
        address: Ipv4Addr,
    },
    /// 组播组重复。
    DuplicateGroup {
        /// 重复组。
        group: Ipv4Addr,
    },
    /// 来源 CIDR 重复。
    DuplicateSource {
        /// 重复 CIDR（规范化后文本）。
        cidr: String,
    },
    /// 指定为接口的地址不是单播主机地址（组播/广播/0.0.0.0 等）。
    NotInterfaceAddress {
        /// 非法地址。
        address: Ipv4Addr,
    },
    /// 指定为组播组的地址不是 224.0.0.0/4。
    NotMulticastGroup {
        /// 非法地址。
        address: Ipv4Addr,
    },
    /// 非法 IPv4/CIDR 文本。
    BadCidr {
        /// 原始文本。
        text: String,
    },
    /// 前缀长度越界。
    BadPrefix {
        /// 原始前缀。
        prefix: u32,
    },
}

impl fmt::Display for ListenConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListenConfigError::TooManyInterfaces { max } => {
                write!(f, "网卡接口数超过上限 {max}")
            }
            ListenConfigError::TooManyGroups { max } => {
                write!(f, "组白名单条目超过上限 {max}")
            }
            ListenConfigError::TooManySources { max } => {
                write!(f, "来源白名单条目超过上限 {max}")
            }
            ListenConfigError::DuplicateInterface { address } => {
                write!(f, "网卡接口 {address} 重复")
            }
            ListenConfigError::DuplicateGroup { group } => {
                write!(f, "组播组 {group} 重复")
            }
            ListenConfigError::DuplicateSource { cidr } => {
                write!(f, "来源白名单 {cidr} 重复")
            }
            ListenConfigError::NotInterfaceAddress { address } => write!(
                f,
                "{address} 不是可用的单播接口地址（不接受 0.0.0.0/组播/广播）"
            ),
            ListenConfigError::NotMulticastGroup { address } => {
                write!(f, "{address} 不是 IPv4 组播组地址（224.0.0.0/4）")
            }
            ListenConfigError::BadCidr { text } => write!(
                f,
                "非法 IPv4/CIDR \"{text}\"（期望 a.b.c.d 或 a.b.c.d/0..32）"
            ),
            ListenConfigError::BadPrefix { prefix } => {
                write!(f, "CIDR 前缀 /{prefix} 越界（0..=32）")
            }
        }
    }
}

impl std::error::Error for ListenConfigError {}

/// 接收接口集合（组播 = 每个接口一条 join；单播 = bind 主接口地址）。
///
/// 空 = 旧行为 0.0.0.0；构造后不可变（启动期一次）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverInterfaces {
    addresses: [Ipv4Addr; MAX_RECEIVER_INTERFACES],
    len: u8,
}

impl Default for ReceiverInterfaces {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl ReceiverInterfaces {
    /// 空集合（旧行为 0.0.0.0）。
    pub const EMPTY: Self = Self {
        addresses: [Ipv4Addr::UNSPECIFIED; MAX_RECEIVER_INTERFACES],
        len: 0,
    };

    /// 单接口集合。
    pub fn from_addr(address: Ipv4Addr) -> Self {
        let mut addresses = [Ipv4Addr::UNSPECIFIED; MAX_RECEIVER_INTERFACES];
        addresses[0] = address;
        Self { addresses, len: 1 }
    }

    /// 由地址列表构造（校验上限/重复/单播性；空列表 = 旧行为）。
    pub fn try_from_addresses(addresses: &[Ipv4Addr]) -> Result<Self, ListenConfigError> {
        if addresses.len() > MAX_RECEIVER_INTERFACES {
            return Err(ListenConfigError::TooManyInterfaces {
                max: MAX_RECEIVER_INTERFACES,
            });
        }
        let mut set = Self::EMPTY;
        for address in addresses {
            if *address == Ipv4Addr::UNSPECIFIED
                || address.is_multicast()
                || address.is_broadcast()
            {
                return Err(ListenConfigError::NotInterfaceAddress { address: *address });
            }
            if set.contains(*address) {
                return Err(ListenConfigError::DuplicateInterface { address: *address });
            }
            set.addresses[set.len as usize] = *address;
            set.len += 1;
        }
        Ok(set)
    }

    /// 接口数（0 = 旧行为 0.0.0.0）。
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// 是否为空（0.0.0.0 旧行为）。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 按序取第 index 个接口。
    pub fn get(&self, index: usize) -> Option<Ipv4Addr> {
        if index < self.len() {
            Some(self.addresses[index])
        } else {
            None
        }
    }

    /// 第一个接口（单播 bind 地址；空 = None）。
    pub fn primary(&self) -> Option<Ipv4Addr> {
        self.get(0)
    }

    /// 全部接口地址（按配置顺序）。
    pub fn addresses(&self) -> &[Ipv4Addr] {
        &self.addresses[..self.len()]
    }

    /// 是否包含该地址。
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        self.addresses().contains(&address)
    }

    /// 描述文本（报告/警告/错误用；空 = "0.0.0.0（所有接口）"）。
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "0.0.0.0（所有接口）".to_string();
        }
        self.addresses()
            .iter()
            .map(|address| address.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// IPv4 组播组白名单（只 join/接受这些组；空 = 旧行为）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupAllow {
    groups: [Ipv4Addr; MAX_ALLOWED_GROUPS],
    len: u8,
}

impl Default for GroupAllow {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl GroupAllow {
    /// 空白名单（旧行为：接受 SDP 声明的组）。
    pub const EMPTY: Self = Self {
        groups: [Ipv4Addr::UNSPECIFIED; MAX_ALLOWED_GROUPS],
        len: 0,
    };

    /// 由组列表构造（校验上限/重复/组播地址）。
    pub fn try_from_groups(groups: &[Ipv4Addr]) -> Result<Self, ListenConfigError> {
        if groups.len() > MAX_ALLOWED_GROUPS {
            return Err(ListenConfigError::TooManyGroups {
                max: MAX_ALLOWED_GROUPS,
            });
        }
        let mut set = Self::EMPTY;
        for group in groups {
            if !group.is_multicast() || group == &Ipv4Addr::BROADCAST {
                return Err(ListenConfigError::NotMulticastGroup { address: *group });
            }
            if set.contains(*group) {
                return Err(ListenConfigError::DuplicateGroup { group: *group });
            }
            set.groups[set.len as usize] = *group;
            set.len += 1;
        }
        Ok(set)
    }

    /// 白名单条目数（0 = 旧行为）。
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// 是否为空（旧行为）。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 全部组（按配置顺序）。
    pub fn groups(&self) -> &[Ipv4Addr] {
        &self.groups[..self.len()]
    }

    /// 组是否在允许列表内。
    pub fn contains(&self, group: Ipv4Addr) -> bool {
        self.groups().contains(&group)
    }

    /// 描述文本（错误/报告用）。
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "any".to_string();
        }
        self.groups()
            .iter()
            .map(|group| group.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// 一个 IPv4 CIDR（网络地址已按前缀规范化；主机位清零）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Cidr {
    network: Ipv4Addr,
    prefix_len: u8,
}

impl Default for Ipv4Cidr {
    fn default() -> Self {
        // 默认值仅用于定长数组占位（0.0.0.0/0）；有效条目由 try_from_cidrs 校验后写入。
        Self {
            network: Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
        }
    }
}

impl Ipv4Cidr {
    /// 构造（主机位自动清零；前缀 >32 → 错误）。
    pub fn new(address: Ipv4Addr, prefix_len: u8) -> Result<Self, ListenConfigError> {
        if prefix_len > 32 {
            return Err(ListenConfigError::BadPrefix {
                prefix: u32::from(prefix_len),
            });
        }
        let mask = prefix_mask(prefix_len);
        Ok(Self {
            network: Ipv4Addr::from(u32::from(address) & mask),
            prefix_len,
        })
    }

    /// 解析 "a.b.c.d"（隐含 /32）或 "a.b.c.d/N"。
    pub fn parse(text: &str) -> Result<Self, ListenConfigError> {
        let text = text.trim();
        let (address_text, prefix_text) = match text.split_once('/') {
            Some((address, prefix)) => (address.trim(), Some(prefix.trim())),
            None => (text, None),
        };
        let address = address_text.parse::<Ipv4Addr>().map_err(|_| {
            ListenConfigError::BadCidr {
                text: text.to_string(),
            }
        })?;
        let prefix_len = match prefix_text {
            None => 32u8,
            Some(prefix) => {
                let prefix: u32 = prefix.parse().map_err(|_| ListenConfigError::BadCidr {
                    text: text.to_string(),
                })?;
                if prefix > 32 {
                    return Err(ListenConfigError::BadPrefix { prefix });
                }
                prefix as u8
            }
        };
        Self::new(address, prefix_len)
    }

    /// 规范化网络地址。
    pub fn network(&self) -> Ipv4Addr {
        self.network
    }

    /// 前缀长度。
    pub fn prefix_len(&self) -> u8 {
        self.prefix_len
    }

    /// 地址是否命中本 CIDR。
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        u32::from(address) & prefix_mask(self.prefix_len) == u32::from(self.network)
    }
}

impl fmt::Display for Ipv4Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix_len)
    }
}

/// 前缀长度 → 主机字节序无关的 u32 掩码（由 32 位无符号计算）。
fn prefix_mask(prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else if prefix_len >= 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - u32::from(prefix_len))
    }
}

/// RTP 来源白名单（IPv4/CIDR；空 = 旧行为：接受任意来源）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceAllow {
    cidrs: [Ipv4Cidr; MAX_SOURCE_CIDRS],
    len: u8,
}

impl Default for SourceAllow {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl SourceAllow {
    /// 空白名单（旧行为）。
    pub const EMPTY: Self = Self {
        cidrs: [Ipv4Cidr {
            network: Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
        }; MAX_SOURCE_CIDRS],
        len: 0,
    };

    /// 由 CIDR 列表构造（校验上限/重复）。
    pub fn try_from_cidrs(cidrs: &[Ipv4Cidr]) -> Result<Self, ListenConfigError> {
        if cidrs.len() > MAX_SOURCE_CIDRS {
            return Err(ListenConfigError::TooManySources {
                max: MAX_SOURCE_CIDRS,
            });
        }
        let mut set = Self::EMPTY;
        for cidr in cidrs {
            if set.cidrs().iter().any(|candidate| candidate == cidr) {
                return Err(ListenConfigError::DuplicateSource {
                    cidr: cidr.to_string(),
                });
            }
            set.cidrs[set.len as usize] = *cidr;
            set.len += 1;
        }
        Ok(set)
    }

    /// 白名单条目数（0 = 旧行为）。
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// 是否为空（旧行为）。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 全部 CIDR（按配置顺序）。
    pub fn cidrs(&self) -> &[Ipv4Cidr] {
        &self.cidrs[..self.len()]
    }

    /// IPv4 地址是否被允许。
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        self.cidrs().iter().any(|cidr| cidr.contains(address))
    }

    /// 任意 IP 地址是否被允许（IPv6 来源一律不允许；本接收端不接 IPv6）。
    pub fn contains_ip(&self, address: IpAddr) -> bool {
        match address {
            IpAddr::V4(address) => self.contains(address),
            IpAddr::V6(_) => false,
        }
    }

    /// 描述文本（错误/报告用）。
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "any".to_string();
        }
        self.cidrs()
            .iter()
            .map(|cidr| cidr.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_interfaces_construct_and_validate() {
        let empty = ReceiverInterfaces::try_from_addresses(&[]).expect("empty");
        assert!(empty.is_empty());
        assert_eq!(empty.describe(), "0.0.0.0（所有接口）");
        let single =
            ReceiverInterfaces::try_from_addresses(&[Ipv4Addr::new(192, 0, 2, 222)]).expect("1");
        assert_eq!(single.len(), 1);
        assert_eq!(single.primary(), Some(Ipv4Addr::new(192, 0, 2, 222)));
        let dual = ReceiverInterfaces::try_from_addresses(&[
            Ipv4Addr::new(192, 0, 2, 222),
            Ipv4Addr::new(10, 0, 0, 5),
        ])
        .expect("2");
        assert_eq!(dual.len(), 2);
        assert!(dual.contains(Ipv4Addr::new(10, 0, 0, 5)));
        assert_eq!(dual.describe(), "192.0.2.222,10.0.0.5");
        // 重复 / 非法 / 超限。
        assert_eq!(
            ReceiverInterfaces::try_from_addresses(&[
                Ipv4Addr::LOCALHOST,
                Ipv4Addr::LOCALHOST
            ]),
            Err(ListenConfigError::DuplicateInterface {
                address: Ipv4Addr::LOCALHOST
            })
        );
        assert_eq!(
            ReceiverInterfaces::try_from_addresses(&[Ipv4Addr::UNSPECIFIED]),
            Err(ListenConfigError::NotInterfaceAddress {
                address: Ipv4Addr::UNSPECIFIED
            })
        );
        assert_eq!(
            ReceiverInterfaces::try_from_addresses(&[Ipv4Addr::new(239, 69, 1, 1)]),
            Err(ListenConfigError::NotInterfaceAddress {
                address: Ipv4Addr::new(239, 69, 1, 1)
            })
        );
        let too_many: Vec<Ipv4Addr> = (1..=9).map(|last| Ipv4Addr::new(10, 0, 0, last)).collect();
        assert_eq!(
            ReceiverInterfaces::try_from_addresses(&too_many),
            Err(ListenConfigError::TooManyInterfaces {
                max: MAX_RECEIVER_INTERFACES
            })
        );
    }

    #[test]
    fn group_allow_matches_and_rejects_non_multicast() {
        let allow = GroupAllow::try_from_groups(&[
            Ipv4Addr::new(239, 69, 16, 146),
            Ipv4Addr::new(239, 69, 16, 147),
        ])
        .expect("groups");
        assert!(allow.contains(Ipv4Addr::new(239, 69, 16, 146)));
        assert!(!allow.contains(Ipv4Addr::new(239, 69, 16, 148)));
        assert_eq!(
            GroupAllow::try_from_groups(&[Ipv4Addr::new(192, 0, 2, 222)]),
            Err(ListenConfigError::NotMulticastGroup {
                address: Ipv4Addr::new(192, 0, 2, 222)
            })
        );
        assert_eq!(
            GroupAllow::try_from_groups(&[
                Ipv4Addr::new(239, 69, 16, 146),
                Ipv4Addr::new(239, 69, 16, 146)
            ]),
            Err(ListenConfigError::DuplicateGroup {
                group: Ipv4Addr::new(239, 69, 16, 146)
            })
        );
    }

    #[test]
    fn ipv4_cidr_parse_and_match() {
        let cidr = Ipv4Cidr::parse("192.0.2.0/24").expect("cidr");
        assert_eq!(cidr.prefix_len(), 24);
        assert_eq!(cidr.network(), Ipv4Addr::new(192, 0, 2, 0));
        assert!(cidr.contains(Ipv4Addr::new(192, 0, 2, 222)));
        assert!(!cidr.contains(Ipv4Addr::new(192, 168, 64, 1)));
        // 主机位自动清零。
        let host = Ipv4Cidr::parse("192.0.2.222/24").expect("host cidr");
        assert_eq!(host.to_string(), "192.0.2.0/24");
        // 无前缀 = /32。
        let single = Ipv4Cidr::parse("192.0.2.222").expect("single");
        assert_eq!(single.to_string(), "192.0.2.222/32");
        assert!(single.contains(Ipv4Addr::new(192, 0, 2, 222)));
        assert!(!single.contains(Ipv4Addr::new(192, 0, 2, 223)));
        // /0 = 全放行。
        let any = Ipv4Cidr::parse("0.0.0.0/0").expect("any");
        assert!(any.contains(Ipv4Addr::new(8, 8, 8, 8)));
        // 非法。
        assert!(matches!(
            Ipv4Cidr::parse("192.0.2.0/33"),
            Err(ListenConfigError::BadPrefix { prefix: 33 })
        ));
        assert!(matches!(
            Ipv4Cidr::parse("not-an-address"),
            Err(ListenConfigError::BadCidr { .. })
        ));
        assert!(matches!(
            Ipv4Cidr::parse("192.0.2.0/x"),
            Err(ListenConfigError::BadCidr { .. })
        ));
    }

    #[test]
    fn source_allow_matches_and_dedupes() {
        let allow = SourceAllow::try_from_cidrs(&[
            Ipv4Cidr::parse("192.0.2.0/24").expect("a"),
            Ipv4Cidr::parse("192.0.2.222").expect("b"),
        ])
        .expect("allow");
        assert_eq!(allow.len(), 2);
        assert!(allow.contains(Ipv4Addr::new(192, 0, 2, 10)));
        assert!(allow.contains(Ipv4Addr::new(192, 0, 2, 222)));
        assert!(!allow.contains(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!allow.contains_ip(IpAddr::V6("::1".parse().expect("v6"))));
        assert!(allow.contains_ip(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))));
        assert_eq!(
            SourceAllow::try_from_cidrs(&[
                Ipv4Cidr::parse("192.0.2.0/24").expect("a"),
                Ipv4Cidr::parse("192.0.2.5/24").expect("b"),
            ]),
            Err(ListenConfigError::DuplicateSource {
                cidr: "192.0.2.0/24".to_string()
            })
        );
    }
}
