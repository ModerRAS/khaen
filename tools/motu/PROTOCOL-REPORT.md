# MOTU CueMix 5 ↔ UltraLite-mk5 控制协议逆向 — 报告 v2（真机已验证）

- 任务：搞清 CueMix 5 如何控制 UltraLite-mk5，判断 Linux 板端能否直接读/写 mixer 路由（尤其关掉 input→output 直通，用于 final insert）。
- 日期：2026-11-15（v1 静态逆向 20:04；v2 真机验证 20:34–20:43，soak 结束后板端空闲）
- 状态：**可行，且已在真机上端到端验证**（只读全量状态 + 写 mixer + 还原 0 差异）
- v1 结论修正：**USB-MIDI SysEx 不是 UltraLite-mk5 的控制通道**（v1 那个结论错），真正的通道是 **CDC-ECM 上的网络控制：UDP/1280 发现广播 + TCP WebSocket 1280**。

---

## 1. 结论（TL;DR）

| 问题 | 结论 | 证据级别 |
| --- | --- | --- |
| Linux 板端能否直接读写 mixer 路由？ | **能**。ws://169.254.111.126:1280/ 讲 CueMix 5 的 datastore 协议，纯 Python 标准库即可 | **确证**（真机 2498 条状态 + 写入回读） |
| 需要 MOTU 驱动/Windows 代理吗？ | **不需要**。板端 stock cdc_ether 网口 + 一个临时链路本地 IPv4 即可 | **确证** |
| 需要 RSA 口令鉴权吗？ | **不需要**（当前设备未设网络口令；未收到 kAuthRequest）。若用户设过口令，客户端已内置 RSA-OAEP 分支 | 确证（本例）/ 实现就绪 |
| 怎么发现设备？ | 设备每 1 s 向 255.255.255.255:1280 广播 JSON 信标，含 ip 与 ipv6；**被动监听即可，不发包** | **确证**（25 s 抓 26 包） |
| 能否关掉 input→output 直通？ | **能**，写 kiMixFader（id 1016）crosspoint=0 即可；真机验证 280 条写后直通全灭、USB playback 保留 | **确证** |
| 怎么写？有什么坑 | 单条写没问题；**一次灌 1937 条会让设备/连接超时** → 必须按 CueMix 的做法批量（≤~1400 B/帧）+ 只写差异 | **确证**（踩坑并修复） |
| 写坏了能还原吗？ | 能：全量快照 state-before.json → 写 → 读回 diff；实测还原后 **0 mismatch** | **确证** |
| 有没有硬件旁路（不经 DSP 的直通）？ | 没有。kFPGAPatch 显示 12 个输出口全部由 DSP/mixer tap 馈送 | **确证** |
| USB-MIDI SysEx 能不能用？ | **不能**（对 mk5）。USB MIDI 口是纯 DIN 桥：host→DIN OUT，DIN IN→host；设备不应答，usbmon 证实无回包 | **确证**（见 §9） |

**给 T1/T4/T5/final insert 的直接结论**：最终 insert 模式可以在板端用脚本一键完成（先快照、再关直通、读回验证、需要时一键还原），不需要在 PC 上开 CueMix 去点。T1 环回测试前应保持现有路由（或按测试需要），只要求"记录 CueMix 路由 + 截图"这一步可以改为**机器可读的 JSON 证据**。

---

## 2. 设备与环境事实（实测）

| 项 | 值 |
| --- | --- |
| USB | 07fd:000c UltraLite-mk5，USB2 HS 480M，serial UL5LFE7E6E |
| 固件 | 2.1.2+3352（datastore kDevVersion） |
| datastore API | version 10（kAPIVersion） |
| 设备 IPv4 | **169.254.111.126**（链路本地/APIPA，来自设备自己的 UDP-1280 信标） |
| 设备 IPv6 | FE80::1:F2FE:7E6E（信标里的 ipv6 字段） |
| 设备侧 MAC | 00:01:f2:fe:7e:6e（datastore kDeviceMAC） |
| 板端接口 | enx0001f2fe7e6f（cdc_ether，host 侧 MAC 00:01:f2:fe:7e:6f，carrier=1，100M half） |
| 板端 ALSA | card4 UltraLitemk5：PCM 22/20ch、rawmidi midiC4D0、control controlC4 |
| 板端网口状态 | 默认**没有任何 IP**（NetworkManager disconnected；DHCP 无服务器）→ 需要临时加一个 169.254/16 地址 |
| 设备采样率 | 空闲时 192000（CamillaDSP 跑起来会被 ALSA 设回 48000；不影响 mixer 矩阵语义） |

