# khaen-aoip-aes67

自研用户态 AES67 协议 crate（纯 std、**零第三方依赖**）。覆盖三条线：

1. **媒体面**（task-235）：SDP 解析 → RTP L24 BE → 抖动缓冲 → 媒体时钟 → capture 固定块。
2. **P0 发现面**（task-315）：**SDP 生成**（RFC 8866 子集）+ **SAP 通告/监听**
   （RFC 2974 子集，替代第三方 aes67-daemon 的发现/通告角色）。
3. **P1 发现/协商面**（task-317，本分支新增）：**RTSP**（RFC 2326 子集，RAVENNA 连接管理）、
   **HTTP SDP 分发**（极简 HTTP/1.1 GET）、**mDNS/DNS-SD**（RAVENNA 设备/流发现，
   AES67-2023 Annex E.6）。

## 模块

| 模块 | 作用 | 状态 |
| --- | --- | --- |
| sdp | SDP 解析（v/o/s/c/t/m、rtpmap、ptime、framecount、ts-refclk、mediaclk、channel-order、source-filter、方向） | P0；未知属性忽略 |
| sdp_write | SDP 生成（与解析器写-读互逆；CRLF、严格 ASCII） | P0（task-315） |
| sap | SAP 通告器/监听器/会话表（EV 头、msg id hash、删除包、周期通告、快照） | P0（task-315）；认证/加密/压缩 fail closed |
| rtsp | RTSP/1.0 子集：服务端 OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN + 客户端 DESCRIBE→SETUP→PLAY→TEARDOWN；Transport 解析/回显；会话表/超时清理；单连接多请求/并发多客户端 | P1（task-317） |
| http_sdp | 极简 HTTP/1.1：GET /<stream>.sdp（application/sdp + Content-Length；404/405/400/505）+ 客户端拉取 | P1（task-317） |
| mdns | DNS-SD 子集：A/PTR/SRV/TXT 编解码（名字压缩）、服务模型、mDNS 通告/应答/浏览、TTL 缓存/goodbye 注销；服务类型/子类型按 RAVENNA OP | P1（task-317） |
| receiver / jitter / clock / capture / rtp / sender | 媒体面（接收端原型 + 自有 loopback 发送端） | P0（task-235） |

## 协议矩阵（P1）

| 能力 | 实现 | 规范依据 |
| --- | --- | --- |
| RTSP OPTIONS / DESCRIBE / SETUP / PLAY / TEARDOWN | 服务端 + 客户端 | RFC 2326 §10.1/10.2/10.4/10.5/10.7 |
| CSeq 请求/响应配对（缺失 400） | 是 | RFC 2326 §12.17 L2775-2781 |
| Transport: RTP/AVP;multicast;destination;port;ttl / unicast;client_port | 解析 + 回显 | RFC 2326 §12.39 L3230-3414 |
| 404 保持连接（RAVENNA 推荐） | 是 | RAVENNA OP §3.4.1 L578-584 |
| 501 未实现方法 / 461 不支持传输 / 454 会话不存在 | 是 | RFC 2326 表 2 注 L1648-1652、§11.3.5、§11.3.12 |
| /by-name/<name> 与 /by-id/<id> 寻址、百分号解码 | 是 | RAVENNA OP §3.5.3 L666-707 |
| 会话/连接空闲超时清理（默认 60 s，可配） | 是 | RFC 2326 §12.37 L3185/L3199 |
| ANNOUNCE 异步 SDP 推送 | **未实现**（404 保持连接已实现） | RAVENNA OP §3.4.1（推荐非强制） |
| HTTP GET SDP、Content-Type/Content-Length、404/405/400/505 | 是 | 通用 HTTP/1.1（EXTENSION：RAVENNA 未规定 HTTP 取 SDP 路径） |
| DNS 报文编解码（A/PTR/SRV/TXT，名字压缩，9000 B 上限） | 是 | RFC 1035 §4、RFC 6762 §18 |
| DNS-SD 服务实例枚举/解析（PTR→SRV→TXT→A） | 是 | RFC 6763 §4/§5/§12 |
| mDNS 组播 224.0.0.251:5353、TTL 120/4500、goodbye TTL 0 + 1 s 宽限 | 是（可配单播测试） | RFC 6762 §3/§10/§10.1 |
| 启动通告（≥2 次、间隔 ≥1 s） | 是 | RFC 6762 §8.3 L1661-1665 |
| RAVENNA 服务类型：_rtsp._tcp / _http._tcp + _ravenna 子类型；会话 <name>._rtsp._tcp + _ravenna_session 子类型 | 是（构造函数内置） | RAVENNA OP §3.5.1 L607-623、§3.5.2 L640-649 |
| TXT（RAVENNA 可选；空 = 单个空串） | 是 | RFC 6763 §6 L586-592；RAVENNA OP §3.5.1.1 L628-632 |
| 探测/冲突改名、已知答案抑制、重复问题抑制、NSEC 否定应答、AAAA/IPv6、单播遗留应答 | **未实现** | RFC 6762 §7/§8.1/§9/§6.7（NOT_RUN） |

## 最小用法（P1）

