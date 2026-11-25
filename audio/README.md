# 泰山派 3M（RK3576）音频 DSP/输出设备 —— 音频软件栈集成方案

> 目标平台：泰山派 3M（立创 LCKFB-TSPI3M-RK3576-4G-64G，RK3576 / 4×A72+4×A53）
> 系统：Debian 12 (bookworm) aarch64 + PREEMPT_RT（kernel 6.1.x，由 firmware-builder 产出）
> 角色（S1 历史）：AoIP 接收 → CamillaDSP DSP → MOTU UltraLite-mk5（USB，**只做输出**）
> **现行口径（2026-11-17 起，覆盖本文）**：CamillaDSP **已退役**——Phase 1 运行镜像不安装/不运行它，仅保留为历史/离线 oracle 参考（G-no-C，见 `docs/CURSOR-TASK-FINAL-AUDIT.md`、`rt-stack/`）；Phase 1 运行时为新自研 RT host/daemon 单宿主链路（`R_pre→G_modal→P_post`）。
> 本文档是音频栈 S1 时代的**历史口径**（组件版本、构建、配置、服务、镜像集成、验收与风险），用于历史审计与迁移参考；**不得据此把 CamillaDSP 当现行运行组件**。改动请同步 overlay-manifest.md。

## 0. 状态与边界

- 本目录只做「音频软件栈 + 镜像集成契约」，不碰 SDK/内核构建（firmware-builder 负责），不在 Windows 上跑交叉编译。
- 构建目标机 = Debian 12 aarch64（推荐在 aarch64 构建机/目标板同版本 rootfs 内构建；CamillaDSP 也支持官方 aarch64 预编译包作为兜底）。
- 一切文件落盘位置：本目录（长期）+ `scratch/lead/tspi-audio-dsp/audio/`（过程材料，可清理）。
- 本文中「待验证」「待确认」标记的内容=尚未在 RK3576 实机验证，不得当作已实现承诺。

## 1. 两种互斥工作模式

| 模式 | AoIP 接收 | PTP | 图例 |
| --- | --- | --- | --- |
| `dante`（模式 A） | Inferno ALSA PCM（`pcm.inferno`） | PTPv1 **follower**（teodly/statime fork），主钟**可由 H16-AIP 担任**（第三方实作文章原文「NuPrime H16-AIP 做 主時鐘」；官方仅称默认 Slave、需至少一台 Master）——设置入口待手册/实机确认 | `H16-AIP（Dante 16ch, 96k） → eth0 → Inferno → CamillaDSP 96k/24（16ch 入） → MOTU（5.3.2 / 10 路）` |
| `aes67`（模式 B1，默认） | aes67-linux-daemon（MergingRavennaALSA LKM） | **PTPv2 slave**（LKM 内置），需网内有 PTPv2 GM（**H16-AIP 默认也是 slave——这是 B1 的前提风险，见 §1.1 / R25**） | `H16-AIP（AES67 16ch, 48k） → eth0 → daemon/LKM → CamillaDSP 48k/24（16ch 入） → MOTU（5.3.2 / 10 路）` |
| `b2`（模式 B2，2026-11-17 实测打通） | Inferno ALSA PCM（不加载 RAVENNA LKM/daemon；媒体时钟=PHC usrvclock 桥） | **PTPv2 grandmaster**（ptp4l masterOnly + `phc2sys -O 37`，网卡 PHC 跑 TAI）；同一时刻**不能**再起 daemon/LKM/statime | `H16-AIP 等 slave 设备锁本机 GM；板端 Inferno（CLOCK_PATH=/tmp/ptp-usrvclock）→ CamillaDSP → MOTU（5.3.2 / 10 路）`。`--ptp-only` 只起 GM 时钟栈 |
| `off` | 全停（维护/安全状态） | 无 | - |

两种模式**互斥**：同一时刻只允许一套 AoIP 与一套 PTP 在跑。原因（全部有源码依据，见 RISKS-AND-DECISIONS.md）：
1. PTPv1 与 PTPv2 是两套协议（报文格式不同），交叉运行会让从时钟选主混乱；
2. Inferno 的 PTPv1 依赖 statime fork（usrvclock 导出），AES67 的 PTPv2 依赖 LKM 内置 slave，两者对 PHC/系统时钟的假设不同；
3. aes67 LKM 会注册 netfilter PRE_ROUTING 钩子拦截 PTP/RTP 包，加载期间会干扰其它 PTP 栈——因此 **LKM 只在模式 B 加载，切走时必须卸载**。

### 1.1 源设备 H16-AIP（2026-11-15 新前提，关键路径）

