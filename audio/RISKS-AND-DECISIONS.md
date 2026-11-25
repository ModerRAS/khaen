# 风险表与决策点（音频栈）

> 关联：README.md（总体方案）、ACCEPTANCE-MATRIX.md（验收）。「状态」列：已确认=源码/上游证据已核实；待验证=需上板实测；待确认=需用户/Lead 决策。

## 1. 风险表

| ID | 风险 | 影响 | 概率 | 依据（可复查） | 缓解 / 结论 | 状态 |
| --- | --- | --- | --- | --- | --- | --- |
| R1 | **AES67 daemon ARM64 可行性未实机验证** | 模式 B 无法交付 | 中 | 可行证据：LKM 有 ARM32/64 补丁 `1f644eb`（arm-32bit.patch，启用 ARM 32/64bit）；驱动维护到 kernel 7.2（strscpy、6.12 managed buffer、6.15 timer、6.18 hrtimer 修复）；aes67-daemon 是重写的 C++ 守护进程（旧 README 的 “public Butler 仅 x86_64” 指旧闭源 Butler，不适用于它）；Ubuntu ARMv7 有官方测试记录 | 结论：**源码级可行**。上板验证清单：LKM 在 6.1(+RT) 编译/insmod、aplay/arecord 打开 `hw:ravenna,0`、daemon 与 LKM netlink 通信、PTP lock、48k L24/L16 收流；失败则转 B2（见 D1） | 待验证 |
| R2 | 双 PTP 栈/双 AoIP 互相干扰 | 时钟错乱、丢流 | 高（若同时跑） | PTPv1 与 PTPv2 报文不兼容；LKM 在 netfilter `PRE_ROUTING` 拦截 PTP/RTP（`c_wrapper_lib.c:60`），且按接口名过滤（`EtherTubeNetfilter.c:266`） | **设计上互斥**：dante/aes67/off 三模式；LKM 只在模式 B 加载、切走 `rmmod`；两套 PTP 服务互相排斥（脚本顺序停机）。已消除 | 已确认 |
| R3 | **Dante 模式本机不能做 PTPv1 master** | 若网内无 Dante 主钟则无音频 | 确定（软件限制） | teodly/statime fork README 原文 “PTPv1 slave (follower) support”；只有 PTPv2 具备 leader 能力（`slave-only=false` 仅对 PTPv2 有意义） | **H16-AIP 可做 Dante clock leader**（厂商实测/论坛资料）→ 模式 A 由 H16 当主钟、box 做 follower，D2a 可行；若 H16 固件不支持 leader：外部 Dante 主钟或全网 AES67（PTPv2 leader，`configs/variants/inferno-ptpv2-leader.toml`）；待 H16 实测确认 | 待确认（H16 配置） |
| R4 | **AES67 模式“本机 PTPv2 master + daemon”不成立** | 需求冲突：daemon 收不了音 | 确定（若按原需求） | LKM PTP 为 slave-only（Announce/Sync/FollowUp + DelayReq，选主逻辑 `manager.c`）；本地发包走 `LOCAL_OUT` 不经过 `PRE_ROUTING`，LKM 永远收不到本机 ptp4l 的包；无 lock 时音频回调不出帧（`manager.c:1481`），改采样率 4 s 超时（`manager.c:469`） | 模式 B 默认 **B1：daemon 做 slave + 外部 PTPv2 GM**；如需本机 GM 见 D1（B2 换接收端 / B3 双网口）。**待用户拍板** | 待确认 |
| R5 | LKM 与 PREEMPT_RT 内核兼容性 | 模式 B 抖动/崩溃 | 中 | 驱动近期用 tasklet + HRTIMER_MODE_HARD、CPU-pinned timer（6.15+）；RT 内核下 hrtimer/netfilter 行为与主线不同 | 上板跑 cyclictest + 24 h 流；准备非 RT 内核镜像做对照；必要时给 LKM 打 RT 适配补丁（预留工作量 2–3 人日） | 待验证 |
| R6 | LKM 与目标内核 6.1 编译 | 模式 B 起不来 | 低 | 上游明确支持 5.15→6.2 区间（`ce46ebd` stdarg 5.15、`ee262e9` 6.2/6.3、6.8/6.12/6.15/6.18 各有修复） | 用 firmware-builder 产出的 6.1(+RT) kernel build 目录编译；保留 kernel 日志；失败则回退到主线 6.1.99-rt36 头文件交叉验证 | 待验证 |
| R7 | 切换模式短暂断音/爆音 | 用户体验 | 确定（可接受） | 停 DSP→停 AoIP→等 PTP lock→起 DSP，秒级 | 先停 CamillaDSP 再动时钟；切换完成前不出声；报告“断音 2–10 s”为设计行为；web-manager 展示 transition 状态 | 已确认（可接受） |
| R8 | 切换失败留下半死状态 | 设备无声且难排查 | 中 | 任何一步都可能失败（PTP 不 lock、设备没插、daemon 起不来） | 脚本内置回滚（恢复切换前模式，exit 9）+ `--dry-run` + `/run/khaen/state.json` 记录 last_error + /var/log/khaen/mode.log；回滚本身失败则退到 off（fail-safe） | 已确认（需验收） |
| R9 | Debian 12 的 alsa-ucm-conf 1.2.8 没有 MOTU mk5 UCM | 无 UCM 便捷设备/路由 | 确定 | bookworm=1.2.8-1（sources.debian.org）；上游 mk5 UCM 2024-05-27 加入（commit 2bd2aca），首个含它的 release=v1.2.13；v1.2.13 的 UCM 树用 Syntax 7，而 bookworm alsa-lib 1.2.8 只支持 ≤Syntax 6（ucm_local.h） | **不整树替换**（会破坏所有 UCM）。方案：回移 mk5 两个文件 + 在 1.2.8 的 USB-Audio.conf 插入 Syntax 6 的 mk5 分支（`ucm-backport-mk5.sh`，幂等）；**主音频链路不依赖 UCM**（CamillaDSP 直接 hw: + mixer 映射），UCM 失败只降级为“无便捷 stereo 设备” | 已确认（回移待验证） |
| R10 | MOTU 96 kHz 下通道数/格式未知 | 96k 配置可能打不开 | 中 | UCM 的 22/20 通道是 1FS 口径；96k 下 ADAT 通道会变化，USB 流通道数需实测 | khaen-mode 在切换时 `aplay --dump-hw-params` 探测实际 CHANNELS/FORMAT，按探测值渲染 CamillaDSP；`S24_3_LE` 不接受则回落不指定 format | 待验证（上板） |
| R11 | 32 帧（chunksize）在 96k/FIR 重载下达不到 | 延迟指标不达标 | 高（96k 0.33 ms/块） | 96 kHz chunksize 32 = 0.333 ms；RT 抖动 + USB + FIR 计算叠加 | 32 帧定位为“目标/最好情况”，验收以 24 h 0 xrun 的**稳定最小值**交付（ACCEPTANCE-MATRIX.md）；96k 建议从 64 起测，48k 从 32 起测 | 已确认（验收口径） |
| R12 | Inferno alpha 质量 / PTPv1 时钟抖动 | Dante 偶发断流 | 中 | 上游 README 自述“Highly experimental”；PTPv1 需要 statime fork；NTP 会污染时钟（README 明确） | 镜像禁用 PTP 网卡上的 NTP（chrony/timesyncd 排除 eth0 或直接停用），statime 用 `virtual-system-clock-base = monotonic_raw`；RT 权限（limits+systemd）强制；24 h soak | 已确认（需验收） |
| R13 | RT 权限缺失导致静默降级 | 抖动/xrun、无法复现 | 高（Debian 默认） | Debian13.7 VM 实测：普通用户 `ulimit -r=0`，Inferno loopback_trx 普通用户失败、root 通过 | overlay 预置 `/etc/security/limits.d/95-khaen-audio.conf`（`@audio rtprio 99/memlock unlimited`）、所有音频 unit `LimitRTPRIO=99`/`LimitMEMLOCK=infinity`、`RealtimePriority` 检查；验收含 `ulimit -r` 与 `chrt -f 80 true` | 已确认（需验收） |
| R14 | USB 音频 IRQ / 线程优先级未调 | USB xrun | 中 | USB xHCI 中断与 MOTU 周期传输；RT 下需绑核/提优先级 | 建议（写进 unit/文档，不强制）：CamillaDSP 主线程由 `audio_thread_priority` 自动提升（CamillaDSP 内置），xHCI IRQ 绑 A72 核、避开 isolcpus 的系统核；`cyclictest` 与 `cat /proc/interrupts` 对照。具体 IRQ 号上板确认 | 待验证 |
| R15 | 离线包/工具链版本漂移 | 构建不可复现 | 中 | Debian 点版本会更新 | packages.list 固定版本（source 版本）；`fetch-debs.sh` 记录实际 .deb 文件名+sha256 到 `release/debs/MANIFEST.txt`；Rust 工具链用 rustup 固定版本（建议 1.90.x 或 1.100.x，记录 rustc -V） | 已确认 |
| R16 | 服务以 root 运行面过大 | 安全 | 中 | 音频栈需要 CAP_SYS_TIME/CAP_NET_ADMIN/RT | v1 全部 root（简单可靠）；文档列出收紧路线（daemon: CAP_NET_ADMIN+CAP_SYS_RAWIO? statime: CAP_SYS_TIME；camilladsp: 仅 RT），列为后续加固项 | 已确认（技术债） |
| R17 | CamillaDSP websocket 暴露 | 未授权控制 | 低 | 若绑 0.0.0.0 | 固定绑 `127.0.0.1:1234`，只由 web-manager（本机）转发；防火墙默认拒绝外部 1234 | 已确认 |
| R18 | 采样率/位深口径错误（源与设备不匹配） | 无声/变调 | 中 | AES67 标准 48k；Dante 96k；L16/L24 与 S24_3_LE 选择 | 模式固定采样率（AES67 48k/Dante 96k）；CamillaDSP 不自动改率；源侧采样率不符时由 web-manager 报“源不匹配”；不做自动重采样（除 rate adjust 的 async 1:1 微调） | 已确认 |
| R19 | UCM 回移补丁在 Debian 点版本升级后失配 | 镜像构建失败/警告 | 低 | 补丁基于 bookworm 1.2.8-1 的 USB-Audio.conf 文本锚点 | `ucm-backport-mk5.sh` 幂等+锚点校验；失配时 warning 并跳过（不阻断镜像）；提供 `--motu-ucm no` 开关 | 已确认 |
| R20 | 声道路由错误（MOTU 22 通道映射） | 声音出到错误接口 | 中 | UCM 通道顺序假设：0/1=Main 1-2、2..9=Line 3..10、10/11=Phones、12+=SPDIF/ADAT（来自 alsa-ucm-conf UltraLite-mk5-HiFi.conf 的 PlaybackChannel 顺序，**未上板核对**） | 5.3.2 映射表见 configs/layout-5.3.2.md；验收 A14 逐路扫频核对（`speaker-test -c 22` 先验硬件顺序，再经 CamillaDSP 验 mixer 映射）；不一致只改 mixer dest | 待验证 |
| R21 | **5.3.2@96k USB 带宽** | xrun/掉线 | 中 | 22ch × 96k × 24bit = 6.34 MB/s ≈ 50.7 Mbps（仅播放，未含 USB 帧开销；若输入端点也工作再叠加）；USB2 HS 等时可用 ~280–350 Mbps | 验收 A17：24h 压力 + `hw_params` 确认 22ch/96k + dmesg/usbmon；失败降 48k 或减少 active 通道（记录口径变化） | 待验证 |
| R22 | **10 路 FIR/CPU 预算** | xrun/延迟不达标 | 中 | 直接卷积等价：10 路 × 16384 taps @96k = 31.5 GFLOP/s；FFT 卷积实际约 0.43 GFLOP/s（估算，见 layout-5.3.2.md §4） | 首选 IIR 分频 + 512–4096 taps/路短 FIR；16384 仅压力档；验收矩阵按 10 路重跑，24h 0 xrun 的 taps 作为上限 | 待验证 |
| R23 | **H16 16ch 输入容器顺序/低音管理/高置位置 TBD** | 通道映射错误、返工 | 高 | H16-AIP 送 16ch（当前 7.1.4/9.1.4 容器），但 7.1.4 只占 12ch，16ch 多出的 4 路含义未知；低音管理/高置摆位未定 | 模板 capture=16ch、mixer 按假设顺序（0=L 1=R 2=C 3=LFE 4=Ls 5=Rs 8=Tfl 9=Tfr）映射到 5.3.2；验收 A18 用 H16 逐通道测试信号核对后只改 mixer `sources[].channel`，不动服务/脚本 | 待确认（用户+H16 实测） |
| R24 | CamillaDSP 不做 Atmos/DTS 解码 | 多声道源无法直接用 | 确定 | CamillaDSP 只处理 PCM 通道，不做码流解码 | H16 必须在 HDMI 侧收到已解码 PCM（或上游解码后送 HDMI LPCM）；文档与 web-manager 明确提示 | 已确认（约束） |
| R25 | **H16-AIP 默认是 PTP Slave** | AES67 B1（daemon=slave）收不到 lock，模式 B 无声 | 高（若 H16 保持默认） | **官方原文**（<https://nuprime-x.com.tw/product/h16-aip/> 固件更新说明）：「由於 H16 預設設定為 Slave，請確保至少有一台 AoIP 裝置設定為 Master」→ **H16 默认必须由别的设备当 Master**。此前"社区实测某些配置可切 leader"**无来源，已作废（2026-11-21 复核，见 R28）** | 先按 Lead 口径**保持 B1 默认**，等用户确认 H16 时钟设置；若 H16 只能 slave 且网内无其他 GM → 必须走 B2（box 当 PTPv2 GM + 换 AES67 接收端，8–12 人日）或加外部 GM；**不能假装 B1 能工作** | 待确认（关键路径） |
| R26 | **H16 16ch 输入通道顺序未知** | 声道错位（C/LFE/环绕/高置互换） | 高 | 16ch 容器 7.1.4（12ch）/9.1.4（14ch）之外多出的通道含义不明；WAVE 顺序假设未验证 | 验收 A18：H16 逐通道测试信号（或已知声道内容）→ box 16ch 回读；确认后改 mixer 映射；板卡验证清单加 16ch 输入逐路核对 | 待验证 |
| R27 | **采样率限制：AES67 固定 48k；AES67 96k 不可用** | 96k 只能走 Dante 模式 | 确定 | H16 规格：AES67 模式固定 48 kHz；Dante 模式 96 kHz（开启 AES67 时关闭 96k） | 模式矩阵固定：aes67=48k、dante=96k；web-manager 提示 96k 需选 Dante 模式；ACCEPTANCE 只测对应采样率 | 已确认（约束） |
| R28 | H16 作为 PTP/Dante 主时钟（clock leader / PM） | 决定 B1 能否闭环、要不要自建 GM | — | **判定 UNPROVEN（明确不是 slave-only）**。①官方只写「預設設定為 Slave…至少有一台 AoIP 裝置設定為 Master」（<https://nuprime-x.com.tw/product/h16-aip/>），**从未说不能切 Master**；官方 28 页手册全文**无** PTP/时钟角色章节（负证据）。②**模块级证据**：H16 的 AoIP 模块在 AudioGridController 4.4.5 里按 **A series** 管理（Device Config 显示 `Nuprime-a227-xxxx`），AGC 手册 §V 明确有 **Preferred Master(PM)/Slave Only(SO)**、「若网内只有一台设备带此标志且处于 Slave，会立刻转成 master clock」→ **模块平台能当 master 是确定的**，未定的是 H16 固件是否开放 PM。③**两份独立第三方报告**：大音聲技 2026-04-30 商家 how-to「NuPrime H16-AIP 做 主時鐘」（Dante/DVS 场景，很可能是自动当选）；AudiophileStyle 用户 Carousel 2026-06「I switched clock leader to the H16, which is active for AES67」→ AES67 时钟恢复（**未写是在哪个界面点的、无截图**，这是唯一关键缺口）。→ **不能把"H16 当 GM"当作 B1 前提规划；也不要再写"H16 固定 slave"**。验证步骤见 `rt-stack/evidence/h16-clock-role-20261121/FINDINGS.md §6`（约 10–30 分钟：AGC Clock Status 截图 + PM 标记 + ptp4l -m/pmc + tcpdump 抓 Announce/SAP） |

