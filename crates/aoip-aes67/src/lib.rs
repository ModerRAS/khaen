//! 用户态 AES67 协议栈（纯 std、零第三方依赖）：媒体面 + 发现/协商控制面。
//!
//! 媒体面链路：
//!
//! ```text
//! SDP（RFC 4566/7273 子集）──► 组播/单播 UDP + SO_RCVBUF ──► RTP 头/L24(24bit BE)
//!     ──► 抖动缓冲（预分配固定帧环，绝对媒体帧号 + L24 解码）──► capture 固定块
//!     （48k / 128 帧；交错 i32 满量程，由 host 适配层转 planar f32 交给图输入）
//! ```
//!
//! 设计要点：
//! - **热路径零分配**：所有缓冲在 `open`/`new` 启动期一次性分配；IO 热路径只做
//!   固定大小写（RTP 头解析、L24 解码、环写入），不产生任何堆分配/格式化。
//! - **无锁 SPSC 环**：IO 线程（producer）→ RT/capture 线程（consumer）用原子游标 +
//!   frame valid bitset 通信（`jitter`），不持锁、不 syscall、不等待。
//! - **固定块**：consumer 每次取 64/128 帧（48k / 128 = 2.667 ms）；欠载输出整块静音
//!   并计数（不半块输出/不阻塞）。
//! - **媒体时钟**：`clock::MediaClock` trait；离线 = `UsrvClock`（单调时基）；
//!   真实 `/dev/ptp0` PHC 只剩 trait 接线（`PtpPhcClock` 当前 NOT_WIRED，需板端接入）。
//! - **错误计数不阻塞**：丢包/乱序/欠载/溢出/过期只累加原子计数，收包路径永不失败等待。
//!
//! 控制面（发现/协商）：`sdp_write`（SDP 生成，RFC 8866 子集）+ `sap`（RFC 2974
//! 通告/监听子集）、`rtsp`（RTSP/1.0 子集，RFC 2326；RAVENNA 连接管理，服务端
//! OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN + 客户端 DESCRIBE→SETUP→PLAY→TEARDOWN）、
//! `http_sdp`（极简 HTTP/1.1 GET SDP 分发/拉取）、`mdns`（DNS-SD 子集：A/PTR/SRV/TXT
//! 编解码、mDNS 通告/浏览/缓存/TTL/注销）。控制面不参与 RT 音频路径、不分配 RT 端口。
//!
//! 边界：真实板端链路/电平、PTP 锁、IGMP/组播网络、真实 H16 源全部 **NOT_RUN**（见
//! crate README）；`sender` 模块只是接收端自带的 loopback 载荷源，生产发送端/CLI
//! 不在本 crate 内。
//!
//! 迁移说明：本 crate 自 legacy 分支 `rt-stack/crates/aoip-aes67` 原样迁入（模块与
//! 单元测试全部保留），包名沿用 `khaen-aoip-aes67`、保持纯 std 无第三方依赖。源码注释
//! 中的 task-NNN/TNN 是迁移前的任务编号，仅作历史溯源。真实 socket 测试（loopback /
//! mDNS / SAP / RTSP / HTTP）在无网/受限环境自动跳过，也可用
//! `KHAEN_AES67_SKIP_NET_TESTS=1` 强制跳过（见 `testnet`）。

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

pub mod capture;
pub mod clock;
pub mod http_sdp;
pub mod iface;
pub mod input_profile;
pub mod jitter;
pub mod listen;
pub mod mdns;
pub mod receiver;
pub mod rtp;
pub mod rtsp;
pub mod sap;
pub mod sdp;
pub mod sdp_write;
pub mod sender;

mod sockopt;

#[cfg(test)]
mod testnet;

/// 本 crate 支持的媒体采样率（AES67 媒体面 48k；H16 源固定 48k）。
pub const AES67_RATE_HZ: u32 = sdp::AES67_RATE_HZ;
/// 固定块帧数（48k / 128 = 2.667 ms；与核心引擎 64/128 的 block 合同一致）。
pub const CAPTURE_BLOCK_FRAMES: usize = 128;
/// 单条 flow 最大通道数（AES67 媒体面 profile）。
pub const AES67_MAX_CHANNELS_PER_FLOW: u16 = sdp::AES67_MAX_CHANNELS_PER_FLOW;

/// crate 版本号（诊断/报告用；不参与 RT 决策）。
pub const PROTO_VERSION: &str = env!("CARGO_PKG_VERSION");