- **NuPrime H16-AIP**：HDMI → AES67/Dante **16ch 发送端**；**AES67 模式固定 48 kHz**、**Dante 模式 96 kHz**（AES67 96k 不可用）。
- **H16 默认是 PTP Slave**：官方产品页要求“网内至少一台 AoIP 设备设为 Master”；社区实测部分配置可切 clock leader（AES67 有成功案例），Dante 侧实测可做 clock leader。
- 对两种模式的影响：
  1. **AES67（B1）**：daemon 是 slave，需要网内已有 PTPv2 GM。若 H16 保持默认 slave 且无其他 GM → **B1 无法 lock，AES67 模式不出音**；此时只能：H16 切 leader / 加外部 GM / 走 B2（box 当 GM + 换接收端）——**等用户确认 H16 时钟角色**（RISKS R25/A19）。
     2026-11-17 进展：B2 链已在板端实测打通（GM=MASTER、VM slave offset −126 µs、PHC→usrvclock、VM TX→板端 capture 15 s 无断流），并已进 `tspi-mode b2`；H16 实机互操作与 24 h soak 仍 NOT_RUN（`rt-stack/evidence/b2-ptp-gm/REPORT.md`、`rt-stack/evidence/b2-mode/REPORT.md`）。
  2. **Dante**：H16 可当 Dante clock leader，box 的 Inferno 做 PTPv1 follower → D2 默认路径成立（待实测，A3/A20）。
  3. **输入是 16ch 多声道容器（当前 7.1.4/9.1.4）**，不是立体声；CamillaDSP `capture.channels=16`，通道顺序 TBD（A18），5.3.2 输出 10 路见 `configs/layout-5.3.2.md`。

## 2. 组件矩阵

版本均为 2026-11-15 核对到的上游最新稳定/开发快照（滚动更新时以 overlay-manifest.md 为准）。

| # | 组件 | 版本 / 来源 | 构建方式 | 依赖（构建/运行） | 安装路径 | 服务 | 配置 | 默认状态 | 风险 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | CamillaDSP（**已退役**；S1 历史） | **v4.1.3**（2026-04-09，HEnquist/camilladsp tag v4.1.3；本地校验源码 projects/camilladsp） | 官方 aarch64 资产 `camilladsp-linux-aarch64.tar.gz`（2,632,436 B）或 `cargo build --release`（rustc ≥1.90，edition 2024） | 构建 libasound2-dev；运行 libasound2、libc6 | `/usr/local/bin/camilladsp` | `tspi-camilladsp` | `/etc/camilladsp/dante.yml`、`aes67.yml` → 渲染到 `/run/tspi/camilladsp.yml`；**输出 5.3.2/10 路（22ch 补零）** | S1 历史：安装、不 enable 自启、由 tspi-mode 拉起；**现行已退役（Phase 1 不安装/不运行，仅历史/离线 oracle）** | 10 路 × FIR @96k 需实测（历史） |
| 2 | Inferno（Dante/AoIP） | dev `9767558`（2026-09-08） | `cargo build --release --locked`（workspace，含 3 子模块） | 构建 libasound2-dev；运行 libasound2 | `/usr/local/bin/inferno2pipe`（可选）+ `/usr/lib/aarch64-linux-gnu/alsa-lib/libasound_module_pcm_inferno.so` | 无常驻服务：由使用 ALSA `pcm.inferno` 的进程（CamillaDSP）加载 | `/etc/asound.conf` 定义 `pcm.inferno`；参数走 `INFERNO_*` 环境变量（`/run tspi/inferno.env`） | 安装；仅模式 A 使用 | alpha 质量；PTPv1 只有 follower |
| 3 | Statime（Inferno 定制 fork） | branch `inferno-dev` `244f20a`（2025-07-18，teodly/statime） | `cargo build --release`（MSRV 1.67） | 无动态库；运行 root + CAP_SYS_TIME/CAP_NET_BIND_SERVICE | `/usr/local/bin/statime` | `tspi-statime-inferno`（模式 A） | `/etc/statime/inferno-ptpv1.toml`；观测 socket `/run/tspi/statime-observe.sock` | 安装、不 enable；模式 A 由脚本拉起 | **只支持 PTPv1 slave**；PTPv1 master 无开源实现 |
| 4 | linuxptp | Debian 12 bookworm **3.1.1-4**（可用 apt 包，或源码同版本） | apt 包安装或 `build-linuxptp.sh` 源码构建 | 无（仅 libc） | `/usr/sbin/ptp4l`、`phc2sys`、`pmc`、`phc_ctl`（包安装） | `tspi-ptp4l-master`、`tspi-phc2sys`、`tspi-phc-usrvclock`（B2，由 `tspi-mode b2` 拉起） | `/etc/ptp4l-master-aes67.conf`；phc2sys `-O 37`（TAI）在 unit 内 | 安装、不 enable | 与 daemon/LKM/statime 同时用无效（见 §4.4；unit 已声明 Conflicts） |
| 5 | AES67 Linux daemon | `master` @ `93314d2`（2026-10-25，bondagit/aes67-linux-daemon） | `build.sh`：cmake C++17 + 内核模块 + WebUI 预编译包 | 构建：g++/cmake/boost/avahi/systemd/内核头；运行：libstdc++6、libboost-*、libavahi-client3、libsystemd0 | `/usr/local/bin/aes67-daemon`；内核模块 `/lib/modules/$(uname -r)/extra/MergingRavennaALSA.ko` | `tspi-aes67-daemon`（模式 B）+ 加载 LKM | `/etc/aes67-daemon/daemon.conf`；REST `127.0.0.1:8080/api/ptp/status`；`ptp_status_script` 写 `/run/tspi/ptp-lock` | 安装、不 enable；模式 B 由脚本拉起 | ARM64 已源码级可行、**未上板验证**；RT 下 hrtimer/netfilter 需实测 |
| 6 | MergingRavennaALSA（LKM） | bondagit/ravenna-alsa-lkm branch `aes67-daemon` @ `8aea781`（driver v2.4，2026-10-25） | 目标内核头下 `make -C /lib/modules/$(uname -r)/build M=$PWD modules` | 内核：NETFILTER、HIGH_RES_TIMERS、NETLINK、（RT 内核） | `/lib/modules/.../extra/MergingRavennaALSA.ko` | 由 `tspi-ravenna-lkm.service`（modprobe）加载 | modprobe 参数 `id=ravenna index=-1` → ALSA `hw:ravenna,0` | 仅模式 B 加载 | 内核 6.1+RT 未实测；`jitter_buffer_multiplier` 默认 3×tic |
| 7 | alsa-ucm-conf（MOTU mk5 UCM） | bookworm 为 **1.2.8-1（无 mk5）**；上游 mk5 UCM 自 **2024-05-27**（commit `2bd2aca`，首个 release v1.2.13） | **回移**：mk5 两个 UCM 文件 + 1.2.8 dispatcher 的 Syntax 6 补丁（ucm-backport-mk5.sh） | alsa-lib 1.2.8（bookworm） | `/usr/share/alsa/ucm2/USB-Audio/MOTU/UltraLite-mk5*.conf` + 打补丁的 `USB-Audio.conf` | 无 | - | 可选、尝试性；失败不影响主链路 | 上游 v1.2.13 起 UCM 语法需 alsa-lib ≥1.2.12（Syntax 7），**不能整树替换** |
| 8 | MOTU UltraLite-mk5 | USB `07fd:000c`，class compliant；**22 playback / 20 capture** 通道（UCM HWChannels 实证）；24-bit，44.1–192 kHz | 不需要驱动（`snd-usb-audio`） | 内核 CONFIG_SND_USB_AUDIO | `/dev/snd/*`；udev 稳定别名 `/dev/snd/motu` | 无 | `/etc/modprobe.d/tspi-audio.conf`（`id=motu`）、`/etc/udev/rules.d/95-motu-ultralite-mk5.rules`、`/etc/asound.conf` | 插入即用 | 96k 下可用通道数/通道顺序以实测为准（A14/A17） |
| 9 | ALSA 工具/库 | bookworm：alsa-lib **1.2.8-1**、alsa-utils 1.2.8-1、alsa-ucm-conf 1.2.8-1 | apt（镜像构建期离线 .deb） | - | 系统路径 | - | - | 必备 | - |