## 2. 决策点（需要用户/Lead 拍板）

### D1（最大）AES67 模式的 PTP 角色：B1 / B2 / B3

背景：需求同时要求“AES67 daemon”与“PTP master”。单网口上二者不可兼得（R4）。

**H16-AIP 依赖（2026-11-15 新事实，关键路径）**：H16 官方产品页明确“默认 PTP Slave，必须至少一台 AoIP 设备设为 Master”。若 H16 不能切换为 clock leader、且网内没有其他 PTPv2 GM，则 **B1（daemon=slave）无法 lock，AES67 模式收不到音**；此时只有三条路：① H16 固件切 leader（社区实测部分配置可行，待用户确认 H16 设置）；② 网内加外部 PTPv2 GM；③ 走 B2（box 当 GM + 换 AES67 接收端）。**当前按 Lead 口径保持 B1 默认交付，但 B1 的上板前提是“网内已有 GM”；不能把 B1 当作 H16 默认拓扑下的可工作方案。**

三选一：

| 方案 | 拓扑 | 优点 | 缺点 | 工作量 | 建议 |
| --- | --- | --- | --- | --- | --- |
| **B1（默认交付）** | daemon 做 PTPv2 **slave**；网内需外部 PTPv2 GM（AES67 发送端/交换机/专用 GM） | daemon 开箱即用；最稳；AES67 标准做法 | 本机不是 GM；“PTP master”需求不满足 | 本方案已含 | ✅ 默认 |
| **B2** | 本机 ptp4l masterOnly 做 GM；AES67 接收改用不依赖内核 PTP slave 的接收端 | 满足“本机 GM”；daemon 仍可预装（不用） | 换接收端=新软件栈+新风险；与用户“用 AES67 daemon”的要求偏差 | 见下对比 | 若用户坚持 GM |
| **B3（实验）** | 加 USB 千兆网口；ptp4l master 从网口 B 发，经交换机进入网口 A，LKM 在 A 收到并 lock | 保留 daemon、本机是 GM | 需第二网口+同 L2 组播泛洪；依赖交换机 IGMP 行为；时钟链跨两张网卡需实测 | 2–4 人日（含硬件） | 低优先级 |