---

## 3. 控制通道拓扑（已验证）

    ┌──────────────────────────── 板子 (Linux, RK3588) ───────────────────────────┐
    │                                                                              │
    │  client.py                                                                    │
    │    ├─ 发现：AF_PACKET 被动监听 enx0001f2fe7e6f                                 │
    │    │        ← UDP 169.254.111.126:1280 → 255.255.255.255:1280                │
    │    │          {"uid":"UL5LFE7E6E", ..., "ip":"169.254.111.126",              │
    │    │           "ipv6":"FE80::1:F2FE:7E6E", "version":"2.1.2+3352", ...}      │
    │    ├─ 临时加地址：ip addr add 169.254.111.200/16 dev enx...                   │
    │    └─ TCP WebSocket：ws://169.254.111.126:1280/   ← 二进制帧 = datastore 消息 │
    └──────────────────────────────────────────────────────────────────────────────┘
                                       ▲ USB CDC-ECM（class-compliant，无需 MOTU 驱动）
                                       ▼
    ┌──────────────────────── MOTU UltraLite-mk5 (固件 2.1.2+3352) ────────────────┐
    │  内嵌 TCP/IP 栈 + WebSocket 控制服务器（UDP/TCP 均 1280），datastore 引擎     │
    │  混音矩阵 14 bus × 32 input，EQ/Gate/Comp，FPGA patch，loopback，reverb ...   │
    └──────────────────────────────────────────────────────────────────────────────┘

Windows 上 CueMix 5 走的是同一条路：手册 p.17 明说 CDC-ECM 网口 "currently used by CueMix 5 … on Windows and iOS"；Windows 上多一层 MOTUGen5WebSocketProxy.exe（监听 0.0.0.0:1281）只是把本机设备共享给局域网里其它 CueMix 实例（即 JS 里 wsIPAddr=127.0.0.1:1281 的代理路径与 Gen5Proxy- 广播）。板上**不需要**这一层，直连设备 1280 即可。

---

## 4. 发现协议（确证）

- 设备在 CDC-ECM 链路上自配 **169.254.111.126/16**（APIPA 链路本地），并每 **1 s** 广播一次：

      UDP src=169.254.111.126:1280 dst=255.255.255.255:1280
      {"uid":"UL5LFE7E6E","name":"UltraLite-mk5","ip":"169.254.111.126",
       "ipv6":"FE80::1:F2FE:7E6E","model":"UltraLite-mk5","version":"2.1.2+3352","interval":1}

- 板端**不需要联网、不需要 DHCP、不需要扫网段**：被动监听 1 个广播即可拿到 IP（探测脚本就是这么做的，零发包）。
- 设备 IP 可能随设备/网络栈重启变化（APIPA 通常与 MAC 相关、相对稳定），**每次运行都应重新发现**，不要硬编码。若信标丢失，备选：前面板 Display → About 显示设备 IP（手册 p.30）。

---

## 5. 消息层（datastore 协议，确证）

与 v1 从 JS 还原的完全一致，已被真机 2498 条消息验证：

    device -> host :  id:u16  index:u16  value              (长度由类型决定)
    host -> device :  id:u16  index:u16  len:u16  value     (大端)
    类型: B=u8  I=s32  I16=u16  F=8.24 定点(s32/2^24)  S=32B NUL 填充  A=字节数组
    特殊 id: 6000=meters, 0xFFFF=proxy status, 0xFFFE=proxy 消息(口令/公钥)

