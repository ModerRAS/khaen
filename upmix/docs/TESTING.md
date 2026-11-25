# 测试方法与结果（2.0 上混）

## 1. 环境与依赖

- CamillaDSP 4.1.3（Windows 文件后端用官方 windows-amd64 包；板端 /usr/local/bin/camilladsp）。
- Python 3 + PyYAML：跑生成器与文件后端脚本（板端自带 python3-yaml）。
- numpy + scipy：仅开发机做信号生成与结果分析（板端不装也可，分析可回传开发机）。
- khaen-channelmap：renderer 变体需要（板端 OEM v5 已含修复版）。

## 2. 测试信号（tests/gen_signals.py）

| 信号 | 用途 |
| --- | --- |
| noise_pink_stereo / mono / antiphase | 粉噪：频带、电平、相关性、能量 |
| sweep_stereo / antiphase | 20 Hz–20 kHz 对数扫频：全频段检查 |
| tone_center_1k / left_1k / right_1k / antiphase_1k | 中心/左/右/反相单音：矩阵与隔离 |
| sine_40hz / sine_100hz | 低音管理分频 |
| impulse | 延迟/滤波群延迟 |
| music_proxy | 合成音乐（中心低音/旋律 + 左右打击乐）冒烟 |

## 3. 本机测试（开发机）

```bash
python3 tests/gen_signals.py --out /tmp/upmix-signals
python3 tests/run_file_backend.py --workdir /tmp/upmix-run \
  --signals /tmp/upmix-signals --camilladsp /path/to/camilladsp \
  --khaen-upmix ./khaen-upmix \
  --layout-presets /etc/khaen-audio/layout-presets.yml \
  --channelmap /usr/local/bin/khaen-channelmap \
  --variants standalone,renderer \
  --presets upmix-4.1.2-music,upmix-4.1.2-movie,upmix-4.1.2-wide,upmix-5.3.2-music,upmix-5.3.2-movie,upmix-5.3.2-wide
python3 tests/analyze.py --workdir /tmp/upmix-run --report /tmp/upmix-run/report.md
```

每条 entry 的检查项：

1. **与参考 DSP 仿真逐样本一致**：analyze.py 用 numpy/scipy 复刻同一 plan（矩阵、LR4 分频、
   delay、gain），与 CamillaDSP 实际输出比较（跳过起始 0.3s 滤波器建立过程），阈值 max diff ≤ 2e-4。
2. **通道电平与设计一致**：每通道实测 RMS 与仿真预测比较，|Δ| ≤ 0.25 dB；预测静音通道实测 ≤ −80 dBFS。
3. **无数字削波**：分析窗内 peak ≤ 0.999。
4. **脉冲延迟/滤波链符合配置**：impulse 测试中每角色脉冲峰位置与仿真一致（±8 sample），
   覆盖 Delay 滤波与 LR 分频群延迟。
5. **两条路径一致**：standalone 与 renderer 两组输出逐样本比较（≤1e-6 判一致）。
6. 报告额外给出：每 preset 的倍频带电平（32 Hz–16 kHz）、通道相关性矩阵、与 FL 的互相关（±50ms）。

结果（2026-11-16 复跑，CamillaDSP 4.1.3，高度全频化后；旧 11-15 数据已作废）：

- standalone/renderer 各 78 条 entry（6 预设 × 13 信号），全部成功；**480 项检查，0 失败**。
- 两条路径输出**逐样本相同**（max|diff| = 0.00e+00，全部 preset/信号）。
- 与参考仿真 max|diff| ≈ 1e-8（float32 量化级）。
- 关键电平（4.1.2-music，输入 −12 dBFS 单音）：
  - 1 kHz 中心：FL/FR −15.0，SL/SR −240（差信号 0），SUB1 −102.8，HFL/HFR −30.0（全频高度，2026-11-16 起）
  - 1 kHz 仅左：FL −15.0，FR −240，SL/SR −27.0（差信号 ×0.5 再 −6 dB）
  - 1 kHz 反相：FL/FR −15.0，SL/SR −21.0（全差信号），SUB −240
  - 40 Hz 中心：FL/FR −39.6（80 Hz 高通），SUB1 −15.5（低频全部到 SUB）
- 相位/梳状：SL/SR 与 FL 的 0-lag 相关 ≈ 0.00；最大相关出现在配置延迟处（−15.1 ms）。
  纯差信号预设 SL–SR = −1.00；wide 的左右延迟差把相关降到约 −0.95（allpass 模式）。
- 4.1.2-movie 的 SL/SR 相关约 −0.95；高度 HFL–HFR ≈ +1.00（music 单声道高度；movie width=0.3 略低）。
- **验收边界（A-P1-15）**：以上“与参考仿真一致”只证明渲染器忠实执行 plan；plan/映射本身正确性不由该测试证明（plan 错仿真也会一致）。映射正确性另由 role→物理通道期望表/人工复核。
- 本轮（2026-11-16）实际：156/156 条 entry 成功（6 预设×13 信号×2 路径），480 项检查 0 失败；两条路径 max|diff|=0.00e+00。

## 4. 板端测试（RK3576，6.1.99-rt36，OEM v5）

> **本节数据为 2026-11-15 旧版（bandlimited height + 旧渲染器）实测，标 SUPERSEDED；2026-11-16 高度全频化后未在板端复测，仅作历史参考。**

只跑文件后端与配置校验，**不占声卡、不启动服务**。