B2 的接收端对比：

| 选项 | 说明 | 工作量 | 风险 |
| --- | --- | --- | --- |
| **PipeWire 1.4+ AES67** | Debian 12 **bookworm-backports 有 pipewire 1.4.2-1~bpo12+1**（sources.debian.org 实测）；PipeWire 1.2+ 带 RTP/SAP AES67 模块，可把 AES67 流接到 CamillaDSP（PipeWire 后端或 ALSA 节点）；时钟侧需按 PipeWire AES67 文档配 PTP（ptp4l/linuxptp） | 集成+调试 2–4 人日；再做 24 h 稳定性 | 中：PTP 与 PipeWire 时钟耦合、backports 包维护、与 daemon 不是同一实现 |
| **自研最小 AES67 RTP 接收器** | Rust 重写：SDP 解析（L16/L24）、RTP 收包、9 字节/标准头、抖动缓冲、按 PTP 时钟调度、ALSA 输出（或直接接 CamillaDSP WS/管道）；可用 linuxptp 的 PHC/CLOCK_TAI | 8–12 人日（含测试） | 高：时钟恢复/丢包补偿/长期漂移全部自己担；与 daemon 功能重复 |

**B3-a 低优先级实验（veth 本地回环）**：Lead 提出可用 veth 让本机 ptp4l 的包进入 RX 路径从而触发 LKM 的 `PRE_ROUTING` 钩子。已核实两点：
1. veth 收包确实会走 `PRE_ROUTING`（作为 ingress 设备），方向成立；
2. 但 LKM 在 `EtherTubeNetfilter.c:266` 按 `ifname` 与配置的 `interface_name` **逐字节比较**，只接受同一张网卡的包。因此“PTP 从 veth 来、音频从 eth0 来”会被丢弃；除非把音频接口也改成 veth/桥接拓扑（复杂、额外故障面）。
结论：在“单物理网口 + 音频必须走 eth0”的前提下 veth 方案**不可行**，不列入 B3 实施；B3 仍按“第二块网卡 + 同 L2 交换机”的方向（需硬件）。