- **批量写**：CueMix 把多条设备消息拼进**一个** WebSocket 帧，单帧 ≤1452 B（datastore.js:1143）。我们的客户端同样按 ≤1400 B 分帧（client.py: chunk_messages）。
- **读**：连上即收全量状态（2498 条 ≈ 19 KB JSON 快照），无需请求/轮询。
- **鉴权**：本设备未启用网络口令，未发 kAuthRequest；authenticated=True 由"收到第一条非 proxy-status 消息"判定。若启用口令：设备先发 0xFFFE/6（RSA 公钥 PEM）、0xFFFE/2（口令开关）、0xFFFE/0（10 字节序列号），客户端回 0xFFFE/1 = RSA-OAEP(SHA-1)(sha256_hex(password))。client.py 已实现（纯 Python，无第三方库），但**本次未实测**（无口令设备）。
- **未知 id**：设备还发了 17 个 JS 表里没有的 id（1、5、9、12、13、14、17、18、22、23、24、25、5007、5008、5009、5013、16000），多为网络/IP/时钟状态，不影响 mixer；client.py 会跳过并计数。

---

## 6. 混音矩阵语义（确证）

- 矩阵是**扁平数组**：index = bus * 32 + input，共 14 bus × 32 input = 448 crosspoint。
- 相关属性（www/dev.js → DeviceProps_ul.defaultDatastore）：

| key | id | 类型 | 含义 |
| --- | --- | --- | --- |
| kiMixFader | 1016 | F(8.24) | crosspoint 增益（**直通开关就是它：0 = CueMix 里 IM Off**，见 home.js:1012） |
| kiMixMute | 1019 | B | crosspoint mute |
| kiMixSolo | 1018 | B | solo |
| kiMixPan | 1017 | F | pan（0.5 = 中） |
| kiMixStereo | 1000 | B | 输入立体声链接（32） |
| koBusFader / koBusMute | 1027 / 1028 | F / B | 14 条总线的输出增益/静音 |
| kHostOutMixMode | 1020 | B | 0=basic / 1=extended（USB playback 是否进所有 bus） |
| kFPGAPatch | 5010 | B | FPGA 路由表（**硬件旁路检查**） |

- **bus 索引（矩阵输出行）** = mixOutputs.*.iBus：0/1=Main 1/2，2..9=Line Out 3..10，10/11=Phones 1/2，12/13=Reverb 1/2。
- **input 索引（矩阵输入列）**：0/1=Mic/Line 1/2，2..7=Line 3..8，8/9=S/PDIF 1/2，10..17=Optical 1..8，18/19=USB playback Main L/R，20..27=USB playback Line 3..10，28/29=USB playback Phones L/R，30/31=Reverb return。
- **关直通 = 把物理输入列的 crosspoint 全写 0**（kiMixFader），保留 18..29（USB playback）。这同时覆盖 CueMix 的 "IM Off" 语义；也可加 kiMixMute=1（--hard-mute，更保险但会改 CueMix 里看得见的状态）。
- **硬件旁路检查**：kFPGAPatch 的 12 个输出口（Line/Main/Phones）全部指向 0x2E..0x41 的 **DSP/mixer tap**，没有指向裸物理输入。所以关掉 mixer crosspoint 就真的静音，不存在"FPGA 里还有一条直通"。

---

## 7. 真机实测记录（2026-11-15 20:34–20:43）

### 7.1 只读全量读取（确证）

    ws://169.254.111.126:1280/  →  2498 条 datastore 消息, authenticated=True (无口令)
    kModelID=UL5L  kDevName=UltraLite-mk5  kDevVersion=2.1.2+3352  kAPIVersion=10
    kDevUID=UL5LFE7E6E  kDeviceMAC=00:01:f2:fe:7e:6e  kSampleRate=48000/192000
    mixer: 448 faders 收到, 20 个非零 crosspoint

实测直通路由（= 用户说的 in7→out7 / in8→out8 那类东西）：

    Main 1   <- Mic/Line 1(1.000), Mic/Line 2(1.000), Optical 1(0.935)
    Line 3   <- Line 3(0.973)
    Line 5   <- Line 5(0.954)
    Line 7   <- Line 7(0.935)
    （其余物理输入无直通；USB playback 18..29 全部 1:1 到对应输出）

