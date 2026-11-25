//! 自研用户态 AES67 接收端原型（**task-235**）。
//!
//! 用户批准 AES67 走自研用户态接收端（Rust，禁止 Python）：本 crate 实现最小可验证链路
//!
//! ```text
//! SDP（RFC 4566/7273 子集）──► 组播/单播 UDP + SO_RCVBUF ──► RTP 头/L24(24bit BE)
//!     ──► 抖动缓冲（预分配固定帧环，绝对媒体帧号 + L24 解码）──► capture 固定块
//!     （48k / 128 帧，planar f32 交给 rt-host 图输入）
//! ```
//!
//! 设计要点（详见 evidence/aes67-rx-proto/RT-DESIGN.md）：
//! - **热路径零分配**：所有缓冲在 `open`/`new` 启动期一次性分配；IO 热路径只做
//!   固定大小写（RTP 头解析、L24 解码、环写入），不产生任何堆分配/格式化。
//! - **无锁 SPSC 环**：IO 线程（producer）→ RT/capture 线程（consumer）用原子游标 +
//!   frame valid bitset 通信（`jitter`），不持锁、不 syscall、不等待。
//! - **固定块**：consumer 每次取 128 帧（48k = 2.667 ms），与 TSPI core-v1 的
//!   block 合同（64/128）一致；欠载输出整块静音并计数（不半块输出/不阻塞）。
//! - **媒体时钟**：`clock::MediaClock` trait；离线 = `UsrvClock`（单调时基）；
//!   真实 `/dev/ptp0` PHC 只剩 trait 接线（`PtpPhcClock` 当前 NOT_WIRED，板端接）。
//! - **错误计数不阻塞**：丢包/乱序/欠载/溢出/过期只累加原子计数，收包路径永不失败等待。
//!
//! 边界：本 crate 是**离线原型**；板端真实链路/电平、PTP 锁、IGMP/组播、真实 H16
//! 源全部 **NOT_RUN**（见 evidence/aes67-rx-proto/NOT_RUN.md）。不 git、不碰板端、
//! 不改 CueMix。生产发送端归 task-236（rt-stack/tools/aes67-sender），本 crate 内
//! `sender` 模块只用于自有 loopback 集成测试/示例，不写入 tools 目录。
//!
//! task-315（P0 发现面）：`sdp_write`（SDP 生成，RFC 8866 子集）+ `sap`（RFC 2974
//! 通告/监听子集）补齐此前由第三方 aes67-daemon 承担的发现/通告角色。
//!
//! task-317（P1 发现/协商面）：`rtsp`（RTSP/1.0 子集，RFC 2326；RAVENNA 连接管理，
//! 服务端 OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN + 客户端 DESCRIBE→SETUP→PLAY→TEARDOWN）、
//! `http_sdp`（极简 HTTP/1.1 GET SDP 分发/拉取）、`mdns`（DNS-SD 子集：A/PTR/SRV/TXT
//! 编解码、mDNS 通告/浏览/缓存/TTL/注销）。三者仍是**控制面**：不参与 RT 音频路径、
//! 不分配 RT 端口；真实组播网络/第三方控制器互通/板端全部 NOT_RUN（见 crate README）。
//!
//! 来源：用户 2026-11-18 决策（自研 AES67 用户态接收）；rt-stack/spec/core-v1.md
//! §3.2/§6.1（48k、block 64/128、capture 输入平面 f32）；T43 aoip.rs（AES67 SDP
//! 只读解析/A12–A18，本 crate 对齐其字段语义但不重写端点层）。

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

pub mod capture;
pub mod clock;
pub mod http_sdp;
pub mod iface;
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

/// 本 crate 支持的媒体采样率（AES67 媒体面 48k；H16 源固定 48k）。
pub const AES67_RATE_HZ: u32 = sdp::AES67_RATE_HZ;
/// 固定块帧数（48k / 128 = 2.667 ms；与 core-v1 BLOCK_SIZES 的 128 一致）。
pub const CAPTURE_BLOCK_FRAMES: usize = 128;
/// 单条 flow 最大通道数（AES67 媒体面 profile）。
pub const AES67_MAX_CHANNELS_PER_FLOW: u16 = sdp::AES67_MAX_CHANNELS_PER_FLOW;

/// 原型版本号（证据 JSON 用；不参与 RT 决策）。
pub const PROTO_VERSION: &str = env!("CARGO_PKG_VERSION");
