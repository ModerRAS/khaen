# 验收矩阵（chunksize × FIR × 模式）与实测方法

> 判定基线：**24 h 运行 0 xrun（underrun/overrun）且无爆音/断流** 才算通过。32 帧是目标值不是硬承诺：达不到就报「该 FIR/声道配置下的稳定最小值」，并把稳定最小值作为交付值。
> 输出布局 = **5.3.2（10 路：Main 1-2 + Line 3-10；USB 22 playback 中 10..21 补零）**；输入 = **H16-AIP 16ch**（AES67 48k / Dante 96k；AES67 96k 不可用；16ch 容器 7.1.4/9.1.4，通道顺序 TBD→A18）。
> 本文所有命令均为上板执行（RK3576 Debian12 aarch64 + PREEMPT_RT）。记录表模板见 §7。

## 1. 维度与定义

| 维度 | 取值 | 说明 |
| --- | --- | --- |
| 模式 | `aes67`（48 kHz）、`dante`（96 kHz） | 见 README §1；两模式互斥，逐模式测 |
| chunksize | 32 / 64 / 128 / 256 / 1024 帧 | CamillaDSP `devices.chunksize`；由 `tspi-mode ... --chunksize N` 注入 |
| target_level | 默认 4×chunksize（32 帧档额外测 2× 与 8×） | 帧数；上限 `(4+queuelimit)×chunksize`=8×chunksize |
| FIR 强度 | 低 / 中 / 高（定义见下） | 只作用于输入声道（默认 2ch）；输出 22ch 中仅映射通道有信号 |
| 声道 | 输出 5.3.2 = 10 路（USB 22ch 补零）；输入 H16-AIP 16ch | 捕获 16ch（7.1.4/9.1.4 容器）→ mixer 映射到 MOTU 0..9；通道 10..21 必须静音（补零验证） |

FIR 强度定义（**每输出声道，10 路同时生效**；48k/96k 同一套 taps 数。正式分频建议 IIR + 短 FIR，见 configs/layout-5.3.2.md §4）：

| 强度 | FIR taps/声道 | 附加 IIR | 说明 |
| --- | --- | --- | --- |
| 低 | 512 | IIR 分频 + 2 PEQ/路 | 10 路 × 512 taps，常见房间校正量级 |
| 中 | 4096 | IIR 分频 + 2 PEQ/路 | 10 路 × 4096 taps（压力上升） |
| 高 | 16384 | IIR 分频 + 2 PEQ/路 | 10 路 × 16384 taps：直接卷积等价 15.7/31.5 GFLOP/s（48k/96k），FFT 实际约 0.4 GFLOP/s；只作压力档 |

系数生成（构建机/PC 上，任意工具）：
- 任意音频工具生成单声道 32-bit float WAV 冲激响应（长度= taps），或用 python/numpy 生成截止频率 80 Hz 的线性相位低通（窗函数法）；
- CamillaDSP Conv 过滤器支持 `type: Raw`（文本/二进制）与 `type: Wav`（读 WAV 某通道）；示例见 `configs/fir/`。
- 验收时把系数放 `/etc/camilladsp/coeffs/`，config 引用绝对路径。

## 2. 每次测试必须记录（缺一不算通过）

| 字段 | 获取方式 |
| --- | --- |
| chunksize | `tspi-mode status --json` → `camilladsp.chunksize` |
| **实际 ALSA period / buffer（帧）** | CamillaDSP `-l debug` 日志中 `Setting period size to N frames` / `Device is using a buffer size of N frames`（两设备各一条，分别记 playback/capture） |
| target_level | `tspi-mode status --json` → `camilladsp.target_level` |
| 输出映射核对 | 逐通道扫频：0..9 每路单独出声、10..21 必须静音（见 §5 A14） |
| 输入映射核对 | H16 16ch 容器顺序（in0..in15 ↔ 7.1.4/9.1.4 元素），见 §5 A18；结果回填 configs/layout-5.3.2.md |
| xrun 次数 | 日志（journal 或 `-o` 文件）匹配 `PB: wait underrun|PB: write underrun|Capture: .*overrun|Restarting playback after buffer underrun` 的行数；用 `journalctl -u tspi-camilladsp --since ... | grep -c -E ...` |
| CPU | `mpstat -P ALL 1`（camilladsp 进程所在核的 %usr+%sys+%soft 峰值/均值）；或 `pidstat -t -p $(pidof camilladsp) 1` |
| 最坏调度延迟 | 并发 `cyclictest --mlockall --smp --priority=80 --interval=5000 --distance=0 -D 30m`，取 Max 列 |
| 线程优先级/绑核 | `ps -eLo pid,tid,cls,rtprio,psr,comm | grep camilladsp`；`chrt -p <tid>` |
| 音频主观/客观 | 播 1 kHz 正弦 10 min + 节目素材，听爆音；`arecord` 回录 MOTU 输出（外部回环/录音接口）做连续性检查（waveform gap） |
| 端到端延迟估算 | AoIP 抖动缓冲 + target_level/采样率 + (buffer/2)/采样率 + USB≈1–2 ms（见 README §5） |