结论：**除非用户明确要求“本机必须当 AES67 GM”，否则按 B1 交付**；B2 优先级低于先把 B1 上板跑通。

### D2 Dante 模式的主钟来源

- D2a（默认，新事实）：**H16-AIP 可做 Dante clock leader**（社区实测/论坛资料）→ H16 当 Dante 主钟、本机 statime PTPv1 follower。**待 H16 实测确认**（R28）；若 H16 固件不支持 leader，再退回外部 Dante 主钟（DVS/Via/硬件设备）。
- D2b：网内没有 PTPv1 主钟且 Dante 设备都支持 AES67 → statime PTPv2 leader（`configs/variants/inferno-ptpv2-leader.toml`，`slave-only=false`；Inferno 用 PTPv2 + `CLOCK_PATH=/tmp/ptp-usrvclock`）。注意这不再是 PTPv1 网络。
- D2c：网内既无主钟、设备又不支持 AES67 → Dante 模式不可用（硬件限制，非软件可解）。

### D3 声道与输出映射（**已定：5.3.2 / 10 路**，2026-11-15 用户确认）

- 输出布局 = **5.3.2**（5 主 + 3 低音 + 2 高置 = 10 路）：MOTU **Main 1-2 + Line 3-10**；USB 22 playback 中 10..21（S/PDIF/ADAT/Phones）不用、补零。
- 映射表与实现：见 `configs/layout-5.3.2.md`；默认模板为 2ch→10ch 占位 upmix，正式分频/低音管理等待测量。
- **输入已明确（2026-11-15）**：H16-AIP 16ch 容器（7.1.4/9.1.4；AES67 48k / Dante 96k）；`capture.channels=16`。**仍 TBD**：① 16ch 容器内通道顺序（R26/A18）；② 低音管理（3 只 sub 的分频/增益/延迟/DBA）；③ 高置音箱位置与延迟。
- 96k 下 10 路 FIR 预算见 `configs/layout-5.3.2.md` §4；**建议 IIR 分频 + 512–4096 taps 短 FIR**，16384 taps×10 路只作压力档。

