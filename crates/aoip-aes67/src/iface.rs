//! 本机 IPv4 网卡枚举与网卡名解析（**启动期一次性**；零外部 crate 依赖）。
//!
//! 用途（组播监听面启动期）：
//! - `[capture] aes67_iface` 允许写网卡名（`enp2s0`/`eth0`/`Ethernet`）而不是 IPv4 字面量；
//! - "aes67_iface 为空 = 0.0.0.0" 在多网卡机器上需要启动警告（统计非环回 IPv4 网卡数）。
//!
//! 机制（直接 FFI；与 `sockopt` 并列的第二个最小 extern 模块）：
//! - Linux/macOS：`getifaddrs(3)`（std 已链接 libc；Linux 板端 = 生产目标）；
//! - Windows：`iphlpapi!GetAdaptersAddresses(AF_INET)`（只取 IPv4 单播地址）；
//! - 其它平台：`Unsupported`（网卡名解析 fail closed；IPv4 字面量仍可用）。
//!
//! 纪律：只在启动期（配置解析/开 socket 之前）调用，**不进入收包热路径**；
//! 枚举失败一律返回 `Err`，调用方 fail closed（不猜网卡、不静默退回 0.0.0.0）。

use std::fmt;
use std::io;
use std::net::Ipv4Addr;

/// 本机一个 IPv4 网络接口。
///
/// 一张网卡可能有多个 IPv4 地址 = 多条记录（共享 `name`）；按名字解析时多地址 =
/// `IfaceLookupError::Ambiguous`（不猜用户想要哪一个）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv4Interface {
    /// 主名称（Linux `ifa_name`，如 `enp2s0`；Windows 友好名 FriendlyName，缺失时用 GUID）。
    pub name: String,
    /// 备用匹配名（Windows adapter GUID/`AdapterName`；Linux/macOS = None）。
    pub alt_name: Option<String>,
    /// 该网卡上的一个 IPv4 地址。
    pub address: Ipv4Addr,
    /// 是否环回（Linux `lo`；Windows 软件环回适配器 / 127.0.0.0/8）。
    pub loopback: bool,
    /// 接口是否 up（Linux `IFF_UP`；Windows `IfOperStatusUp`）。
    pub up: bool,
}

impl Ipv4Interface {
    /// 名称是否匹配本接口（主名或备用名；大小写不敏感）。
    pub fn matches_name(&self, name: &str) -> bool {
        if self.name.eq_ignore_ascii_case(name) {
            return true;
        }
        match self.alt_name.as_deref() {
            Some(alt) => alt.eq_ignore_ascii_case(name),
            None => false,
        }
    }
}

/// 网卡名解析错误（启动期；文案含可操作信息）。
#[derive(Debug)]
pub enum IfaceLookupError {
    /// 本平台不支持网卡名解析（请改用 IPv4 字面量）。
    Unsupported,
    /// 找不到该名字的网卡。
    NoSuchName {
        /// 用户写的名字。
        name: String,
        /// 本机已枚举到的网卡名（去重排序；错误信息里列出，便于改配置）。
        available: Vec<String>,
    },
    /// 同名网卡有多个 IPv4 地址（必须写 IPv4 字面量指定其一）。
    Ambiguous {
        /// 用户写的名字。
        name: String,
        /// 该名字下全部 IPv4 地址。
        addresses: Vec<Ipv4Addr>,
    },
    /// 枚举本机网卡失败（系统调用错误；fail closed）。
    Enumerate(io::Error),
}