kFPGAPatch：12/12 输出口都是 DSP tap → **无硬件旁路**。

### 7.2 写路径（确证，含踩坑）

1. 快照 state-before.json（448 faders、70 keys）。
2. --mute-inputs --yes：280 条 kiMixFader=0（物理输入 0–17、30、31 × 14 bus）。
3. 读回：**"none: NO physical input reaches any output bus"**；USB playback→输出的 1:1 路由保留（Main1←USB Main L、Line3←USB Line 3 … Phones←USB Phones L/R）。
4. 还原：首次用"全量写 1937 条"的方式还原时**超时断连**（设备/WS 层受不了这种突发）。改成"只写差异 + 批量帧"后，实际只需 **6 条**（原来非零的 6 个 crosspoint），还原后 **0 mismatch**。

结论：**写路径可用；但必须 (a) 批量 ≤1400 B/帧、(b) 只写差异、(c) 写后读回校验**。client.py 已按这三条实现。

### 7.3 设备状态还原证据

before.json 与 final.json 的 mixer 相关键（kiMix* / koBus* / kFPGAPatch / kLoopback* / kHostOutMixMode）逐值比较：**0 mismatch**；非零 fader 数都是 20。

---

## 8. 板端操作手册（已验证的脚本）

脚本：probe_plan.sh + client.py（纯 bash/python，无第三方库）。

    # 上传（在 PC 上，用项目自带的 board.py）
    python board.py put --local client.py      --remote /tmp/cuemix-probe/client.py
    python board.py put --local probe_plan.sh  --remote /tmp/cuemix-probe/probe_plan.sh

    # 只读：发现 + 全量状态 + 矩阵/直通/FPGA 报告
    #（默认临时加 host 169.254.111.200/16，退出自动删）
    python board.py run --cmd "/tmp/cuemix-probe/probe_plan.sh --read 15"

    # 写测试：快照 → 关直通 → 读回验证 → 还原 → 读回验证（REVERSIBLE）
    python board.py run --cmd "/tmp/cuemix-probe/probe_plan.sh --with-write --yes --read 15"

    # 任意时刻一键还原（用某个 run 目录里的 state-before.json）
    python board.py run --cmd "/tmp/cuemix-probe/probe_plan.sh --rollback --from /tmp/cuemix-probe-YYYYmmdd-HHMMSS"

安全设计：
- **硬化门禁**：tspi-soak12.service active 时直接 exit 3（拒绝运行）。
- 默认只读；写必须同时 --with-write --yes。
- 临时地址在 trap 里删除；脚本中断会先尝试还原 mixer 快照。
- 不动 NetworkManager 配置、不动 DHCP、不动 USB、不重启服务、不扫网段。
- 全部证据落 --out 目录并生成 SHA256SUMS，含 state-before.json（还原源）。

client.py 关键参数：

    --discover / --discover-iface    被动发现设备（UDP 1280）
    --transport ws --host <ip> --port 1280
    --read N --json FILE --dump-matrix --dump-direct --dump-fpga
    --mute-inputs [--hard-mute] --yes      关物理输入直通
    --restore FILE --yes                   只写与当前不同的值（批量）
    --apply ops.json --yes                 写任意 [key,index,value] 列表
    --selftest                             离线编解码自检

---

## 9. 为什么 v1 的 USB-MIDI SysEx 结论是错的（更正）

v1 只读 JS 就断定"控制走 USB-MIDI SysEx"。真机否定了它，证据：

1. **USB MIDI 描述符是纯 DIN 桥**：接口 4（MIDI Streaming）只有 1 个 embedded IN jack(1)、1 个 external IN jack(2, "Main Out 2")、1 个从 jack 2 取源的 embedded OUT jack(3)、1 个从 jack 1 取源的 external OUT jack(4, "Main Out 1")。即 **host MIDI → 设备 MIDI OUT 插座；设备 MIDI IN 插座 → host**，控制 CPU 不在这个环里。
2. **探测无任何回包**：F0 00 00 3B 00 01 01 F7（协议号 0x0001）以及 enable/0x0000/0x0002 变体、amidi/rawmidi 两种方式、单/多 fd、重复发——设备 Rx bytes 始终 0。
3. **usbmon 实证**：probe 确实从 host 发出（Bo:1:005:8 ... 04f00000 043b0001 0601f700），而 MIDI IN 端点 0x89 的所有 URB 都以 -2 0（无数据）完成——**设备一个字节都没回**。
4. 同期同一台设备在 CDC-ECM 上每 1 s 广播、TCP 1280 秒连、WS 一接就吐 2498 条状态——控制通道根本不在这条路上。