~~~rust
use std::net::Ipv4Addr;
use khaen_aoip_aes67::mdns::{
    ravenna_node_service, ravenna_session_service, DnsName, MdnsAdvertiser,
    MdnsAdvertiserConfig, MdnsBrowser, MdnsBrowserConfig, MdnsEndpoint, RavennaNodeService,
};
use khaen_aoip_aes67::rtsp::{RtspClient, RtspServer, RtspServerConfig, RtspStream};
use khaen_aoip_aes67::http_sdp::{HttpSdpServer, HttpSdpServerConfig, HttpSdpStream};

// 1) 控制面：RTSP 服务端（DESCRIBE 返回 sdp_write 生成的 SDP；每连接一线程）
let server = RtspServer::start(
    RtspServerConfig::new("0.0.0.0:8854".parse()?),
    vec![RtspStream::new("Line 1", sdp.clone()).with_id(1)],
)?;
// 客户端：DESCRIBE → SETUP → PLAY（解析 SDP 交给既有 receiver 配置）
let client = RtspClient::with_timeout(std::time::Duration::from_secs(3));
let mut playback = client.open("rtsp://192.0.2.222:8854/by-name/Line%201")?;
let receiver_config = playback.receiver_config(None)?; // ReceiverConfig::from_session
playback.teardown()?;

// 2) HTTP SDP 分发/拉取
let http = HttpSdpServer::start(
    HttpSdpServerConfig::new("0.0.0.0:8080".parse()?),
    vec![HttpSdpStream::new("Line 1", sdp.clone())],
)?;
let fetched = khaen_aoip_aes67::http_sdp::http_get_sdp(
    "http://192.0.2.222:8080/Line%201.sdp",
    std::time::Duration::from_secs(3),
)?;

// 3) mDNS/DNS-SD：通告（真实组播用 MdnsEndpoint::default_v4()；单测可改单播）
let host = DnsName::from_str("tspi-aes67-1.local.")?;
let node = ravenna_node_service(
    "My-Corp devicex 1234-5678", RavennaNodeService::Rtsp, host.clone(), 8854,
    Ipv4Addr::new(192, 168, 63, 222),
)?;
let session = ravenna_session_service(
    "Line 1", host, 8854, Ipv4Addr::new(192, 168, 63, 222),
)?;
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
    println!("{} {}:{} {:?}", instance.instance, instance.host, instance.port, instance.addresses);
}
advertiser.stop(); // 发 goodbye（TTL 0）
# Ok::<(), Box<dyn std::error::Error>>(())
~~~

## 测试

~~~
cargo test -p khaen-aoip-aes67
# lib 84 + tests/loopback 4 + tests/zero_alloc 1 = 89 passed, 0 failed
~~~

P1（task-317）新增 26 条单测（rtsp 11 / http_sdp 5 / mdns 10）：RTSP loopback 全流程
（OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN、单连接多请求、keep-alive）、错误方法 501、
Transport TCP 461、缺 Transport 400、缺 CSeq 400、404 保持连接、并发 4 客户端唯一会话、
会话超时清理、DESCRIBE body 与 sdp_parser 互逆、客户端非 SDP/非 200 明确错误；
HTTP GET/404/405+Allow/505、Content-Length 校验、客户端错误路径、HTTP 与 RTSP 同一 SDP；
mDNS 名字压缩/指针环防护、已知报文向量字段、RAVENNA 记录构造（子类型/缓存冲洗位/空 TXT）、
查询应答、缓存实例组合、TTL 过期、goodbye 宽限、坏报文拒绝、单播通告→浏览→注销全流程。
既有 63 条测试全部保持通过、未放宽。

## 边界（NOT_RUN / EXTENSION / UNVERIFIED）

- **NOT_RUN（P1）**：真实组播网络（224.0.0.251:5353 跨主机/跨交换机）、与第三方 RAVENNA
  控制器/设备（Lawo/Merging/DirectOut 等）或 ffprobe/ffmpeg 的 RTSP+DNS-SD 互通、
  5353 与系统 mDNS 守护共存（Windows 多进程监听、Android 系统占用）、ANNOUNCE 推送、
  板端（TSPI/N150）长稳/资源。离线上限止于本机 loopback（RTSP/HTTP TCP、mDNS 单播注入）。
- **NOT_WIRED**：RTSP SETUP 不分配 RTP/RTCP 端口（控制面；组播回显 SDP 的
  destination/port/ttl，单播回显 client_port + SDP 发送端口）；mDNS 多网卡严格出接口
  （IP_MULTICAST_IF）未接（std UdpSocket 无跨平台 API）。
- **EXTENSION**：单播 SETUP 的 server_port 取值；默认组播 TTL 32；HTTP 取 SDP 的
  路径/语义（RAVENNA 只在 OP §3.4 规定 HTTP 配置服务，未规定 SDP 分发路径）；
  RAVENNA TXT 的具体 key（OP §3.5.1.1 只说 MAY 放节点信息，本实现默认不写）。
- **UNVERIFIED**：HTTP/1.1 报文细节（本机语料 docs/rfc 未含 RFC 7230/2616，只按通用语法
  实现并用 loopback 报文自测）；mDNS/DNS 报文细节已按 scratch/aes67-rtsp/rfc/ 的
  RFC 1035/6762/6763 原文实现（该 RFC 原文是 P1 任务期间取到工作区，非 artifacts 语料）。
- **P1 仍不覆盖**：SIP（AES67 单播连接管理）、NMOS IS-04、RTCP、RAVENNA
  clock-domain/sync-time 的媒体时钟接线（发现层只透传 SDP 文本）。
