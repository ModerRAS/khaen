# aes67-rx —— dsp-host AES67 接收端运行二进制

> 证据任务：`evidence/calib-chain-20261121/aes67-rx-20261121/`（2026-11-21）
> 定位：**独立二进制**，不替换/不改 `/opt/tspi-rt-stack` 生产二进制；MOTU 独占由测试窗口保证。

## 链路

```text
sender-host NSP (4.1.2, L24/48k, 7ch, 1ms) --RTP/UDP 单播 192.0.2.222:5004-->
  crates/aoip-aes67（SDP 校验 / RTP L24 BE 解包 / 无锁抖动环 / 128 帧块）
  --> 本二进制播放线程（媒体时基 = origin + latency + 已写出帧数）
  --> ALSA hw:UltraLitemk5,0 / S24_3LE / 48k / 22ch
  --> ch0,1,2,3,4,5,7 = L,R,Ls,Rs,Ltf,Rtf,LFE；其余 15 通道静音
  --> MOTU UltraLite-mk5 物理输出 → 喇叭
```

## 用法

```bash
# 1) 解析/校验 SDP
aes67-rx sdp-info --sdp aes67.sdp

# 2) 接收真实流（先 systemctl stop tspi-rt-host 独占 MOTU）
aes67-rx run --sdp aes67.sdp --duration-secs 20 --mlock     --report /root/aes67-rx/evidence/run.json

# 3) 本机自测（自带 RTP 发送端，1 kHz/通道，写 MOTU）
aes67-rx selftest --duration-secs 10 --mlock     --report /root/aes67-rx/evidence/selftest.json

# 4) 独立测试音发送端（跨机联调）
aes67-rx tone --dest 192.0.2.222:5004 --channels 7 --tone-hz 1000 --duration-secs 10
```

## 设计要点（RT）

| 项 | 做法 |
|---|---|
| 线程 | 收包线程（crate 内 `aes67-rx-io`，SCHED_FIFO `--rx-prio`）+ 播放线程（`aes67-rx-playback`，SCHED_FIFO `--playback-prio`，默认绑 CPU2） |
| 环 | crate 的无锁 SPSC 抖动环（绝对媒体帧号 + valid bitset），ISR/收包线程不被播放线程阻塞 |
| 预滚 | 首包锚定后等 **24 ms（1152 帧 = 24 个 1 ms 包）** 再起播；预滚期间只写静音、不消费抖动环，确保第一包音频从 media frame 0 开始；起播 **10 ms 余弦淡入** |
| 零分配 | 块缓冲/输出字节缓冲/淡入淡出状态全部启动期预分配；热路径无 `format!`/无锁/无系统调用（除 ALSA writei） |
| 防爆音 | 停流/欠载：**40 ms 余弦淡出**（跨多块状态机）后才静音，且继续按 2.667 ms 节拍写静音块（不 xrun/不咔哒）；恢复：10 ms 余弦淡入。`--no-click-guard` 可关，`--fade-in-ms/--fade-out-ms` 可调 |
| 欠载策略 | 静音块照常推进播放游标（crate 语义），ALSA EPIPE 后 `prepare` 重开并计数，不 panic |
| 时基 | 无 PTP（localmac/到达时间）：媒体目标帧 = 已写出的 48k 帧数，播放节奏由 ALSA 阻塞 writei 提供（设备时钟）；抖动缓冲用收包线程 `UsrvClock` 锚定首包 |
| 统计 | 每秒状态行（主线程）+ 结束 JSON（收包/抖动/ALSA/RT/映射），JSON 内嵌 crate `report_json` |
| dump（仅诊断） | **默认关闭**；`--dump-dir DIR`（或兼容 `--dump-wav FILE` → FILE.d）才落盘 22ch PCM。**有界环形**：默认 5 s/段 × 3 段、总时长 ≤30 s、总字节 ≤50 MB（先到为准），旧段自动截断覆盖；启动检查所在 fs 剩余 <1 GB 则拒绝启用；**写失败只停 dump+计数，不影响播放**；启动时清空自己的 dump 目录。常规运行不落盘音频。 |

## 现场建议参数（2026-11-21 NSP HT 实测）

```bash
aes67-rx run --sdp aes67.sdp --gain-db -6 --latency-us 60000 --mlock
```

- NSP 侧存在 **burst 重排**（单次跨度最大 ~44 ms，pcap 证明 loss=0%），24 ms 抖动缓冲会把迟到包当
  late 丢弃 → 出现 ~33 ms 缺口；**60 ms 缓冲可完全吸收**（62 s 实测 under/late/over=0、缺口 0）。
- 10 s 周期卡顿的根因是 `jitter.rs` 旧判据 `|ts-ts0|>10s` 把"持续时长"误判为跳变；已修（相对上一包）。

## 边界（NOT_RUN / 已知限制）

- 无 PTP/PHC：`ts-refclk:localmac` 仅占位，媒体时基是到达时间 + 设备时钟，不是采样级 PTP 对齐。
- 无 ASRC：发送端 48k 与 MOTU 设备 48k 的 ppm 差会让抖动缓冲水位缓慢漂移（8192 帧环 ≈170 ms 余量，长时间运行需观察）。
- 未做 8h soak / 丢包注入 / 组播真实交换机（组播代码路径支持但本任务主用单播）。