### D4 是否开机自动恢复模式

- 默认不自动（安全）：`khaen-boot-mode.service` 默认 disabled；若开启则读 `/etc/khaen/last-mode` 调 `khaen-mode --restore`。**待用户确认**。

### D5 是否升级 alsa-lib + 整棵 ucm2

- 若用户坚持“官方最新 UCM 而不打补丁”：需要把 bookworm alsa-lib 1.2.8 升到 ≥1.2.12 + ucm2 ≥v1.2.13（自建 deb 或替换），风险扩散到全系统音频；不推荐。默认走回移补丁。

## 3. 已定决策（记录，勿反复）

| 决策 | 内容 | 依据 |
| --- | --- | --- |
| DEC-1 | dante / aes67 / off 三模式互斥，统一入口 `khaen-mode` | Lead 2026-11-15 明确 |
| DEC-2 | AES67 默认 B1（daemon=PTP slave，外部 GM） | R4 + Lead 同意按 B1 写默认，B2/B3 写决策点 |
| DEC-3 | 采样率：AES67 48k、Dante 96k；输出 24-bit；处理率跟输入，不默认 192k | 用户口径 |
| DEC-4 | CamillaDSP 输出到 MOTU 用硬件通道 + mixer 映射（不依赖 UCM/route 插件） | R9、R20；便于多路分频扩展 |
| DEC-5 | 离线预装：二进制 + 配置 + unit 进 overlay；.deb 包池离线安装；服务安装但不开机自启（ConditionPathExists 守卫） | 用户 OEM 要求 + Lead 契约 |
| DEC-6 | LKM 只在模式 B 加载，切走 rmmod | R2 |
| DEC-7 | 32 帧不是硬承诺，以 24 h 0 xrun 的最小值为准 | Lead 口径 |
| DEC-8 | **D4 已定：默认不自动恢复模式**，khaen-boot-mode.service 保持 disabled；镜像首次开机为 off，由 web-manager/人工切换 | Lead 验收 2026-11-15 |
| DEC-9 | **D5 已定：接受 MOTU mk5 UCM 回移方案**（只回移 mk5 文件 + Syntax 6 分派补丁；主音频链路不依赖 UCM，失败仅降级） | Lead 验收 2026-11-15 |
| DEC-10 | D1 维持默认 B1（AES67 daemon=PTPv2 slave，外部 GM）；B2/B3 仍为决策点待用户选择 | Lead 2026-11-15 |
| DEC-11 | D2（Dante PTPv1 主钟来源）待用户确认 | Lead 2026-11-15 |
| DEC-12 | **D3 已定：输出布局 5.3.2 / 10 路**（Main1-2 + Line3-10；USB 22ch 补零）；输入声道数、低音管理、高置位置仍 TBD | 用户 2026-11-15 |
| DEC-13 | 5.3.2 映射假设（0/1=Main1-2、2..9=Line3-10）需上板逐路核对（A14）；不一致只改 mixer dest | 本方案 |
| DEC-14 | **H16-AIP 事实纳入方案**：HDMI→AES67/Dante 16ch 发送端；AES67 固定 48k、Dante 96k（AES67 96k 不可用）；输入 16ch（7.1.4/9.1.4 容器）；H16 默认 PTP slave，官方要求至少一台 AoIP 设备为 Master | Lead 2026-11-15（厂商页+社区资料） |
| DEC-15 | **Dante 模式主钟 = H16-AIP（待实测确认）**；本机 statime 只做 PTPv1 follower | Lead 2026-11-15 + R28 |
| DEC-16 | **AES67 模式 PTP 角色仍按 B1 默认交付，但依赖“网内已有 GM”**；若 H16 只能 slave 且无外部 GM，需用户拍板 B2/B3 或改 H16 设置 | Lead 2026-11-15 |

