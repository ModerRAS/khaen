# MOTU UltraLite-mk5 硬件冒烟（泰山派3M，RT OEM v5/v6，2026-11-15）

> 板子：`6.1.99-rt36 #3 SMP PREEMPT_RT`，MOTU card 3 `UltraLite-mk5`（USB `07fd:000c`），音频默认 off；测试用户 lckfb（audio 组，sudo 可用）。

## 1. ALSA 能力（`--dump-hw-params` / `stream0`）

| 参数 | 值 |
| --- | --- |
| 可用格式 | **仅 `S24_3LE`**（24-bit packed 3B） |
| 通道数 | [10, 22]（ALSA 视图） |
| 采样率 | [44100, 192000] |
| period_size | [6, 192000] |
| buffer_size | [12, 384000] |
| periods | [2, 1024] |

**关键约束（USB altset 与采样率绑定）**：
- 44.1/48 kHz → **22 通道**（altset 1，`S24_3LE`，ASYNC + Implicit Feedback）；
- 88.2/96 kHz → **18 通道**（altset 2）；
- 176.4/192 kHz → **10 通道**（altset 3）。

⇒ 48k 下不能用 10ch：请求 10ch/48k 会被 ALSA 接受但实际跑 176.4k（aplay 警告 “rate not accurate, got 176400”）。**48k 必须用 22ch（多余通道静音）**，96k 用 18ch。4.1.2（7 路）/5.1.2（8 路）配置需渲染为 playback 22ch 填充。

## 2. 打开/帧长测试

| 测试 | 结果 |
| --- | --- |
| `aplay -D hw:3,0 -c 22 -r 48000 -f S24_3LE --period-size=32 --buffer-size=256` | **OK**；hw_params: period 32 / buffer 256（手动设置时） |
| `aplay ... -c 18 -r 96000 --period-size=32 --buffer-size=256` | **OK**；hw_params: 18ch / 96k / period 32 / buffer 256 |
| `aplay ... -c 10 -r 48000` | 实际 176.4k（不可用；见上） |
| CamillaDSP chunksize 32 | hw_params: **period_size 16 / buffer_size 128**（CamillaDSP 自动 period 计算；2ch 以上填充 22ch 均正常） |

## 3. CamillaDSP 实链路冒烟（WavFile 源 → 22ch USB，48k，chunksize 32，target_level 128/256）

| 配置 | `--check` | 运行 | xrun（6s 窗口） | CPU（单核占比，文件源非实时 pacing，仅参考） |
| --- | --- | --- | --- | --- |
| 直通 22ch，TL128 | valid | rc=0 | **1（仅启动时）** | 17–36% |
| 直通 22ch，TL256 | valid | rc=0 | 1（启动） | 14–16% |
| 5.1.2 渲染配置（22ch 填充） | valid | rc=0 | 1（启动） | 58–69% |
| 4.1.2 渲染配置（22ch 填充） | valid | rc=0 | 1（启动） | 31–53% |

- 每次运行仅在启动瞬间出现 1 条 `PB: Prepare playback after buffer underrun`，随后 6s 内无新增 xrun；提高 target_level 不消除启动 xrun（属启动瞬态）。
- 文件捕获非实时 pacing：6s WAV 的实际 wall 在 1.7–6.2s 之间波动（USB implicit feedback/ALSA 缓冲导致），因此 CPU% 不是最终实时占用；需真实连续源（H16/Ravenna 或 ALSA loopback）复测。
- 时基：chunksize 32（0.667ms）、target_level 128（2.67ms）、ALSA buffer 128（2.67ms）、period 16（0.333ms）。

## 4. 限制与下一步
- **OEM rootfs 未带 snd-aloop/snd-dummy 模块**（`modprobe snd-aloop` 失败、/lib/modules 无 sound 驱动），无法用 loopback 做连续实时冒烟；建议 firmware-builder 在 RT 内核/rootfs 补 `snd-aloop`（或 `snd-dummy`）用于 24h xrun 自测（非产品必需）。
- 板端 smoke 已验证：渲染器 4.1.2/5.1.2 配置 → `camilladsp --check` valid；ALSA 32 帧 + 22ch/48k 可用；启动瞬态 xrun 1 次。
- **待 H16 接入**：AES67/Dante → CamillaDSP → MOTU 真实链路，用 H16 持续信号跑 24h xrun/延迟/CPU，并按 ACCEPTANCE-MATRIX 记录。
## 5. 渲染器 MOTU 默认规则（2026-11-15 决策，Lead 批准）
- 默认 playback 通道数按 MOTU UltraLite-mk5 USB altset：**48k=22ch、96k=18ch、176.4/192k=10ch**（不再依赖 profile 里的 channels）；显式 `--playback-channels` 仍可覆盖。
- 默认 playback format = `S24_3LE`（MOTU 唯一可用格式）。
- 4.1.2 默认输出映射（0-based）：FL=0、FR=1、SL=2、SR=3、HFL=4、HFR=5、SUB1=7（即 1-based 1-2/3-4/5-6/8），通道 6 与 8-21 静音填充。
- 回归：v2 测试 **37/37 PASS**（新增 MOTU 默认通道/4.1.2 映射用例）；渲染产物 `camilladsp --check` valid。
- 覆盖文件：`khaen-channelmap` sha256 `ea245721b180ae4de44ae5771f9ed1dbfa0249ec800949a65f697ec1fee1df19`；`layout-presets.yml` sha256 `4856109265371666a6001804330e2a595dad8599b3aecc2f4390be679b2427da`。