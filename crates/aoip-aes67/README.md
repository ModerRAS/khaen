# khaen-aoip-aes67

用户态 AES67 协议栈 crate（**纯 std、零第三方依赖**）。覆盖两条线：

1. **媒体面**：SDP 解析 → RTP L24 big-endian → 抖动缓冲 → 媒体时钟 → capture 固定块；
   `sender` 模块提供接收端自带的 loopback 载荷源（生产发送端/CLI 不在本 crate 内）。
2. **发现/协商控制面**：SDP 生成、SAP 通告/监听、RTSP/1.0 子集（RAVENNA 连接管理）、
   极简 HTTP SDP 分发、mDNS / DNS-SD 设备与流发现。

## 模块

| 模块 | 作用 |
| --- | --- |
| `sdp` | SDP 解析（v/o/s/c/t/m、rtpmap、ptime、framecount、ts-refclk、mediaclk、channel-order、source-filter、方向）；未知属性忽略 |
| `sdp_write` | SDP 生成（与解析器写-读互逆；CRLF、严格 ASCII） |
| `sap` | SAP 通告器/监听器/会话表（EV 头、msg id hash、删除包、周期通告、快照）；认证/加密/压缩 fail closed |
| `rtsp` | RTSP/1.0 子集：服务端 OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN + 客户端 DESCRIBE→SETUP→PLAY→TEARDOWN；Transport 解析/回显；会话表/超时清理；单连接多请求/并发多客户端 |
| `http_sdp` | 极简 HTTP/1.1：GET /<stream>.sdp（application/sdp + Content-Length；404/405/400/505）+ 客户端拉取 |
| `mdns` | DNS-SD 子集：A/PTR/SRV/TXT 编解码（名字压缩）、服务模型、mDNS 通告/应答/浏览、TTL 缓存/goodbye 注销；服务类型/子类型按 RAVENNA OP |
| `receiver` | 组播/单播 UDP 收包线程 + 抖动缓冲 + 白名单/接口约束（IO 热路径零分配） |
| `jitter` | 预分配固定帧环 + 绝对媒体帧号 + 原子游标 SPSC（无锁、零分配） |
| `clock` | `MediaClock` trait：`UsrvClock`（单调用时基）/ `MockClock`（单测）；`PtpPhcClock` = NOT_WIRED（需板端接 `/dev/ptp0`） |
| `capture` | 抖动缓冲 → host 图输入固定块（64/128 帧，交错 i32 满量程） |
| `rtp` | RTP 头编解码 + L24（24 bit BE）打包/解包 + 满量程 i32 ↔ f32 归一化 |
| `sender` | 最小 RTP/L24 发送端（正弦/静音/任意样本；仅 loopback 测试与示例用） |
| `iface` / `listen` / `sockopt` | 网卡枚举与名称解析、组播监听配置（接口列表/组白名单/来源白名单）、SO_RCVBUF 调优 |

## 协议矩阵（控制面）

| 能力 | 实现 | 规范依据 |
| --- | --- | --- |
| RTSP OPTIONS / DESCRIBE / SETUP / PLAY / TEARDOWN | 服务端 + 客户端 | RFC 2326 §10.1/10.2/10.4/10.5/10.7 |
| CSeq 请求/响应配对（缺失 400） | 是 | RFC 2326 §12.17 |
| Transport: RTP/AVP;multicast;destination;port;ttl / unicast;client_port | 解析 + 回显 | RFC 2326 §12.39 |
| 404 保持连接（RAVENNA 推荐） | 是 | RAVENNA OP §3.4.1 |
| 501 未实现方法 / 461 不支持传输 / 454 会话不存在 | 是 | RFC 2326 表 2 注、§11.3.5、§11.3.12 |
| /by-name/<name> 与 /by-id/<id> 寻址、百分号解码 | 是 | RAVENNA OP §3.5.3 |
| 会话/连接空闲超时清理（默认 60 s，可配） | 是 | RFC 2326 §12.37 |
| ANNOUNCE 异步 SDP 推送 | **未实现**（404 保持连接已实现） | RAVENNA OP §3.4.1（推荐非强制） |
| HTTP GET SDP、Content-Type/Content-Length、404/405/400/505 | 是 | 通用 HTTP/1.1（EXTENSION：RAVENNA 未规定 HTTP 取 SDP 路径） |
| DNS 报文编解码（A/PTR/SRV/TXT，名字压缩，9000 B 上限） | 是 | RFC 1035 §4、RFC 6762 §18 |
| DNS-SD 服务实例枚举/解析（PTR→SRV→TXT→A） | 是 | RFC 6763 §4/§5/§12 |
| mDNS 组播 224.0.0.251:5353、TTL 120/4500、goodbye TTL 0 + 1 s 宽限 | 是（可配单播测试） | RFC 6762 §3/§10/§10.1 |
| 启动通告（≥2 次、间隔 ≥1 s） | 是 | RFC 6762 §8.3 |
| RAVENNA 服务类型：_rtsp._tcp / _http._tcp + _ravenna 子类型；会话 <name>._rtsp._tcp + _ravenna_session 子类型 | 是（构造函数内置） | RAVENNA OP §3.5.1/§3.5.2 |
| TXT（RAVENNA 可选；空 = 单个空串） | 是 | RFC 6763 §6；RAVENNA OP §3.5.1.1 |
| 探测/冲突改名、已知答案抑制、重复问题抑制、NSEC 否定应答、AAAA/IPv6、单播遗留应答 | **未实现** | RFC 6762 §7/§8.1/§9/§6.7（NOT_RUN） |