## 3. 实测矩阵（每格填一个结果；先做 10 分钟筛查，通过的格再跑 24 h）

### 3.1 AES67 模式（48 kHz，B1：daemon=PTP slave，外部 PTPv2 GM）

| chunksize | FIR 低（512） | FIR 中（4096） | FIR 高（16384） |
| --- | --- | --- | --- |
| 32 | ☐ | ☐ | ☐ |
| 64 | ☐ | ☐ | ☐ |
| 128 | ☐ | ☐ | ☐ |
| 256 | ☐ | ☐ | ☐ |
| 1024 | ☐ | ☐ | ☐ |

说明：AES67 的 LKM ALSA period 由 tic 决定（默认 1 ms = 48 帧 @48k），**chunksize 小于 48 不会降低 AoIP 段延迟**，只影响 DSP 内部块与 ring；矩阵里 32 帧仍要测（验证 DSP/播放端小 ring 的稳定性），但报告要写清“端到端下限受 1 ms tic + jitter buffer 约束”。

### 3.2 Dante 模式（96 kHz，PTPv1 follower + 外部 Dante 主钟）

| chunksize | FIR 低（512） | FIR 中（4096） | FIR 高（16384） |
| --- | --- | --- | --- |
| 32 | ☐ | ☐ | ☐ |
| 64 | ☐ | ☐ | ☐ |
| 128 | ☐ | ☐ | ☐ |
| 256 | ☐ | ☐ | ☐ |
| 1024 | ☐ | ☐ | ☐ |

说明：96k 下 32 帧 = 0.333 ms/块，是最高压力档；Inferno 的 ALSA 插件 ring 最小 buffer 为 1024 帧（插件实现限制，低于此的 buffer 请求会被抬到 1024 帧量级），因此 32 帧不保证可达——失败时记录「稳定最小值」。**5.3.2 十路 FIR 档位下 96k 更高，先测 64/128；16384 taps×10 路 96k 仅作压力档。**

### 3.3 target_level 敏感性（只对最小两档 chunksize 做）

| 模式 | chunksize | target=2× | target=4× | target=8× |
| --- | --- | --- | --- | --- |
| aes67 | 32 | ☐ | ☐ | ☐ |
| dante | 32 | ☐ | ☐ | ☐ |
| aes67 | 64 | ☐ | ☐ | ☐ |
| dante | 64 | ☐ | ☐ | ☐ |

## 4. 单项测试步骤（每格）

```bash
# 0) 环境检查（一次性）
cat /sys/kernel/realtime 2>/dev/null || cat /proc/sys/kernel/realtime 2>/dev/null
ulimit -r                                   # 期望 >=99（服务由 systemd LimitRTPRIO 保证；登录 shell 需 limits.d）
chrt -f 80 true && echo "RT ok"

# 1) 应用配置并观察启动
tspi-mode dante --chunksize 64 --target-level 256 --json
tspi-mode status --json

# 2) 抓 debug 日志中的 period/buffer（重启一次带 debug）
sudo systemctl stop tspi-camilladsp
sudo /usr/local/bin/camilladsp -l debug -p 1234 -a 127.0.0.1 /run/tspi/camilladsp.yml 2>&1 | tee /tmp/camilladsp-debug.log &
grep -E "period size|buffer size|rate adjust|resampl" /tmp/camilladsp-debug.log | head
# 记录两设备的 period/buffer 后，停掉手工进程，交回 systemd
kill %1; tspi-mode dante --chunksize 64 --target-level 256

# 3) 并发 cyclictest + 10 分钟筛查
sudo cyclictest --mlockall --smp --priority=80 --interval=5000 --distance=0 -D 10m > /tmp/cyclictest.log 2>&1 &
# 播放节目素材（Dante/AES67 源端播放）10 分钟
sleep 600; kill %1
tail -5 /tmp/cyclictest.log

# 4) 统计 10 分钟 xrun
journalctl -u tspi-camilladsp --since "-10min" | grep -c -E "underrun|overrun|Restarting playback"

# 5) 通过后跑 24 h（用 systemd 自身日志，勿手工 tee）
journalctl -u tspi-camilladsp --since "-24h" -p warning | wc -l

# 6) 采集记录（建议 web-manager 保存）
tspi-mode status --json > /tmp/result-$MODE-$CHUNK-$FIR.json
uptime; date; cat /proc/interrupts | grep -i -E "xhci|eth0|gmac" > /tmp/irq.txt
```