### 2.1 输入 / 输出格式能力（用户口径）

| 环节 | 格式 | 依据 / 结论 |
| --- | --- | --- |
| AES67 接收 | **L16（16-bit）与 L24（24-bit）** 均可；16ch 容器（H16-AIP） | daemon 源码 `session_manager.cpp` 识别 `L16/L24/L2432/AM824`；SDP `rtpmap` 相应变化；H16 送 16ch |
| Dante 接收 | Dante 流以 32-bit 整数进入 Inferno；16-bit 源可用（32-bit 容器），24-bit 原生；H16-AIP 送 16ch@96k | Inferno README「32-bit integers are always used internally；TX_SOURCE_BIT_DEPTH 控制 16/24-bit 输出抖动」；ALSA 插件固定 **S32_LE** |
| DSP 内部 | 64-bit float（CamillaDSP 默认 PrcFmt；可选 32bit 构建） | CamillaDSP 文档 |
| MOTU 输出 | **24-bit**（S24_3_LE，USB 常用 packed 24-bit） | USB class compliant 惯例；实际格式以 aplay --dump-hw-params 为准，S24_3_LE 不可用时回落 S32_LE（低 8 位无效） |
| 输入声道 | **16ch**（H16-AIP，7.1.4/9.1.4 容器；顺序 TBD） | CamillaDSP `capture.channels=16`；映射假设见 `configs/layout-5.3.2.md` §1；A18 上板核对 |
| 输出布局 | **5.3.2 / 10 路**（Main 1-2 + Line 3-10）；USB 22ch 中 10..21 补零 | 用户 2026-11-15 确认；映射与 FIR 预算见 `configs/layout-5.3.2.md` |

采样率（现行硬约束）：AES67 固定 **48 kHz**、Dante **96 kHz**，只允许 **48/96 kHz** 两档、默认 **48 kHz**。处理率跟输入；**禁止 176.4/192 kHz**（产品硬约束；Web `PUT /api/audio/format` 对 192000/176400 返回 400）。下文 S1 历史记录中出现的 192k 仅指当时设备能力/验收项，**已不再是可选运行档、不再支持**。

## 3. 拓扑与信号链

```text
模式 A（Dante，96k/24）：
  [H16-AIP: HDMI -> Dante 16ch @96k] --mcast--> eth0 --> Inferno ALSA plugin (PTPv1 follower, statime; H16 当 Dante clock leader)
                                                     |
                                                     v  S32_LE / 16ch
                                          CamillaDSP (chunksize 可配)
                                                     |  S24_3_LE / 22ch(硬件), 0..9 映射、10..21 补零
                                                     v
                                      MOTU UltraLite-mk5 (USB, 5.3.2 / Main 1-2 + Line 3-10)

模式 B1（AES67，48k/24）：
  [H16-AIP: HDMI -> AES67 L16/L24 16ch @48k] --RTP mcast--> eth0 --> MergingRavennaALSA LKM (PTPv2 slave)
                                                     |  hw:ravenna,0 (S32_LE, period=tic 1ms)
                                                     v
                                          CamillaDSP (chunksize 可配, 16ch 入)
                                                     |
                                                     v  MOTU UltraLite-mk5 (5.3.2 / 10 路)
```