## 最小用法（控制面）

```rust,no_run
use std::net::Ipv4Addr;
use khaen_aoip_aes67::http_sdp::{HttpSdpServer, HttpSdpServerConfig, HttpSdpStream};
use khaen_aoip_aes67::mdns::{
    ravenna_node_service, ravenna_session_service, DnsName, MdnsAdvertiser, MdnsAdvertiserConfig,
    MdnsBrowser, MdnsBrowserConfig, MdnsEndpoint, RavennaNodeService,
};
use khaen_aoip_aes67::rtsp::{RtspClient, RtspServer, RtspServerConfig, RtspStream};

fn demo(sdp: String) -> Result<(), Box<dyn std::error::Error>> {
    // 1) RTSP 服务端（DESCRIBE 返回 sdp_write 生成的 SDP；每连接一线程）
    let server = RtspServer::start(
        RtspServerConfig::new("0.0.0.0:8854".parse()?),
        vec![RtspStream::new("Line 1", sdp.clone()).with_id(1)],
    )?;
    // 客户端：DESCRIBE → SETUP → PLAY（解析 SDP 交给 receiver 配置）
    let client = RtspClient::with_timeout(std::time::Duration::from_secs(3));
    let mut playback = client.open("rtsp://192.0.2.222:8854/by-name/Line%201")?;
    let _receiver_config = playback.receiver_config(None)?; // ReceiverConfig::from_session
    playback.teardown()?;
    server.stop();

    // 2) HTTP SDP 分发 / 拉取
    let http = HttpSdpServer::start(
        HttpSdpServerConfig::new("0.0.0.0:8080".parse()?),
        vec![HttpSdpStream::new("Line 1", sdp.clone())],
    )?;
    let _fetched = khaen_aoip_aes67::http_sdp::http_get_sdp(
        "http://192.0.2.222:8080/Line%201.sdp",
        std::time::Duration::from_secs(3),
    )?;
    http.stop();

    // 3) mDNS/DNS-SD：通告（真实组播用 MdnsEndpoint::default_v4()；单测可改单播）
    let host = DnsName::from_str("khaen-aes67-1.local.")?;
    let node = ravenna_node_service(
        "My-Corp devicex 1234-5678",
        RavennaNodeService::Rtsp,
        host.clone(),
        8854,
        Ipv4Addr::new(192, 168, 63, 222),
    )?;
    let session = ravenna_session_service("Line 1", host, 8854, Ipv4Addr::new(192, 168, 63, 222))?;
    let advertiser = MdnsAdvertiser::start(
        MdnsAdvertiserConfig::new(MdnsEndpoint::default_v4()),
        vec![node, session],
    )?;
    let browser = MdnsBrowser::start(MdnsBrowserConfig::new(
        MdnsEndpoint::default_v4(),
        DnsName::from_str("_ravenna_session._sub._rtsp._tcp.local.")?,
    ))?;
    browser.query()?;
    for instance in browser.instances() {
        println!(
            "{} {}:{} {:?}",
            instance.instance, instance.host, instance.port, instance.addresses
        );
    }
    advertiser.stop(); // 发 goodbye（TTL 0）
    Ok(())
}
```