## 4. 上板验证清单（按顺序，失败即停并回报）

```bash
# 1) 内核/硬件
uname -a; cat /sys/kernel/realtime 2>/dev/null; ethtool -T eth0 | grep -E "hardware|SOF_TIMESTAMPING"
# 2) MOTU 枚举
aplay -l; arecord -l; cat /proc/asound/cards
aplay -D hw:<motu>,0 --dump-hw-params /dev/zero 2>&1 | head -30   # 记录 CHANNELS/FORMAT/PERIOD
# 2b) 16ch 输入（H16-AIP）
#   AES67: arecord -D hw:ravenna,0 -c 16 -f S32_LE -r 48000 -d 5 /tmp/in16.wav
#   Dante: arecord -D inferno -c 16 -f S32_LE -r 96000 -d 5 /tmp/in16.wav
#   用 H16 逐通道测试信号核对 16ch 容器顺序（R26/A18），再决定 mixer sources[].channel
# 2c) H16 时钟角色（关键）
#   记录 H16 WebUI：AES67 模式 PTP role（slave/leader）、Dante 模式 clock leader 开关；
#   若 AES67 下 H16 只能 slave → 本机 B1 无法 lock，立即回报 Lead 决定 B2/B3
# 3) RT 权限
ulimit -r; chrt -f 80 true && echo RT-OK
# 4) Dante 模式
khaen-mode dante --json; khaen-mode status --json
#   检查 statime 观测 socket:
socat - UNIX-CONNECT:/run/khaen/statime-observe.sock | head -c 400   # 或 nc -U
# 5) AES67 模式（B1，需外部 PTPv2 GM）
khaen-mode aes67 --json; khaen-mode status --json
curl -s http://127.0.0.1:8080/api/ptp/status
arecord -D hw:ravenna,0 -c 2 -f S32_LE -r 48000 -d 5 /tmp/t.wav && aplay -D hw:<motu>,0 /tmp/t.wav
# 6) 24h soak（见 ACCEPTANCE-MATRIX.md）
```

## 5. 相关文档

- `README.md`：总体方案/矩阵/构建/镜像契约/模式契约；
- `ACCEPTANCE-MATRIX.md`：chunksize×FIR×模式 验收矩阵与实测方法；
- `overlay-manifest.md`：每个镜像文件的来源/权限/默认状态。
