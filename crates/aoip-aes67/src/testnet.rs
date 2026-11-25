//! 单元测试专用：真实 socket 测试的可用性探针（迁移要求：受限/无网环境要能跳过）。
//!
//! 本 crate 的 loopback/mDNS/SAP/RTSP/HTTP 测试走**真实 socket**（127.0.0.1 回环、
//! 组播 join、TCP listen）。在无网/受限沙箱里这些测试必须显式跳过而不是谎报失败；
//! 策略：
//!
//! - 默认探测本机回环：UDP 与 TCP 各 bind 一次 127.0.0.1:0，任一失败即视为不可用；
//! - 环境变量 `KHAEN_AES67_SKIP_NET_TESTS` 置非空且非 `0` 时强制跳过（受限环境即使
//!   bind 通过，也可能有组播/投递策略限制）；
//! - 跳过时向 stderr 打印 `SKIPPED <test>`（与既有组播 skip 语义一致），测试用例本体
//!   与断言一行不改；网络可用时照常执行、覆盖不变。
//!
//! 这只影响 `#[cfg(test)]` 构建，不进入发布库。

/// 环境变量：置为非空且非 `0` 时强制跳过真实网络测试。
pub const SKIP_ENV: &str = "KHAEN_AES67_SKIP_NET_TESTS";

/// 真实网络测试是否可用：回环 UDP/TCP 均可 bind，且未被环境变量强制跳过。
pub fn network_available() -> bool {
    if skip_requested() {
        return false;
    }
    let udp = std::net::UdpSocket::bind(("127.0.0.1", 0)).is_ok();
    let tcp = std::net::TcpListener::bind(("127.0.0.1", 0)).is_ok();
    udp && tcp
}

/// 网络不可用时打印跳过原因并返回 `true`（调用方立即 `return`）。
pub fn skip_if_unavailable(test_name: &str) -> bool {
    if network_available() {
        return false;
    }
    eprintln!(
        "SKIPPED {test_name}: loopback sockets unavailable (offline/restricted environment); \
         set {SKIP_ENV}=1 to force skip elsewhere, real network keeps coverage"
    );
    true
}

fn skip_requested() -> bool {
    match std::env::var(SKIP_ENV) {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    }
}