> 保留 --sysex-legacy 仅为其它型号/历史对照，**mk5 不要用**。

---

## 10. 风险与限制

| 风险 | 说明 | 缓解 |
| --- | --- | --- |
| 设备 IP 变化 | APIPA 地址可能随重启变化 | 每次先被动发现；脚本不硬编码 |
| 需要临时加 host 地址 | 板上 CDC-ECM 默认无 IP | 脚本 ip addr add + trap 删除；不动 NM 配置 |
| IPv6 链路本地自动出现 | ip addr add 后内核可能自动加 fe80 | 脚本记录 pre-state，退出时清理（若原本没有） |
| 写突发导致设备/连接超时 | 实测 1937 条全量写会断 | 批量 ≤1400 B/帧 + 只写差异（已实现） |
| 写错路由 | 会改变设备当前监听路由 | 写前强制快照；写后读回验证；--rollback 一键还原 |
| 设备是否掉电保存 mixer 状态 | 未验证（推测设备保存当前配置） | 还原到原状态即可，不依赖掉电行为 |
| 网络口令（若用户设置） | 会要求 RSA 口令 | client 已实现 RSA-OAEP 分支；本次设备无口令 |
| 多主机同时控制 | CueMix/其它控制端同时写会互相覆盖 | 测试窗口内不要开 PC 的 CueMix |
| 固件版本 | 实测 2.1.2+3352；CueMix JS 对应 datastore v10 | 换固件后先跑 --selftest + 只读 dump 再写 |

---

## 11. 对 T1/T4/T5 与 final insert 的影响

- **T1（out9→in7 环回）**：写/读路由不影响 ALSA 音频；T1 只需记录"测试时 CueMix 路由"——现在可以直接用 probe_plan.sh 产出的 state-before.json 作为机器可读证据，替代/补充 CueMix 截图。
- **T4（5.3.2 十路映射）**：同理，需要时用脚本把物理输入直通关掉再测，测完还原。
- **final insert**：已实测可行。推荐流程：
  1. probe_plan.sh --read 15 取快照（确认 state-before.json 有 448 faders）；
  2. client.py --mute-inputs --yes（或 probe_plan.sh --with-write --yes）；
  3. 读回确认 physical input -> output bus crosspoints: none，并保存 verify-muted.txt + state-muted.json；
  4. 保留 state-before.json 作为 --rollback 源。
- **注意**：final insert 会关掉所有物理输入监听（包括 mic/line）。之后若还要做 T1 环回（MOTU 输入），要先还原。

---

## 12. 兜底方案（若某天网络路径不可用）

1. PC 上 CueMix 5 手动操作（Home/Output 页把 input monitoring 关掉）并截图，作为人工证据；本报告 §6 的矩阵索引可用来对照。
2. 若 TCP 1280 不通，依次检查：设备 USB 枚举、enx* carrier、设备信标（UDP 1280）、host 是否在 169.254/16。
3. 若口令被启用且忘记：CueMix 的 Reset Password 流程（proxy 消息 id 5）需要本机代理；板上直连设备时若设备要求口令而不知道，只能回 PC 用 CueMix 重置。

---

## 13. 交付物与复现