时钟域（两种模式共同点）：**AoIP 时钟（PTP）≠ MOTU USB 时钟**。CamillaDSP 必须用 `enable_rate_adjust: true` + **Async 重采样器**把两者解耦（见 §4.5），否则数分钟后必然漂移到欠载/溢出。

## 3.1 先读：三条最关键的技术结论

1. **AES67 模式下本机不能同时做 PTPv2 GM 与跑 aes67 daemon**（单网口）。daemon 的 LKM 是 PTP **slave-only** 且只在 netfilter `PRE_ROUTING` 收包，本机 ptp4l 的包走 `LOCAL_OUT`，永远收不到、不会 lock；无 lock 时音频回调不出帧（`manager.c:1481`），换采样率会在 4 s 后失败。因此 `aes67` 模式默认按 **B1：daemon 做 slave，外部提供 PTPv2 GM** 交付。**B2（本机 GM + 换接收端）已实现为独立模式 `tspi-mode b2`**（ptp4l GM + phc2sys -O 37 + PHC usrvclock 桥 + Inferno；不启 daemon/LKM），与 B1/dante/off 互斥；B3（双网口 hack）仍是决策点（RISKS-AND-DECISIONS.md D1）。
2. **Dante 模式下本机也不能做 PTPv1 GM**：teodly/statime fork 只实现 PTPv1 slave。模式 A 的 Dante 主钟建议由 **H16-AIP 做 Dante clock leader**（社区实测可行，待确认）；不行则用外部 Dante 主钟（DVS/Via/硬件）。若全网 Dante 设备都开 AES67，可用 statime 的 PTPv2 leader 配置（`configs/variants/inferno-ptpv2-leader.toml`），但那是 PTPv2 路径、不是 PTPv1。
3. **CamillaDSP 的 `chunksize` 不是 ALSA period**（v4.1 buffermanager：buffer≈next_pow2(max(3×chunksize,4×min_period))，period=buffer/8）。因此「32 帧」只压缩 DSP/ring 段；AES67 的 LKM 自身 ALSA period 固定为 tic（默认 48 帧=1 ms @48k），AoIP 抖动缓冲（daemon `jitter_buffer_multiplier`/Dante latency）另算，端到端下限受它们约束。

## 4. 关键事实与配置口径（写实现前必读）

### 4.1 CamillaDSP v4.1.3 配置口径

- 顶层 `devices` 必填：`samplerate`、`chunksize`、`capture`、`playback`；可选项含 `queuelimit`(默认 4)、`enable_rate_adjust`、`target_level`(默认=chunksize)、`adjust_period`(默认 10 s)、`resampler`、`capture_samplerate`、`stop_on_rate_change`、`rate_measure_interval`(默认 1 s)、`volume_ramp_time`(默认 400 ms)、`multithreaded`、`worker_threads`。
- `target_level` 上限（ALSA 播放）：`(4 + queuelimit) × chunksize`（源码 utils.rs 校验）。
- ALSA 设备字段：capture/playback `type: Alsa`、`channels`、`device`、可选 `format`（枚举：S16_LE / S24_3_LE / S24_4_LE / S32_LE / F32_LE / F64_LE）。capture 另支持 `stop_on_inactive`、`link_volume_control`、`link_mute_control`。
- 音量控制、websocket、状态文件：`-p/--port`、`-a/--address`、`-s/--statefile`、`-l/--loglevel`、`-c/--check`（只校验配置）；我们用 `-p 1234 -a 127.0.0.1` 供 web-manager 对接。
- `--check` 可用于镜像内离线验证配置（不需要声卡）。

### 4.2 Inferno（Dante）配置口径

- ALSA 插件只支持 **S32_LE**；`RX_CHANNELS/TX_CHANNELS` 决定通道数（默认 2）；应用必须常开（Dante 设备只在 PCM 打开期间存在）。
- 环境变量（`INFERNO_` 前缀）：`NAME`、`DEVICE_ID`（16 hex，建议 MAC 补零 `aabbccddeeff0000`）、`BIND_IP`（或网卡名）、`SAMPLE_RATE`（模式 A=96000）、`PROCESS_ID`、`ALT_PORT`、`RX_CHANNELS`、`TX_CHANNELS`、`RX_LATENCY_NS`（默认 10 ms）、`TX_LATENCY_NS`、`CLOCK_PATH`（默认 usrvclock socket `/tmp/ptp-usrvclock`，即 statime 的导出路径）。
- 时钟：必须同时跑 statime fork（`inferno-ptpv1.toml`），并把系统时间同步服务停掉/排除 PTP 网卡（NTP 跳变会进媒体时钟）。
- 实时性：两个实时线程（FIFO 80/81）；需要 `ulimit -r ≥ 99`/等价 systemd `LimitRTPRIO=99`、`LimitMEMLOCK=infinity`，否则 RT 设置失败可能静默降级（Debian13.7 VM 实测 loopback_trx 普通用户失败/root 通过）。

