# aes67-sender — Windows 用户态 AES67 发送端（Rust）

> team task-236 · agent `rt-aes67tx` · 2026-11-18/19
> 目的：让 PC（Windows）直接当 AES67 发送端，方便测试；**不写 ASIO 驱动、不需要虚拟声卡驱动**。
> 第一阶段**没有 PTP/PHC**：RTP 时间戳来自本机单调时钟（QPC），**不是严格 AES67 PTP 同步**，
> 只用于链路/内容验证。详见 `rt-stack/evidence/aes67-tx-win/CLOCK-AND-SYNC.md`。

## 能力（全部 Rust，无 Python）

| 输入模式 | 说明 |
| --- | --- |
| `--source wasapi-loopback` | **优先**：WASAPI **系统 loopback**，抓任意应用播到某个 render endpoint 的声音（按 endpoint 选择；默认 console 端点） |
| `--source wasapi-process` | 可选：WASAPI **process loopback**（Win10 2004+），只抓指定 PID 的进程树（`--pid`），不占系统级 |
| `--source wav` | WAV 文件（PCM 8/16/24/32、f32/f64、任意声道 1..8；非 48k 自动线性重采样，prototype 质量） |
| `--source raw` | 原始交错 PCM（48k；`--raw-format l16/l24/l32/f32`，文件为 little-endian） |
| `--source sine` | 合成测试音（L/R 可分别设频率，默认 1 kHz / 2 kHz） |

输出：AES67 风格 RTP，L24（24-bit 大端），48 kHz，ptime 可配（1 ms 默认 = 48 帧/包），
单播或组播（`239.69.x.x`），可指定网卡（`--iface-ip`）、TTL、multicast loop；
SDP 写文件/打印（`a=ts-refclk:local`，`a=mediaclk:direct=0`）。

本 crate 是**独立 Cargo workspace**（`Cargo.toml` 末尾空 `[workspace]`），
不注册进 `rt-stack/Cargo.toml`（那是 integrator 独占写域）。
SDP/RTP 类型暂时自带，**TODO(task-236/shared)**：等 `rt-aes67rx` 的 `crates/aoip-aes67` 冻结公共 API 后统一。

## 快速开始

```powershell
cd rt-stack/tools/aes67-sender
cargo build --release --offline

# 列出可被 loopback 抓的 render endpoint（本机默认是 VB-Audio CABLE Input）
.\target\release\aes67-devices.exe --formats

# 合成音 -> 单播（本机 loopback 自测）
.\target\release\aes67-send.exe --source sine --duration 3 --dest 127.0.0.1 --port 5510 \
    --sdp-out unicast.sdp --stats-json tx.json
.\target\release\aes67-recv.exe --port 5510 --duration 5 --silence-ms 1000 --wav-out rx.wav \
    --stats-json rx.json --expect-tone 1000 --expect-tone 2000

# WASAPI 系统 loopback：抓 "CABLE Input" endpoint 上任意应用的播放
.\target\release\aes67-send.exe --source wasapi-loopback --wasapi-device "#4" --duration 5 \
    --dest 239.69.63.10 --iface-ip 192.0.2.94 --sdp-out mcast.sdp

# WASAPI 进程 loopback（只抓指定 PID）
.\target\release\aes67-send.exe --source wasapi-process --pid 1234 --duration 5 \
    --dest 127.0.0.1 --port 5513

# 用虚拟声卡做“不出声”的渲染端自测（本仓证据就是这么做的）
.\target\release\aes67-gen-wav.exe --out tone.wav --duration 3 --bits 24 --freq-l 1000 --freq-r 2000
.\target\release\aes67-play.exe --wav tone.wav --device "#4" --duration 6
```

**去板端联调前**：把 `--iface-ip` 指到连板子的网卡（如 192.0.2.94），`--dest` 用板端可加入的组播地址/端口（默认 `239.69.63.10:5004`），
把 SDP 交给板端接收端（板端 daemon 或 `crates/aoip-aes67` 接收例程）。**白天静音窗口 + 用户窗口**见 `NOT_RUN.md`。

## 构建 / 测试

```powershell
cargo build --release --offline      # 产物：target/release/aes67-*.exe
cargo test                           # 35 unit + 4 integration（含真实 UDP loopback）
cargo fmt --check
```

Toolchain（本机实测）：rustc 1.99.1 / cargo 1.99.1 / 默认 `x86_64-pc-windows-gnu`；
Windows 11 25H2 build 26200（process loopback 支持）。依赖仅 `anyhow`、`socket2`、Windows 平台 `windows 0.52`，全部离线可构建。

## 时钟与实时节奏（重要）