| 文件 | 说明 |
| --- | --- |
| client.py | 控制协议客户端：ws（主）/sysex（legacy）双传输；只读默认；写需 --yes；--discover、--dump-matrix/--dump-direct/--dump-fpga、--mute-inputs、--save/--restore/--apply、--selftest |
| probe_plan.sh | 板端一键探测（发现→临时地址→只读 dump→可选写测试→还原→证据包+sha256+一键回滚），soak 门禁 |
| protocol.json | UltraLite-mk5 全量 87 个属性（id/类型/可写/长度/默认值）+ 总线/输入元数据（从 www/dev.js 求值导出） |
| verify_client.py | 假设备端到端测试（解码/矩阵/写/FPGA 断言），ALL CHECKS PASSED |
| board-evidence2/ | 真机证据：diag2-usbmidi-net.txt、diag3-beacon.txt、diag5-ws-read.txt、diag6-write-test.txt、diag7-restore.txt、before.json/muted.json/final.json、run2-*（probe_plan v2 只读跑） |
| src/ | CueMix 5 的 JS 源码副本（只读） |
| strings/ | MOTUGen5WebSocketProxy.exe / mseries.node strings 取证 |

复现（PC 上）：

    cd C:\WorkSpace\Claw\scratch\lead\cuemix-proto
    python client.py --selftest
    python verify_client.py
    cd C:\WorkSpace\Claw\projects\tspi-audio-dsp\tools\board
    python board.py put --local ...\client.py --remote /tmp/cuemix-probe/client.py
    python board.py put --local ...\probe_plan.sh --remote /tmp/cuemix-probe/probe_plan.sh
    python board.py run --cmd "/tmp/cuemix-probe/probe_plan.sh --read 15"

---

## 14. 时间线

| 时间 | 事件 |
| --- | --- |
| 20:04–20:30 | v1：静态逆向 JS/native，得出 sysex 假设（当时未上真机，标注"待验证"） |
| 20:31 | lead 反馈：真机 sysex 探测 10 s 零响应 |
| 20:34 | diag_sysex.sh：rawmidi/amidi/usbmon 全试 → 设备 MIDI IN 端点 0 字节回包；确认 sysex 死路 |
| 20:35 | diag2.sh：被动 L2 抓包发现设备 UDP-1280 信标（169.254.111.126）；usbmon 证实 probe 从 EP8 发出、EP9 无回包 |
| 20:36 | diag3.sh：拿到完整信标 JSON（uid/ip/ipv6/model/version/interval） |
| 20:37 | diag4.sh：临时加 host 169.254.111.200/16 → **TCP 169.254.111.126:1280 秒连**（1281 拒绝） |
| 20:38 | diag5.sh：WS 只读 → **2498 条 datastore 消息**、448 faders、矩阵/直通/FPGA 全出 |
| 20:39 | diag6.sh：写测试（280 条关直通）→ 读回确认直通全灭；全量还原超时（踩坑） |
| 20:40 | diag7.sh：差异还原 6 条 → 读回 **0 mismatch**，设备恢复原状 |
| 20:41–20:43 | client 增加 --discover/--apply/批量帧/差异还原；probe_plan.sh v2 重写并在板上只读跑通 |

---

## 15. 附录：属性 id 速查（UltraLite-mk5，protocol.json 有全量）

    0 kDeviceMAC A   2 kDevUID A   3 kDevName S   4 kDevVersion S
    6 kiChannelNames S  7 koChannelNames S  8 kAPIVersion I16  10 kSampleRate I
    11 kClockSource B  15/16 PLL/FPGA lock B  19 kMIDIThru B  20 kLoopbackFirst B
    21 kModelID S  26 kNetControlEnable B
    1000 kiMixStereo B  1001 koMixStereo B  1016 kiMixFader F  1017 kiMixPan F
    1018 kiMixSolo B    1019 kiMixMute B    1020 kHostOutMixMode B
    1027 koBusFader F   1028 koBusMute B
    5006 kOpticalMode B 5010 kFPGAPatch B  5014 kLoopbackSource B
    5000 koTrim B 5001 kiGain B 5002 kiPhase B 5003 kPad B 5004 k48V B
    （EQ/Gate/Comp 见 1002–1015、1021–1026；完整表见 protocol.json）

矩阵索引：index = bus*32 + input；bus：0/1 Main、2–9 Line Out 3–10、10/11 Phones、12/13 Reverb；
input：0/1 Mic、2–7 Line 3–8、8/9 S/PDIF、10–17 Optical、18/19 USB Main、20–27 USB Line、28/29 USB Phones、30/31 Reverb return。