### 4.3 linuxptp（B2 板端 GM 路径；`tspi-mode b2`）

AES67 口径 master（domain 0、E2E、announce 1 s、sync 250 ms、delay-req 1 s、TTL 16）：见 `/etc/ptp4l-master-aes67.conf`。
- `ptp4l -H -i eth0 -f /etc/ptp4l-master-aes67.conf -m`（RK3576 GMAC 支持 IEEE 1588 硬件时间戳，用 `ethtool -T eth0` 验证）。
- `phc2sys -s CLOCK_REALTIME -c eth0 -O 37 -m`：**必须加 `-O 37`**（IEEE1588 时标是 TAI=UTC+闰秒差；GM 若播 UTC，slave 会看到 ~37 s 假偏移、锁不上——2026-11-17 VM 实测）。闰秒变化时改 unit 里的数字并同步 `TSPI_GM_TAI_OFFSET`。
- B2 的接收端不是 daemon/LKM（LKM 是 PTP slave-only 且在 PRE_ROUTING 截包，本机 GM 的包走 LOCAL_OUT 永远收不到）：改用 Inferno + `tspi-phc-usrvclock` 桥（`/usr/local/lib/tspi/phc-usrvclock` 把 PHC 导出成 `/tmp/ptp-usrvclock`，Inferno `CLOCK_PATH` 指向它）。
- B1 模式不需要 ptp4l/phc2sys/桥；`tspi-mode b2 --ptp-only` 只起 GM 时钟栈（不动音频）。

### 4.4 AES67 daemon 配置口径

- `daemon.conf` 关键项：`interface_name`（必须为接 AoIP 的网卡，如 `eth0`）、`sample_rate: 48000`、`tic_frame_size_at_1fs: 48`、`max_tic_frame_size: 1024`、`ptp_domain: 0`、`ptp_dscp: 48`、`playout_delay`、`rtp_mcast_base`、`http_port: 8080`、`ptp_status_script`。
- ALSA 设备：LKM 注册卡 `Merging RAVENNA`（我们用 modprobe `id=ravenna` 固定为 `hw:ravenna,0`）；**捕获/播放同一个 PCM 设备**（`snd_pcm_new(..., 1, 1, ...)`），CamillaDSP 用 capture 方向。
- daemon 是 PTPv2 **slave**：`GET /api/ptp/status` 返回 `ptp_status`（unlocked/locking/locked）；`ptp_status_script` 状态变化回调写 `/run/tspi/ptp-lock`（我们的 hook）。
- 通道上限：LKM 编译上限 128；public 构建/daemon 实际可用以 SDP 与 daemon 配置为准（默认 8 路安全），本次默认 2 路。
- 内核模块前提：`NETFILTER`、`HIGH_RES_TIMERS`、`NETLINK`；RT 内核需实测（hrtimer 已改用 tasklet+HARD 模式）。

### 4.5 双时钟域：rate adjust + async resampler（关键）

CamillaDSP 的 rate adjust 从 **capture 侧**施加修正：优先调 capture 设备时钟（仅 ALSA Loopback / USB Gadget 支持）；我们的 capture（Inferno 插件 / RAVENNA LKM）不支持，因此**必须启用异步重采样器**（`resampler: type: AsyncSinc, profile: Balanced`，或成本更低的 AsyncPoly），`enable_rate_adjust: true` 才有意义。仅开 `enable_rate_adjust` 而不配 async resampler 会打警告且不生效。

`target_level` 经验：由小到大 2×chunksize → 4×chunksize → 8×chunksize 三档试；越大越抗欠载、延迟越大；以 24 h 0 xrun 的最小值为交付值（见 ACCEPTANCE-MATRIX.md）。

### 4.6 位深/格式选择

- Dante 采集：固定 `S32_LE`（插件要求）；AES67 采集：`S32_LE`（LKM 内部 32-bit 环形缓冲，按 ALSA 格式转换；L16 源也能收）。
- MOTU 输出：`S24_3_LE` 显式声明 24-bit；若设备拒绝则该模式回落到“不指定 format”（CamillaDSP 自动挑最好，可能为 S32_LE），由 tspi-mode 记录 warning。
- 16-bit 输入不需要在 CamillaDSP 侧特殊配置（S32 容器内是 16-bit 有效位），但电平与抖动策略按源侧处理。

## 5. 缓冲/延迟模型（分层，不要混为一谈）

| 层 | 决定因素 | 典型量级 | 可否由本方案压缩 |
| --- | --- | --- | --- |
| AoIP 接收抖动缓冲 | Dante：`RX_LATENCY_NS`（Dante 侧 latency 设置，最大 40 ms）；AES67：LKM `jitter_buffer_multiplier × tic`（默认 3×1 ms） | Dante 1–10 ms；AES67 ≈3 ms（默认） | 可小调，风险高；受发送端 ptime/latency 限制 |
| CamillaDSP 处理块 | `chunksize` + `target_level` + `adjust_period` | chunksize 32 帧 @48k=0.667 ms；@96k=0.333 ms；实际稳态缓冲 ≈target_level | 是（本方案主要旋钮） |
| ALSA period/buffer | MOTU：`snd-usb-audio` period/buffer（CamillaDSP 取 buffer≈next_pow2(max(3×chunksize,4×min_period))，period=buffer/8）；RAVENNA：period=tic（默认 48 帧） | MOTU：~1–5 ms；AES67 固定 1 ms period | 部分（chunksize 间接影响） |
| USB 传输/设备 | USB 帧（1 ms 微帧）+ 设备内部缓冲 | ~1–3 ms | 否 |
| DSP 计算 | FIR 长度、通道数、采样率 | 低（REW 短 FIR）到高（长 FIR/多通道） | 否，需按验收矩阵选 chunksize |