## 5. 其它必测项（与矩阵并行）

| # | 项目 | 方法 | 通过标准 |
| --- | --- | --- | --- |
| A1 | RT 权限 | `ulimit -r`（登录 shell 与 systemd 进程各一次）、`chrt -p $(pidof camilladsp)`、`chrt -f 80 true` | 音频线程 SCHED_FIFO、rtprio 80–99；无 “failed to set realtime” 日志 |
| A2 | PTP lock（AES67 B1） | daemon `GET /api/ptp/status`；`/run/tspi/ptp-lock`。**前提：网内有 PTPv2 GM（H16 默认 slave 或外部 GM，见 A19）** | 30 min 内 `ptp_status=locked`；offset 统计（daemon 报告 delay/jitter）稳定；无 unlock 抖动 |
| A3 | PTP lock（Dante，H16 做 leader） | statime 观测 socket JSON（`instance.current_ds.offset_from_master`）；H16 WebUI 确认 Dante clock leader 开启 | 30 min 内 offset 收敛并稳定（换算后 |offset| < 1 ms，典型 <100 µs）；无频繁 step；H16 状态为 leader |
| A4 | 模式切换 | `for i in $(seq 1 50); do tspi-mode dante; tspi-mode aes67; done`（或 25 轮） | 每轮 exit 0；断音时长记录（秒）；无残留进程/LKM（`lsmod | grep Merging`、`pgrep statime`） |
| A5 | 切换失败回滚 | 拔 MOTU / 停外部 PTP 源 / `systemctl stop tspi-aes67-daemon` 后切模式 | exit 9；自动回到上一模式并恢复出声；state.last_error 有中文/英文原因 |
| A6 | 离线启动 | 断网 + 重启，`tspi-mode dante`（源端本地 DVS 场景）或 `off` 后全链路检查 | 不访问网络也能启动服务；无 apt/dns 调用失败阻塞启动 |
| A7 | 16-bit 输入 | AES67 发送端改 L16（SDP `L16/48000/2`）；Dante 发送端改 16-bit 源 | CamillaDSP 不报错、有正常输出（电平/SNR 合理）；daemon/DC 显示格式正确 |
| A8 | 24-bit 输出 | `aplay -D hw:<motu>,0 --dump-hw-params` 记录格式；CamillaDSP 用 S24_3_LE 起 | 设备接受 S24_3_LE；回录检查无削顶/截断 |
| A9 | MOTU 热插拔 | 运行中拔/插 USB | 拔：CamillaDSP 报错但系统不崩、PTP 不断；插：`tspi-mode dante` 幂等恢复（或自动恢复策略由 web-manager 定） |
| A10 | 负载抗性 | `stress-ng --cpu 8 --io 2 --vm 1 --timeout 30m` 同时跑矩阵最小格 | xrun 仍为 0（或记录最早出错配置，作为“抗负载最小值”） |
| A11 | 温度/降频 | `cat /sys/class/thermal/thermal_zone*/temp` 每 5 min；`cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq` | 24 h 无热降频导致的 xrun；必要时记录环境温度 |
| A12 | UCM 回移（可选） | `alsaucm -c <card> list _verbs`；`alsaucm -c <card> set _verb HiFi`；`aplay -L | grep ultralite` | 列出/进入 HiFi verb；失败则记录并确认主链路（hw:）不受影响 |
| A13 | 开机恢复（可选） | enable `tspi-boot-mode`，重启 3 次 | 恢复到 last-mode；失败时停在 off 且可手动切换 |
| A14 | **5.3.2 通道映射（逐路扫频）** | ① 先用 `speaker-test -D hw:<motu>,0 -c 22 -t sine -f 1000` 快速核对**硬件通道号→物理口**顺序（不经 CamillaDSP）；② 再经 CamillaDSP：把 `/run/tspi/camilladsp.yml` 的 mixer mapping 临时只保留 dest=i（其余 dest 删除或 mute），播放 1 kHz，i 从 0 到 9 逐个听/回录对应物理输出 | 0..9 与 configs/layout-5.3.2.md 表一致（0/1=Main 1-2、2..4=Line 3..5、5..7=Line 6..8、8/9=Line 9/10）；不一致则记录实际顺序并改 mixer dest 映射 |
| A15 | **每路延迟/极性** | 用测量麦克风（REW/REW EQ）或双通道回录，测各通道相对主左的脉冲延迟与极性；也可用同源信号分送两路、一路 mixer `inverted: true` 后近场叠加做抵消验证 | 每路延迟/极性设置与实测一致；分频点附近无抵消凹陷（±3 dB 内） |
| A16 | **22ch 硬件补零** | CamillaDSP 以 22ch 播放；`cat /proc/asound/card<motu>/pcm0p/sub0/hw_params` 确认 channels=22；用回录（MOTU 输入/外部接口）或听感检查 ch10..21（S/PDIF/ADAT/Phones）无信号 | ch0..9 有信号、ch10..21 静音（无串音）；hw_params 显示 22ch |
| A17 | **96k USB 带宽压力** | Dante 96k + 10ch DSP + playback 22ch 连续播放 24 h；`cat /proc/asound/card<motu>/pcm0p/sub0/hw_params`；`dmesg | grep -iE "usb|xhci"` 查错误；必要时 `usbmon` 看带宽 | 0 xrun；无 USB 错误/掉线；CPU/温度在预算内（若失败：降 48k 或减少 active 通道并记录） |
| A18 | **H16 16ch 输入映射（逐路核对）** | ① 用 H16 播放已知声道内容（或专用多声道测试文件：每声道不同频率/语音），box 侧 `arecord -D hw:ravenna,0 -c 16 -f S32_LE -r 48000 -d 10 /tmp/in16.wav`（Dante 用 `-D inferno -r 96000`）；② 用 `sox`/python 逐通道看频谱，记录 in0..in15 对应的 7.1.4/9.1.4 元素；③ 回填 `configs/layout-5.3.2.md` §1 表与模板 mixer `sources[].channel` | in0..in15 与 7.1.4/9.1.4 元素对应关系确认（与文档一致或已更新）；5.3.2 十路来源正确（无 C/LFE/环绕错位） |
| A19 | **H16 PTP 角色 / AES67 主钟依赖（关键）** | 记录 H16 WebUI 的 PTP 设置：AES67 模式 role（slave/leader）、Dante 模式 clock leader；`tspi-mode aes67` 后读 daemon `/api/ptp/status` 与 `/run/tspi/ptp-lock` | 若 H16=leader（或外部 GM 存在）→ daemon lock 成功；若 H16=slave 且无其他 GM → lock 必失败，立即回报 Lead（B2/B3 或改 H16 设置），**不得把失败当软件 bug 修** |
| A20 | **AES67 48k / Dante 96k 固定** | 分别切模式后 `cat /proc/asound/card<motu>/pcm0p/sub0/hw_params` 与捕获侧 hw_params 核对采样率；CamillaDSP status JSON 的 samplerate | aes67=48000、dante=96000；无自动重采样；96k AES67 不可用（文档/模式选择器提示） |