impl fmt::Display for IfaceLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IfaceLookupError::Unsupported => {
                write!(f, "本平台不支持按网卡名解析 IPv4（请改用 IPv4 字面量）")
            }
            IfaceLookupError::NoSuchName { name, available } => {
                if available.is_empty() {
                    write!(
                        f,
                        "找不到网卡 \"{name}\"（本机未枚举到 IPv4 网卡；请改用 IPv4 字面量）"
                    )
                } else {
                    write!(
                        f,
                        "找不到网卡 \"{name}\"（本机 IPv4 网卡：{}）",
                        available.join(", ")
                    )
                }
            }
            IfaceLookupError::Ambiguous { name, addresses } => write!(
                f,
                "网卡 \"{name}\" 有多个 IPv4 地址（{}）；请直接写要使用的 IPv4 字面量",
                addresses
                    .iter()
                    .map(|address| address.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            IfaceLookupError::Enumerate(error) => {
                write!(f, "枚举本机 IPv4 网卡失败：{error}")
            }
        }
    }
}

impl std::error::Error for IfaceLookupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IfaceLookupError::Enumerate(error) => Some(error),
            _ => None,
        }
    }
}

/// 枚举本机全部 IPv4 接口地址（每地址一条；按 name/address 排序，输出确定）。
///
/// - 不返回 IPv6/链路层地址；
/// - 平台不支持或系统调用失败 → `Err`（调用方 fail closed）。
pub fn list_ipv4_interfaces() -> io::Result<Vec<Ipv4Interface>> {
    platform::list_ipv4_interfaces()
}

/// 非环回、up 的 IPv4 网卡数量（"0.0.0.0 = 所有接口"警告用）。
pub fn non_loopback_ipv4_interface_count() -> io::Result<usize> {
    Ok(list_ipv4_interfaces()?
        .into_iter()
        .filter(|interface| !interface.loopback && !interface.address.is_loopback() && interface.up)
        .map(|interface| interface.name)
        .collect::<std::collections::BTreeSet<_>>()
        .len())
}

/// 按网卡名解析 IPv4 地址（大小写不敏感；多个地址 → `Ambiguous`，不猜）。
pub fn resolve_interface_name(name: &str) -> Result<Ipv4Addr, IfaceLookupError> {
    let name = name.trim();
    let interfaces = list_ipv4_interfaces().map_err(IfaceLookupError::Enumerate)?;
    if name.is_empty() {
        return Err(IfaceLookupError::NoSuchName {
            name: name.to_string(),
            available: available_names(&interfaces),
        });
    }
    let mut found: Vec<Ipv4Addr> = Vec::new();
    for interface in interfaces
        .iter()
        .filter(|interface| interface.matches_name(name))
    {
        if !found.contains(&interface.address) {
            found.push(interface.address);
        }
    }
    match found.as_slice() {
        [] => Err(IfaceLookupError::NoSuchName {
            name: name.to_string(),
            available: available_names(&interfaces),
        }),
        [only] => Ok(*only),
        many => Err(IfaceLookupError::Ambiguous {
            name: name.to_string(),
            addresses: many.to_vec(),
        }),
    }
}