**端到端下限** ≈ AoIP 抖动缓冲 + DSP/ALSA 段 + USB。32 帧（0.33–0.67 ms）只压缩 DSP/ALSA 段；若 AoIP 缓冲 3 ms，端到端不可能到 1 ms。验收以「该 FIR/声道配置下 24 h 0 xrun 的最小 chunksize」为准，**32 帧不是硬承诺**。

## 6. 目录与交付物

```text
projects/tspi-audio-dsp/audio/
├── README.md                 本文件
├── RISKS-AND-DECISIONS.md    风险表 + 决策点（B1/B2/B3、PTPv1 leader、ARM 可行性、切换回滚）
├── ACCEPTANCE-MATRIX.md      分级验收矩阵与实测方法
├── packages.list             Debian 12 aarch64 运行时/构建包清单（含版本）
├── fetch-debs.sh             构建机联网时抓齐离线 .deb 包池（aarch64）
├── build-camilladsp.sh       CamillaDSP 构建/取官方 aarch64 包
├── build-inferno.sh          Inferno + ALSA 插件 + statime（PTPv1 fork）
├── build-aes67-daemon.sh     aes67-daemon + MergingRavennaALSA.ko（ARM64）
├── build-linuxptp.sh         linuxptp（默认用 bookworm 包，可选源码）
├── build-all.sh              一键顺序构建（产出 release/bin 与 release/lib）
├── install-into-image.sh     给 firmware-builder 的 rootfs 集成契约
├── overlay/                  **进镜像的文件树**（rootfs 根开始：usr/local/bin、etc、usr/share …）
│   └── usr/local/lib/tspi/ucm-backport-mk5.sh   MOTU mk5 UCM 回移（可选，失败不致命）
├── overlay-manifest.md        overlay 每文件来源/权限/所属包/默认启停
├── configs/                  配置索引/变体与 FIR 示例（权威配置在 overlay/etc）
│   ├── README.md             对照表：任务书各项配置 -> overlay 路径
│   ├── layout-5.3.2.md       **5.3.2 十路映射 + H16 16ch 输入假设 + FIR/CPU/USB 预算**
│   ├── fir/                  交叉分频/FIR 示例配置与系数生成脚本（example-532-crossover.yml）
│   └── variants/             ptp4l slave、inferno PTPv2 leader、phc2sys 参数
├── BUILD-EXEC-2026-11-15.md   aarch64 构建执行记录（task-4：方法/产物/日志/未验证项）
```