## 6. 判定与交付值

1. **单格通过**：10 min 筛查 0 xrun 且主观无爆音 → 进入 24 h；24 h 0 xrun、无断流、PTP 无 unlock → 通过。
2. **每模式交付值** = { min(chunksize | 该 FIR 档 24 h 通过), 对应 target_level, 实测 period/buffer, CPU 峰值, cyclictest max }。
3. 若 32 帧不通过：交付 48k 与 96k 各自“稳定最小值”，并在报告注明与 32 帧的差距原因（AoIP 缓冲/插件 ring/USB）。
4. 高 FIR 档若 96k 不通过：记录“96k 最大可用 FIR taps”，DSP 分频建议用 48k 或缩短 FIR。

## 7. 记录表模板（每格一行，CSV/JSON 均可）

```text
mode,layout,outputs,fir_tier,taps,chunksize,target_level,period_pb,period_cap,buffer_pb,buffer_cap,cpu_pct_max,cyclictest_max_us,xruns_10min,xruns_24h,map_ok,verdict,notes
aes67,5.3.2,10,low,512,32,128,,,,,,,,,,pending,
dante,5.3.2,10,low,512,32,128,,,,,,,,,,pending,
```

## 8. 相关文件

- 配置模板：`overlay/etc/camilladsp/dante.yml`、`aes67.yml`；FIR 示例：`configs/fir/`；
- 模式脚本：`/usr/local/bin/tspi-mode`（`--chunksize/--target-level` 覆盖会写进 `/run/tspi/state.json`）；
- 风险与决策：`RISKS-AND-DECISIONS.md`。