- **没有 PTP/PHC**：RTP 时间戳由 QPC 派生，起点 0，每包递增“帧数”（1 ms ptime → +48）；序号从 0 起递增。
- SDP 写 `a=ts-refclk:local` + `a=mediaclk:direct=0`，**明确不是 IEEE1588 时钟参考**。板端接收必须用抖动缓冲/异步重采样对齐，不能假设 PTP 同步。
- 发送节奏：QPC 绝对 deadline + **高精度 waitable timer**（`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`）+ 最后 ~150 µs spin；
  `timeBeginPeriod(1)`、发送线程 `THREAD_PRIORITY_HIGHEST`；预热 50 ms（可配 `--warmup-ms`），WASAPI 首次无数据时最多等 `--warmup-timeout-ms` 后以静音保持节奏。
- 实测 deadline 迟到（本机 1 ms ptime，3 s 窗口，机器空载时）：**p50 ≈ 0.19–0.30 ms，p99 ≈ 0.65–0.94 ms，max ≈ 1.1–1.8 ms**；负载重时会到 10–30 ms（普通非实时 Windows 进程特性，见证据里的对比记录）。
- 欠载统计：`--stats-json` 输出 `underrun_packets/frames`、`deadline.*`；WASAPI 采集侧另有 `wasapi_capture`（discontinuity/silent/overflow_drops/read_errors）。

## ASIO 说明（按 Lead/用户口径）

**不实现 ASIO 驱动**（需要 Steinberg SDK + 注册 DLL，且单客户端，不适合系统级抓音）。
ASIO-only 应用请通过 **FlexASIO / ASIO4ALL** 输出到某个真实 WASAPI 端点（如 HDMI），
再由本工具的 **WASAPI 系统 loopback** 抓取——仍然不需要自定义驱动。详见
`rt-stack/evidence/aes67-tx-win/ASIO-AND-VIRTUAL-ENDPOINTS.md`。

## SDP 模板

```
v=0
o=- 23735502336229376 1 IN IP4 <origin-ip>
s=TSPI Windows AES67 sender
c=IN IP4 <dest-ip>/32          # 组播时带 /32；单播不带
t=0 0
m=audio <port> RTP/AVP 97
a=rtpmap:97 L24/48000/<channels>
a=ptime:<ms>
a=ts-refclk:local
a=mediaclk:direct=0
a=ssrc:<ssrc> cname:tspi-aes67-sender@<origin-ip>
```

## 组成

| 文件 | 内容 |
| --- | --- |
| `src/clock.rs` | QPC 单调时钟、高精度 waitable timer、deadline 统计、线程优先级 |
| `src/pacer.rs` | 实时发送循环、RTP 打包、UDP/组播 socket、欠载/迟到统计 |
| `src/rtp.rs` / `src/pcm.rs` | RTP 头、L24 打包/解包、f32↔整型 |
| `src/sdp.rs` | SDP 生成（AES67 风格 + `ts-refclk:local`） |
| `src/wav.rs` / `src/resample.rs` | WAV 读写、48k 线性重采样（prototype 质量） |
| `src/source.rs` | sine / WAV / raw 输入 |
| `src/wasapi.rs` | WASAPI 系统 loopback、process loopback、render/playback 辅助、endpoint 枚举 |
| `src/bin/aes67-send.rs` | 发送端 CLI |
| `src/bin/aes67-recv.rs` | 最小 Rust 接收端（自检用：RTP/L24 解码、WAV 落盘、音调/帧/序号/时间戳断言） |
| `src/bin/aes67-gen-wav.rs` | 生成测试 WAV/raw |
| `src/bin/aes67-play.rs` | 渲染 WAV/测试音到指定 WASAPI render endpoint（用于跨进程 loopback 证据） |
| `src/bin/aes67-devices.rs` | 列 render endpoints 与 mix format |
| `tests/loopback.rs` | 真实 UDP sender→receiver 集成测试 |
| `scripts/run_evidence.ps1` | 一键重跑证据（build/test + 5 组 loopback 场景） |

## 已知限制 / 未做

- 严格 AES67 时钟：本阶段用本机单调时钟，**非 PTP**；板端 PTP 联调 NOT_RUN。
- SAP 公告、RTSP 会话、SRT/IGMP query 等未实现（SDP 由文件/手工交付）。
- WAV/raw 多声道代码支持 1..8 ch，但证据只跑了 2 ch；WASAPI 源固定输出 2 ch（endpoint >2ch 时取前两路）。
- 非 48k 输入/采集用线性插值（prototype），未做带限 SRC/异步重采样（接收端负责）。
- process loopback 需要 Win10 2004+；本机 Win11 25H2 验证通过；旧系统会报错退出。
- stats JSON 是手写序列化（无 serde 依赖），字段见 `src/bin/*.rs`。

## 证据

`rt-stack/evidence/aes67-tx-win/`：Windows 本机 sender→Rust receiver 五组 loopback 全 PASS，
含 SDP、收发 stats JSON、接收 WAV、构建/测试/哈希；板端联调 NOT_RUN 与未来步骤见 `NOT_RUN.md`。