/// 去重排序的网卡主名列表（错误文案用）。
fn available_names(interfaces: &[Ipv4Interface]) -> Vec<String> {
    let mut names: Vec<String> = interfaces
        .iter()
        .map(|interface| interface.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(unix)]
mod platform {
    use super::Ipv4Interface;
    use std::ffi::CStr;
    use std::io;
    use std::net::Ipv4Addr;

    /// `AF_INET`（Linux/macOS 均为 2）。
    const AF_INET: u16 = 2;
    /// `IFF_UP`。
    const IFF_UP: u32 = 0x1;
    /// `IFF_LOOPBACK`。
    const IFF_LOOPBACK: u32 = 0x8;

    /// `struct ifaddrs`（Linux/macOS 64 位字段偏移一致：next/name/flags/addr/...）。
    #[repr(C)]
    struct IfAddrs {
        next: *const IfAddrs,
        name: *const core::ffi::c_char,
        flags: u32,
        addr: *const Sockaddr,
        netmask: *const Sockaddr,
        ifu: *const Sockaddr,
        data: *const core::ffi::c_void,
    }

    /// `struct sockaddr` 头部（只看 family + 前 14 字节；IPv4 地址位于 offset 4）。
    #[repr(C)]
    struct Sockaddr {
        family: u16,
        data: [u8; 14],
    }

    extern "C" {
        fn getifaddrs(ifap: *mut *mut IfAddrs) -> i32;
        fn freeifaddrs(ifa: *mut IfAddrs);
    }

    pub fn list_ipv4_interfaces() -> io::Result<Vec<Ipv4Interface>> {
        let mut head: *mut IfAddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs 写 head；失败返回非 0 且 head 不参与后续释放。
        let result = unsafe { getifaddrs(&mut head) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut interfaces = Vec::new();
        let mut cursor = head as *const IfAddrs;
        while !cursor.is_null() {
            // SAFETY: getifaddrs 返回的链表在 freeifaddrs 前保持有效。
            let entry = unsafe { &*cursor };
            if !entry.name.is_null() && !entry.addr.is_null() {
                // SAFETY: ifa_name 是 NUL 结尾 C 字符串；ifa_addr 指向有效 sockaddr。
                let name = unsafe { CStr::from_ptr(entry.name) }
                    .to_string_lossy()
                    .into_owned();
                // SAFETY: ifa_addr 已在上面判空，且按 ifaddrs 契约指向有效 sockaddr。
                let address = unsafe { &*entry.addr };
                if address.family == AF_INET {
                    let octets = [
                        address.data[2],
                        address.data[3],
                        address.data[4],
                        address.data[5],
                    ];
                    let ipv4 = Ipv4Addr::from(octets);
                    interfaces.push(Ipv4Interface {
                        name,
                        alt_name: None,
                        address: ipv4,
                        loopback: entry.flags & IFF_LOOPBACK != 0 || ipv4.is_loopback(),
                        up: entry.flags & IFF_UP != 0,
                    });
                }
            }
            cursor = entry.next;
        }
        // SAFETY: head 由 getifaddrs 分配，成功路径必须 freeifaddrs。
        unsafe { freeifaddrs(head) };
        interfaces.sort_by(|left, right| {
            (left.name.as_str(), left.address).cmp(&(right.name.as_str(), right.address))
        });
        Ok(interfaces)
    }
}

#[cfg(windows)]
mod platform {
    use super::Ipv4Interface;
    use std::io;
    use std::net::Ipv4Addr;

    /// `AF_INET`。
    const AF_INET: u32 = 2;
    /// `GAA_FLAG_SKIP_ANYCAST`。
    const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
    /// `GAA_FLAG_SKIP_MULTICAST`。
    const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
    /// `GAA_FLAG_SKIP_DNS_SERVER`。
    const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x0008;
    /// `ERROR_SUCCESS`。
    const ERROR_SUCCESS: u32 = 0;
    /// `ERROR_BUFFER_OVERFLOW`（缓冲区不足，按返回的 size 重试）。
    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    /// `ERROR_NO_DATA`（没有任何 adapter）。
    const ERROR_NO_DATA: u32 = 232;
    /// `IfOperStatusUp`。
    const IF_OPER_STATUS_UP: u32 = 1;
    /// `IF_TYPE_SOFTWARE_LOOPBACK`。
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

    /// `SOCKET_ADDRESS`。
    #[repr(C)]
    struct SocketAddress {
        sockaddr: *const u8,
        sockaddr_length: i32,
    }

    /// `IP_ADAPTER_UNICAST_ADDRESS_LH` 前缀（其后字段不需要；偏移必须与 winsock2.h 一致）。
    #[repr(C)]
    struct AdapterUnicastAddress {
        // union { ULONGLONG Alignment; struct { ULONG Length; DWORD Flags; } }
        alignment: u64,
        next: *const AdapterUnicastAddress,
        address: SocketAddress,
    }

    /// `IP_ADAPTER_ADDRESSES_LH` 前缀（到 IfOperStatus 为止；64/32 位由 repr(C) 自然对齐）。
    #[repr(C)]
    struct AdapterAddresses {
        // union { ULONGLONG Alignment; struct { ULONG Length; IF_INDEX IfIndex; } }
        alignment: u64,
        next: *const AdapterAddresses,
        adapter_name: *const u8,
        first_unicast: *const AdapterUnicastAddress,
        first_anycast: *const u8,
        first_multicast: *const u8,
        first_dns_server: *const u8,
        dns_suffix: *const u16,
        description: *const u16,
        friendly_name: *const u16,
        physical_address: [u8; 8],
        physical_address_length: u32,
        flags: u32,
        mtu: u32,
        if_type: u32,
        oper_status: u32,
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetAdaptersAddresses(
            family: u32,
            flags: u32,
            reserved: *mut core::ffi::c_void,
            addresses: *mut AdapterAddresses,
            size: *mut u32,
        ) -> u32;
    }

    pub fn list_ipv4_interfaces() -> io::Result<Vec<Ipv4Interface>> {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 16 * 1024;
        let mut buffer: Vec<u8> = Vec::new();
        let mut status = ERROR_BUFFER_OVERFLOW;
        // GetAdaptersAddresses 的 size 是"输入容量/输出需要"；最多重试 4 次（网卡在枚举期间变化）。
        for _ in 0..4 {
            buffer.clear();
            buffer.resize(size as usize, 0);
            // SAFETY: buffer 至少 size 字节且按 u8 分配（对齐足够容纳指针字段的 adapter 结构）。
            status = unsafe {
                GetAdaptersAddresses(
                    AF_INET,
                    flags,
                    std::ptr::null_mut(),
                    buffer.as_mut_ptr() as *mut AdapterAddresses,
                    &mut size,
                )
            };
            if status != ERROR_BUFFER_OVERFLOW {
                break;
            }
        }
        if status == ERROR_NO_DATA {
            return Ok(Vec::new());
        }
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let mut interfaces = Vec::new();
        let mut cursor = buffer.as_ptr() as *const AdapterAddresses;
        while !cursor.is_null() {
            // SAFETY: GetAdaptersAddresses 成功返回的链表指向 buffer 内部，调用期间有效。
            let adapter = unsafe { &*cursor };
            // SAFETY: friendly_name/adapter_name 指针由系统给出（可能为 NULL）。
            let friendly_name = unsafe { wide_to_string(adapter.friendly_name) };
            // SAFETY: 同上。
            let adapter_name = unsafe { ansi_to_string(adapter.adapter_name) };
            let primary = friendly_name
                .clone()
                .or_else(|| adapter_name.clone())
                .unwrap_or_default();
            let alt = if friendly_name.is_some() {
                adapter_name
            } else {
                None
            };
            let loopback_adapter = adapter.if_type == IF_TYPE_SOFTWARE_LOOPBACK;
            let mut unicast = adapter.first_unicast;
            while !unicast.is_null() {
                // SAFETY: first_unicast 链表同样指向 buffer 内部。
                let entry = unsafe { &*unicast };
                let sockaddr = entry.address.sockaddr;
                if !sockaddr.is_null() && entry.address.sockaddr_length >= 8 {
                    // SAFETY: SOCKET_ADDRESS 给出地址与长度；先读 family，再按 AF_INET 读 4 字节地址。
                    let family = unsafe { *(sockaddr as *const u16) };
                    if family as u32 == AF_INET {
                        // SAFETY: AF_INET 的 sockaddr_in 至少 8 字节（family/port/addr）。
                        let octets = unsafe {
                            [
                                *sockaddr.add(4),
                                *sockaddr.add(5),
                                *sockaddr.add(6),
                                *sockaddr.add(7),
                            ]
                        };
                        let address = Ipv4Addr::from(octets);
                        interfaces.push(Ipv4Interface {
                            name: if primary.is_empty() {
                                "adapter".to_string()
                            } else {
                                primary.clone()
                            },
                            alt_name: alt.clone().filter(|alt| !alt.is_empty() && alt != &primary),
                            address,
                            loopback: loopback_adapter || address.is_loopback(),
                            up: adapter.oper_status == IF_OPER_STATUS_UP,
                        });
                    }
                }
                unicast = entry.next;
            }
            cursor = adapter.next;
        }
        interfaces.sort_by(|left, right| {
            (left.name.as_str(), left.address).cmp(&(right.name.as_str(), right.address))
        });
        interfaces.dedup_by(|left, right| left.name == right.name && left.address == right.address);
        Ok(interfaces)
    }

    /// ANSI C 字符串 → String（最多 256 字节；非 UTF-8 用 lossy）。
    unsafe fn ansi_to_string(pointer: *const u8) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        // SAFETY: 调用方保证 pointer 指向 NUL 结尾的有效字符串（最多读 256 字节）。
        let length = unsafe {
            let mut length = 0usize;
            while length < 256 && *pointer.add(length) != 0 {
                length += 1;
            }
            length
        };
        if length == 0 {
            return None;
        }
        // SAFETY: 已按 NUL 截断到 length 字节，pointer 的该范围有效。
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// UTF-16 C 字符串 → String（最多 256 个码元）。
    unsafe fn wide_to_string(pointer: *const u16) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        // SAFETY: 调用方保证 pointer 指向 NUL 结尾的有效宽字符串（最多读 256 码元）。
        let length = unsafe {
            let mut length = 0usize;
            while length < 256 && *pointer.add(length) != 0 {
                length += 1;
            }
            length
        };
        if length == 0 {
            return None;
        }
        // SAFETY: 已按 NUL 截断到 length 个 u16，pointer 的该范围有效。
        let units = unsafe { std::slice::from_raw_parts(pointer, length) };
        Some(String::from_utf16_lossy(units))
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::Ipv4Interface;
    use std::io;

    pub fn list_ipv4_interfaces() -> io::Result<Vec<Ipv4Interface>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IPv4 interface enumeration not wired on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_interface_name_is_rejected() {
        match resolve_interface_name("khaen-no-such-nic-xyz") {
            Err(IfaceLookupError::NoSuchName { name, .. }) => {
                assert_eq!(name, "khaen-no-such-nic-xyz");
            }
            other => panic!("未知网卡名必须返回 NoSuchName，got {other:?}"),
        }
    }

    #[test]
    fn local_interface_name_resolves_to_its_address() {
        let interfaces = list_ipv4_interfaces().expect("枚举本机 IPv4 网卡");
        assert!(interfaces
            .iter()
            .all(|interface| !interface.name.is_empty()));
        for interface in &interfaces {
            let same_name = interfaces
                .iter()
                .filter(|candidate| candidate.name == interface.name)
                .count();
            // 同名多地址 = Ambiguous 属预期契约；唯一名必须能解析回同一地址。
            if same_name == 1 {
                assert_eq!(
                    resolve_interface_name(&interface.name).ok(),
                    Some(interface.address),
                    "网卡 {} 名字解析失败",
                    interface.name
                );
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_loopback_name_resolves() {
        match resolve_interface_name("lo") {
            Ok(address) => assert_eq!(address, Ipv4Addr::LOCALHOST),
            Err(IfaceLookupError::NoSuchName { .. }) => {
                // 极简容器可能没有 lo：跳过（负例由 unknown_interface_name_is_rejected 覆盖）。
                eprintln!("本机没有 lo 网卡，跳过 lo 正例");
            }
            other => panic!("lo 解析异常：{other:?}"),
        }
    }

    #[test]
    fn non_loopback_count_does_not_error() {
        match non_loopback_ipv4_interface_count() {
            Ok(_count) => {}
            Err(error) => panic!("统计网卡数失败：{error}"),
        }
    }
}