> 任务书里的 “configs/ 下 camilladsp config.yml / ptp4l.conf / phc2sys / inferno 配置 / aes67 配置 / ALSA 配置”：为**单一事实来源**全部放在 `overlay/etc/`（`configs/README.md` 有对照表），本目录只放变体与 FIR 示例，避免两份配置漂移。
```

构建产物统一放 `release/`（不进 git）：`release/bin/`、`release/lib/`、`release/debs/`、`release/modules/`。`install-into-image.sh` 只认 `overlay/ + release/ + packages`，不联网。

## 7. 构建步骤（在 Debian 12 aarch64 构建机 / 目标板 rootfs 内执行）

> 从 Windows 拷贝到 Linux 后先 `chmod +x *.sh overlay/usr/local/bin/tspi-* overlay/usr/local/lib/tspi/*`（压缩包/共享目录可能丢执行位）。

前置：`extract` 后先 `fetch-debs.sh`（联网一次）→ 之后可完全离线。

```bash
# 0) 构建机准备（一次性）
sudo ./fetch-debs.sh --arch arm64 --out ./release/debs
sudo apt-get install -y ./release/debs/*.deb   # 或由 install-into-image.sh 离线安装
# 1) 顺序构建（构建机需能访问源码：release/bin 里可放离线 tarball/源码）
./build-linuxptp.sh --from-deb          # 默认直接用 bookworm 3.1.1-4，无需编译
./build-camilladsp.sh                   # 优先下载官方 aarch64 资产（可离线放 release/src/）
./build-inferno.sh                      # Inferno + statime（PTPv1）
./build-aes67-daemon.sh                 # 需目标内核 6.1.x(+RT) 的头文件/Kbuild
./build-all.sh --check                  # 汇总校验产物与 sha256
```

要点：
- **不要在 Windows 上交叉编译**；aes67 LKM 必须在**目标内核版本**的头文件下编译（`--kernel-dir` 指向 firmware-builder 产出的 kernel build 目录）。
- CamillaDSP/Inferno/statime 可用 `cross build --target aarch64-unknown-linux-gnu`（官方推荐），也可在 aarch64 机器原生构建；脚本两者都支持（`--method native|cross`）。
- Rust 版本：CamillaDSP 4.1.3 要求 ≥1.90；统一用 rustup stable（构建机实测 1.100 可编 Inferno：92/92 单测）。
- 构建脚本只负责“产出+校验”，不 `apt install`；依赖由 packages.list/fetch-debs.sh 解决，保证离线可复现。

## 8. 镜像集成契约（firmware-builder 调用）

```bash
# firmware-builder 在 Debian rootfs 组装完成后、镜像打包前调用：
./install-into-image.sh \
  --rootfs   /path/to/debian12-rootfs \
  --overlay  ./overlay \
  --release  ./release \
  --debs     ./release/debs \
  --kernel   /path/to/kernel-6.1-rt \
  --motu-ucm yes|no \
  --enable-units yes
# 退出码 0 = 成功；非 0 见脚本头部；可重复执行（幂等）
```

脚本行为（全部不联网）：
1. 校验 rootfs 是 Debian 12 aarch64（`/etc/debian_version`、`dpkg --print-architecture`）；
2. `dpkg -i` 安装 `--debs` 里的离线包（安装期临时写 policy-rc.d 阻止服务启动；缺依赖直接失败，不联网补齐）；
3. 按 `overlay/` 复制文件（`cp -a`，强制 root:root，保持权限；可重复执行幂等）；
4. 安装 LKM 到 `/lib/modules/<kver>/extra/` 并在 chroot 内 `depmod -a <kver>`；
5. 安装 MOTU UCM 回移（`--motu-ucm yes`，幂等；失败仅警告）；
6. 创建 systemd enable 符号链接（见 overlay-manifest.md 的 enabled/inactive 语义）；
7. 运行离线校验：`camilladsp --check`、`ptp4l -h`、`statime --version`、`aes67-daemon -h`、`ldd` 检查、`systemd-analyze verify`（chroot 内）；
8. 输出 `/etc/tspi-audio-manifest.txt`（安装清单+版本+sha256），供产线核对。

默认服务策略：**安装但不 enable 自启**（等价“enabled 但 inactive”）：所有服务 unit 带 `ConditionPathExists=/run/tspi/active-<mode>` 守卫，开机无 marker → 跳过；镜像里 `/etc/default/tspi-audio` 决定策略；可选 `tspi-boot-mode.service`（默认 disabled）在开机时读 `/etc/tspi/last-mode` 调 `tspi-mode --restore` 实现“恢复上次模式”。

## 9. 模式切换与 web-manager 契约

入口：`/usr/local/bin/tspi-mode`（bash；统一入口，web-manager 只调用它，不直接 systemctl）。

```text
tspi-mode dante [--json] [--dry-run]      # 切到 Dante 模式（statime PTPv1 follower）
tspi-mode aes67 [--json] [--dry-run]      # 切到 AES67 模式（默认 B1=daemon slave）
tspi-mode b2    [--json] [--dry-run] [--ptp-only]
                                          # B2：板端 PTPv2 GM（ptp4l+phc2sys -O 37）
                                          #   + Inferno/PHC 接收端；--ptp-only 只起 GM 时钟栈
tspi-mode off   [--json]                  # 全停（含卸载 LKM / 停 GM / 清 usrvclock socket）
tspi-mode status [--json]                 # 查询，不改变状态
tspi-mode --restore                       # 读 /etc/tspi/last-mode 恢复（给 boot 单元）
```

`TSPI_AES67_PTP_ROLE=master` 是 B2 的兼容别名：`tspi-mode aes67` 看到该值会转成 `b2`（daemon/LKM 是 PTP slave-only，不能与本机 GM 同跑）；默认 `slave` 时 `aes67` 仍是 B1。

退出码（固定契约，web-manager 依赖）：

| code | 含义 |
| --- | --- |
| 0 | 目标模式已就绪（幂等成功） |
| 1 | 未知错误（journalctl -t tspi-audio / -u tspi-camilladsp 查看） |
| 2 | 用法错误 |
| 3 | 前置缺失（二进制/配置/内核模块/内核能力） |
| 4 | 设备未找到（MOTU/RAVENNA/Inferno PCM） |
| 5 | PTP 在超时内未锁定（B2：GM 未进 MASTER / phc2sys 失败 / PHC 非 TAI） |
| 6 | AoIP 流未就绪（daemon sink/Dante 设备） |
| 7 | CamillaDSP 启动失败 |
| 8 | 切换后自检失败（B2：PHC→usrvclock 桥未就绪或桥自身不稳） |
| 9 | 已回滚到切换前模式（目标模式失败） |
| 10 | 另一个切换正在进行（flock） |

`tspi-mode status --json` 输出（schema v1，字段稳定；数值可能为 null 表示不可得）：

```json
{
  "schema": "tspi-audio/v1",
  "ts": "2026-11-15T09:30:00+08:00",
  "mode": "aes67",
  "state": "active",
  "layout": "5.3.2",
  "outputs": 10,
  "ptp_role": "slave",
  "ptp_lock": true,
  "pid": 1234,
  "ptp": {
    "role": "slave",
    "source": "aes67-daemon",
    "lock": true,
    "lock_raw": "locked",
    "offset_ns": 1234,
    "service": "tspi-aes67-daemon.service"
  },
  "aoip": { "kind": "aes67-daemon", "service": "tspi-aes67-daemon.service", "ready": true },
  "camilladsp": { "service": "tspi-camilladsp.service", "state": "active", "pid": 1234,
                  "config": "aes67.yml", "samplerate": 48000,
                  "chunksize": 64, "target_level": 256, "adjust_period": 10 },
  "alsa": { "playback": "hw:motu,0", "playback_channels": 22, "playback_format": "S24_3_LE",
            "capture": "hw:ravenna,0", "capture_format": "S32_LE",
            "capture_channels": 16, "capture_device": "hw:ravenna,0" },
  "last_error": null
}
```

实现要点：状态文件 `/run/tspi/state.json` 由脚本原子写；动态字段（服务 active、pid、PTP lock）在 `status` 时刷新。Dante 模式的 PTP 状态来自 statime 的观测 socket（`/run/tspi/statime-observe.sock`，JSON 内 `current_ds.offset_from_master` 为 i128 定点，按 `bits / 2^32 ≈ ns` 换算）；AES67 模式来自 daemon `ptp_status_script` 写出的 `/run/tspi/ptp-lock` 与 REST `/api/ptp/status`。

B2 的 status 字段：`ptp.role="master"`、`ptp.source="ptp4l"`、`ptp.port_state`（pmc PORT_DATA_SET）、`ptp.grandmaster`（本机 clockIdentity）、`ptp.lock=true` 表示端口已在 MASTER；`aoip.kind="inferno"`、`aoip.service="tspi-phc-usrvclock.service"`；`--ptp-only` 运行态为 `aoip.kind="none"` 且 CamillaDSP 不启动。

切换时序（脚本内部固定顺序，失败回滚）：
1. 停 CamillaDSP → 2. 停当前 AoIP（模式 B 还要 `rmmod` LKM）→ 3. 停当前 PTP（B2 先停 PHC usrvclock 桥）→ 4. 起目标 PTP（Dante: statime；AES67 B1: 无需外部进程；B2: ptp4l GM → 等 MASTER → phc2sys → PHC/TAI 自检）→ 5. 等 PTP lock（超时按退出码 5）→ 6. 起目标 AoIP（B1: modprobe LKM + daemon；A: 无独立进程；B2: PHC usrvclock 桥，等 socket 就绪，退出码 8）→ 7. 等设备/流就绪 → 8. 渲染并启动 CamillaDSP → 9. 自检（服务 active、PTP lock、ALSA 设备、CamillaDSP 存活 ≥3 s）→ 10. 写 state/last-mode。任一步失败：按相反顺序停目标组件，恢复切换前模式（`TSPI_ROLLBACK=1`，默认开；恢复 b2 时保留切换前的 全量/`--ptp-only` 变体）→ 退出 9。

切换期间**必然短暂断音**（秒级：停→等 lock→起），手动切换可接受；切换完成前 CamillaDSP 不输出，不会把爆音送进 MOTU（先停 DSP 再动时钟）。

## 10. 验收矩阵（摘要）

完整矩阵与实测命令见 `ACCEPTANCE-MATRIX.md`。核心口径：
- 维度：模式（48k AES67 / 96k Dante）× chunksize（32/64/128/256/1024）× FIR 强度（低/中/高）× 声道（默认 2，可配）；
- 每格记录：**chunksize、实际 ALSA period、buffer、target_level、CPU、xrun/underrun、最坏调度延迟（cyclictest）、端到端延迟估算**；
- 判定：**24 h 0 xrun** 的配置才算通过；32 帧不是硬承诺——达不到就报「该 FIR/声道配置下的稳定最小值」；
- 另外验收：PTP lock/offset、切换耗时与回滚、断音时长、离线启动（断网）、RT 权限（`ulimit -r`/`chrt`）。

## 11. 待用户确认（阻塞项）

1. **H16-AIP 时钟角色（最关键）**：AES67 模式下 H16 是保持默认 PTP slave，还是能切 clock leader？若只能 slave 且网内无其他 PTPv2 GM → B1 不成立，需要本机当 GM（B2/B3）或加外部 GM。Dante 模式下 H16 是否已开 clock leader？
2. **AES67 拓扑**：若 H16 不能当 GM：接受 B2（本机 GM + 换接收端，8–12 人日）还是 B3（加第二网口）？还是先只跑 Dante 模式？
3. **Dante 拓扑**：确认 H16-AIP 作为 Dante clock leader（待实测），或改用外部 Dante 主钟（DVS/Via/硬件）。
4. **输入 16ch 容器顺序**：H16 的 16ch 里 7.1.4/9.1.4 具体通道排列（A18 实测后回填 `configs/layout-5.3.2.md`）。
5. **低音管理/高置位置**：3 只 sub 的分频/增益/延迟/DBA 策略；Height L/R 摆位与延迟。
6. 是否启用 `tspi-boot-mode.service`（**已定默认不启用**，DEC-8；如需开机恢复请说明）。
7. 96k 下 MOTU 可用通道数与 S24_3_LE 支持情况（上板实测后回填矩阵）。

## 12. 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-11-15 | 初版：双模式互斥、组件矩阵、B1 默认 + B2/B3 决策点、镜像集成契约、验收矩阵 |
| 2026-11-17 | F-44 口径同步：CamillaDSP 已退役（Phase 1 不运行，仅历史/离线 oracle）；采样率统一 48/96 kHz、默认 48、禁止 176.4/192 kHz |