## 测试

```text
cargo test -p khaen-aoip-aes67 --offline
# lib 94 + tests/loopback 4 + tests/zero_alloc 1 = 99 passed, 0 failed
```

单元测试覆盖：SDP 解析/生成互逆、SAP 编解码与周期通告、RTSP 服务端/客户端 loopback 全流程
（单连接多请求、并发客户端、会话超时、错误状态码）、HTTP GET/错误路径、mDNS/DNS 报文与
缓存/goodbye、RTP/L24 编解码、抖动缓冲（乱序/迟到/重复/溢出/欠载/reanchor）、媒体时钟换算、
capture 固定块、SO_RCVBUF 回读；`zero_alloc` 用计数 GlobalAlloc 证明热路径零分配。

### 真实网络测试的跳过开关

loopback / mDNS / SAP / RTSP / HTTP 测试走**真实 socket**。无网/受限环境（CI 沙箱、没有回环
socket 的网络命名空间）会自动跳过这些用例（stderr 打印 `SKIPPED <test>`，其余用例照常断言），
不会谎报失败：

```powershell
# 自动探测失败时跳过；也可显式强制跳过（例如本机有网络但只想跑离线用例）
$env:KHAEN_AES67_SKIP_NET_TESTS = "1"
cargo test -p khaen-aoip-aes67 --offline
```

## 边界（NOT_RUN / NOT_WIRED / EXTENSION / UNVERIFIED）

- **NOT_RUN**：真实板端链路（网卡/IGMP/交换机 PTP、真实 H16 源、电平/静音窗口、长稳/资源）；
  真实组播网络（224.0.0.251:5353 跨主机/跨交换机）、与第三方 RAVENNA 控制器/设备
  （Lawo/Merging/DirectOut 等）或 ffprobe/ffmpeg 的 RTSP + DNS-SD 互通、5353 与系统 mDNS
  守护共存（Windows 多进程监听、Android 系统占用）、ANNOUNCE 推送。离线上限止于本机 loopback
  （RTSP/HTTP TCP、mDNS 单播注入、UDP 回环）。
- **NOT_WIRED**：RTSP SETUP 不分配 RTP/RTCP 端口（控制面；组播回显 SDP 的
  destination/port/ttl，单播回显 client_port + SDP 发送端口）；mDNS 多网卡严格出接口
  （IP_MULTICAST_IF）未接（std UdpSocket 无跨平台 API）；`PtpPhcClock` 的 `/dev/ptp0`
  PHC 读数与锁状态需板端接线。
- **EXTENSION**：单播 SETUP 的 server_port 取值；默认组播 TTL 32；HTTP 取 SDP 的
  路径/语义（RAVENNA 只在 OP §3.4 规定 HTTP 配置服务，未规定 SDP 分发路径）；
  RAVENNA TXT 的具体 key（OP §3.5.1.1 只说 MAY 放节点信息，本实现默认不写）。
- **UNVERIFIED**：HTTP/1.1 报文细节按通用语法实现并用 loopback 报文自测（未逐条对照
  RFC 7230/2616）；mDNS/DNS 报文按 RFC 1035/6762/6763 实现并用已知报文向量自测。
- 不覆盖：SIP（AES67 单播连接管理）、NMOS IS-04、RTCP、RAVENNA clock-domain/sync-time 的
  媒体时钟接线（发现层只透传 SDP 文本）。

## 迁移说明

本 crate 自旧仓库 `legacy/pre-rewrite` 分支的 `rt-stack/crates/aoip-aes67` 原样迁入：
模块与全部单元测试保留，包名 `khaen-aoip-aes67`、零第三方依赖（纯 std）。源码注释里的
task-NNN/TNN 是迁移前的任务编号，仅作历史溯源；迁移只做了三件事：

1. 断开与旧仓库外部路径/产物目录的文档引用，测试全部改用合成夹具或本机回环；
2. 为真实 socket 测试加自动跳过开关（见上）；
3. 修 clippy `-D warnings` 的写法类告警（div_ceil / is_multiple_of / 迭代器写法），
   行为与断言不变。
