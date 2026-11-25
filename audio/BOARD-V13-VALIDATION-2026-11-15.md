# 板端 v13 验证记录（2026-11-15）

镜像：OEM v14 候选前 v13（1,414,687,363 B，sha256 `11dcfa17…def7`）；板端已烧 v13，LKM 修复版加载成功。

## 1. v13 载荷核验（板端实测）
- khaen-mode `c4ec1b88…c329`；khaen-upmix `3625bd60…a0e5`；upmix presets `5004dac3…45b9`；autodetect unit `53d79568…a0`；camilladsp unit `4f72cb3b…7c11`（含 `--gain1=-120 --gain2=0`）；channelmap `7abe6c92…ca1`；LKM `9b722905…98d1`。
- `/proc/asound/cards`：3=UMIK-1（U18dB）、4=MOTU UltraLite-mk5、5=Loopback、6=ravenna；`MergingRavennaALSA` 已加载。
- `khaen-mode status --json` 含 `input_content` 字段；units inactive（按需）。

## 2. UMIK 声学闭环（v13，1kHz / -50 dBFS 源）
- 链路：VM Inferno TX（`inferno-vm-tx`，1kHz -50dBFS 循环）→ 板端 Inferno capture（fake usrvclock）→ CamillaDSP 2.0→4.1.2 → MOTU card4（22ch/48k/S24_3_LE，chunksize 256）。
- 结果：`xruns=0`；UMIK card3 20s S24_3LE 2ch 录音成功。
- FFT：**1kHz 峰值 1000.03 Hz，-86.7 dBFS**；噪声底（200–800Hz）**-113.3 dBFS**；2k/3k 谐波 -115.4/-116.4 dBFS（≈28dB 低于基频）。
- 备注：首次 UMIK 尝试因 `/tmp/dante-test/fakeclock-arm` 未成功部署（scp 目标目录不存在）导致 CamillaDSP 无时钟退出、录到静音；补部署 fakeclock 后复测成功。

## 3. snd-aloop 60s xrun（沉默，MOTU card4）
| chunksize | wall | CPU | xruns | 结论 |
|---|---|---|---|---|
| 32 | 60s | 7.1% | **206** | ❌ 不稳定 |
| 64 | 61s | 7.1% | **2** | ⚠️ 边缘 |
| 128 | 60s / 61s | 6.7% / 5.5% | **1 / 1**（两轮） | ⚠️ 边缘，偶发 1 次 PB underrun |
| 256 | 61s / 60s | 5.6% / 4.7% | **0 / 0**（两轮） | ✅ 稳定 |
- 结论：该 aloop→MOTU@48k 链路在 **chunksize≥256 才两轮 0 xrun**；128/64 每轮 1–2 次（边缘），32 帧 206 次不可用。
- 初步定位：小 chunksize 时 CamillaDSP/ALSA 周期很短（32 帧 @48k ≈ 0.67ms），当前 CamillaDSP 仅 Nice=-10、未设 SCHED_FIFO 实时调度，用户态抖动触发 PB underrun；建议后续用 SCHED_FIFO 80（两个实时线程）、重评 target_level、period/buffer 与 aloop timer，再追求 32 帧。
- 32 帧低延迟目标当前**不作为交付状态**；48k 先用 256。

### 3.1 SCHED_FIFO 80 实验（2026-11-15，不改镜像，仅运行期）
- 先确认现状：CamillaDSP 4.1.3 线程默认 **SCHED_OTHER**；CLI 无 capture/playback 线程优先级参数。
- 方法：chrt -f 80 启动 camilladsp（wrapper）；实测全部 7 个线程均为 **FF/80**，playback hw_params 正常（22ch/48k/S24_3LE）。
- 结果（60s，snd-aloop→MOTU，48k；baseline = Nice=-10）：
| chunksize | baseline xrun | SCHED_FIFO 80 xrun | FIFO CPU |
|---|---|---|---|
| 32 | 206 | **362** ❌ | 1.2% |
| 64 | 2 | **0 / 0** ✅ | 5.0–5.1% |
| 128 | 1 / 1 | **0 / 0** ✅ | 3.6% |
| 256 | 0 / 0 | **0 / 0** ✅ | 2.8% |
- 无回归观察：FIFO 64/128 运行期间 Web :8088/healthz 每 6s 均 200，外部 SSH 正常；CPU 下降。
- 注意：该方式把**主线程也设为 FIFO 80**（全进程），长时间/异常时有饥饿风险；60s 实验不足以证明量产稳定，需 24h soak + 真实 Dante/UMIK 链路复测。
- 结论：SCHED_FIFO 80 使 **64/128 明显改善（0 xrun）**，32 仍不可用（362，更差）。若进镜像：systemd unit 加 CPUSchedulingPolicy=fifo + CPUSchedulingPriority=80（保留 Nice=-10/LimitRTPRIO=99），无需 CamillaDSP 参数。

## 4. 正式 `khaen-mode dante` 阻塞点（记录）
- 板端 v13（MOTU/UMIK 在位、无 H16、无 Dante 源）：`khaen-mode dante --allow-unlocked-ptp` 到 CamillaDSP 启动后 **exit / abort**，`khaen-mode status` 为 `mode=off, state=error`，journal 显示 CamillaDSP panic/abort。
- 原因判定：**当前环境无 H16/无 Inferno 订阅媒体源，也无有效 PTPv1 master**；Inferno capture 无时钟/无订阅时 hw_params 超时，CamillaDSP 退出。属预期环境限制，不是 v13 载荷回归。
- 建议（后续）：khaen-mode 在 dante 模式启动前/失败时给出明确提示（无 active channelmap / 无 Inferno 订阅源 / PTP 未锁），避免 generic panic；正式模式验证需 H16 或先预置 16ch Inferno 订阅 + 可用 PTP 时钟。
- 手工链路（fake usrvclock + 预置 rx_subscriptions）已验证可用（见 §2）。

## 5. v14 同步
- `/etc/default/khaen-audio` 的 `KHAEN_IFACE=end0`、`KHAEN_MOTU_CARD_ID=UltraLitemk5` 在 Windows canonical overlay 已一致（2026-11-15 核）。