```bash
# 开发机把 signals/、tests/、khaen-upmix 传到板端 /tmp/upmix-test 后：
python3 tests/board_time.py --workdir /tmp/upmix-test/run \
  --signals /tmp/upmix-test/signals \
  --khaen-upmix /usr/local/bin/khaen-upmix \
  --layout-presets /etc/khaen-audio/layout-presets.yml \
  --camilladsp /usr/local/bin/camilladsp \
  --presets upmix-4.1.2-music,upmix-5.3.2-music \
  --chunksizes 128,256,512,1024,2048

python3 tests/run_file_backend.py --workdir /tmp/upmix-test/run-backend \
  --signals /tmp/upmix-test/signals \
  --camilladsp /usr/local/bin/camilladsp \
  --khaen-upmix /usr/local/bin/khaen-upmix \
  --layout-presets /etc/khaen-audio/layout-presets.yml \
  --channelmap /usr/local/bin/khaen-channelmap --channelmap-python python3 \
  --variants standalone,renderer \
  --presets upmix-4.1.2-music,upmix-5.3.2-music \
  --signals-filter 'tone_*,noise_pink_stereo.wav,impulse.wav,sine_40hz_center.wav'
```

板端结果（20 s 立体声粉噪，文件后端）：

| preset | chunksize | wall(s) | RTF | CPU(s) | CPU/单核 |
| --- | --- | --- | --- | --- | --- |
| 4.1.2-music | 128 | 15.01 | 1.33 | 1.59 | 7.9% |
| 4.1.2-music | 256 | 5.00 | 4.00 | 1.08 | 5.4% |
| 4.1.2-music | 512 | 2.55 | 7.85 | 0.83 | 4.2% |
| 4.1.2-music | 1024 | 1.48 | 13.49 | 0.75 | 3.7% |
| 4.1.2-music | 2048 | 1.04 | 19.30 | 0.46 | 2.3% |
| 5.3.2-music | 128 | 9.66 | 2.07 | 1.67 | 8.3% |
| 5.3.2-music | 256 | 5.38 | 3.72 | 1.08 | 5.4% |
| 5.3.2-music | 512 | 6.42 | 3.11 | 1.30 | 6.5% |
| 5.3.2-music | 1024 | 1.70 | 11.76 | 0.62 | 3.1% |
| 5.3.2-music | 2048 | 1.21 | 16.51 | 0.63 | 3.1% |

- 28/28 文件后端运行成功；抽 20 条输出回传分析：**64 项检查 0 失败**，两条路径仍逐样本一致。
- ALSA 配置 `camilladsp --check`：6 个示例全部 "Config is valid"（不打开设备）。
- 文件后端的 wall 时间包含小文件读写；cs=128 受 I/O 影响 wall 偏大，CPU 时间才反映 DSP 负载。
  按 CPU 计：20 s 音频耗时 0.46–1.67 s，即单核 2–8%，8 核平台实时余量充足。
- 实时路延迟估算：上混自身最大 18 ms（环绕 movie/wide；高度 0 ms）+ chunksize/48k + target_level/48k
  （实际预设 target_level=1024：48k、chunk 128 时约 18 + 2.7 + 21.3 ≈ 42 ms；chunk 512/2048 时相应增加）。
  真机 ALSA 延迟需 audio-stack 的 MOTU 冒烟确认（P2-5）。
- 独立配置按 MOTU **22ch** 打开，但逻辑布局只用 4.1.2 的槽位 0-5,7 / 5.3.2 的 0-9，其余恒静音；文件后端配置同理（P2-6）。

## 5. 已知测试缺口

1. 未做 MOTU 真机播放（声卡由 audio-stack 占用做 32 帧冒烟）；本工具所有测试只用文件后端。
2. H16 未确认，输入 16ch 容器通道顺序未测（本任务只覆盖 2ch 输入）。
3. 96k（Dante）只做了配置层校验（@camilladsp --check@ + 生成逻辑），**未做 96k 文件后端/真机实测**；MOTU 22ch@96k 上限待核，不能写成“已验证成立”（A-P1-12）。
4. allpass 去相关只在本机 4.1.3 文件后端验证（同一版本二进制），未在板端跑过。

## 6. 复现产出

```
/tmp/upmix-run/
├── results.json               # 每条 entry 的路径/rc/RTF/耗时
├── plans/<preset>.json        # 生成器解析出的 DSP 计划（参考仿真输入）
├── out/<variant>/<preset>/<signal>.wav
├── report.md                  # 自动生成的分析报告（含上述检查表）
└── board-results.json         # 板端计时（可选，analyze 会自动并入报告）
```


## 7. 输入内容模式测试（task-19）

```bash
# 开发机生成容器信号（numpy），板端只需上传 WAV
python3 tests/gen_container_signals.py --out /tmp/container-signals

python3 tests/test_input_mode.py --khaen-upmix ./khaen-upmix \
  --layout-presets /etc/khaen-audio/layout-presets.yml \
  --signals /tmp/container-signals --camilladsp /usr/local/bin/camilladsp
```

检查项（纯 stdlib，板端可跑）：

1. `detect --wav`：2.0 容器→stereo；7.1.4→multichannel；C-only→multichannel；静音→unknown；
   `h16_internal_upmix`/reason/字段齐全。
2. `input-mode`：无覆盖→override=null；`force-stereo/force-multichannel` 归一化；`auto`。
3. `autodetect --simulate`：默认直通、上混/直通迟滞切换、LFE 不作判据、启动冻结、override 优先。
4. 双路径文件后端（给 --camilladsp 时）：stereo 模式输出与 2ch 上混**逐样本一致**；
   pass 模式按角色映射（100/110/500/510/300/310/60 Hz 单音）；C-only 在 stereo 模式静音；
   2.0 容器在 pass 模式只有 FL/FR 有能量。

结果：本机 58/58 PASS；板端 OEM v5 58/58 PASS（不占声卡）。另见 mock websocket 测试
（`tests/test_input_mode.py` 之外单独跑：握手 / GetCaptureSignalRms / SetFaderVolume / state）。

