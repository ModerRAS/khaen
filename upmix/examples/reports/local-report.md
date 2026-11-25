# 2.0 立体声上混 · 文件后端测试报告

## 0. 独立配置 vs channelmap 渲染路径

> **SUPERSEDED（2026-11-16）**：本报告为高度带限版本（HFL/HFR 1.8–10 kHz 带限 + 18 ms 延迟）的旧证据；
> 用户裁决后高度改为全频/0 延迟，examples 已重新生成，最新本机证据见
> `scratch/lead/height-policy/local-report.md`。保留本文件仅作历史对照。


| preset | 对比信号数 | max\|diff\| | 结论 |
| --- | --- | --- | --- |
| upmix-4.1.2-movie | 13 | 0.00e+00 | 一致（逐样本） |
| upmix-4.1.2-music | 13 | 0.00e+00 | 一致（逐样本） |
| upmix-4.1.2-wide | 13 | 0.00e+00 | 一致（逐样本） |
| upmix-5.3.2-movie | 13 | 0.00e+00 | 一致（逐样本） |
| upmix-5.3.2-music | 13 | 0.00e+00 | 一致（逐样本） |
| upmix-5.3.2-wide | 13 | 0.00e+00 | 一致（逐样本） |

## 1. 信号级验证（与参考仿真对比）

每行 = 一个 preset/variant/测试信号；检查项含：与参考仿真逐样本一致、通道电平与设计一致、无数字削波、脉冲延迟/滤波链符合配置。

| preset | variant | 信号 | 结果 | 失败详情 |
| --- | --- | --- | --- | --- |
| upmix-4.1.2-music | standalone | impulse | 4/4 PASS | - |
| upmix-4.1.2-music | renderer | impulse | 4/4 PASS | - |
| upmix-4.1.2-music | standalone | music_proxy | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | music_proxy | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | noise_pink_antiphase | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | noise_pink_antiphase | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | noise_pink_mono | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | noise_pink_mono | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | noise_pink_stereo | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | noise_pink_stereo | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | sine_100hz_center | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | sine_100hz_center | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | sine_40hz_center | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | sine_40hz_center | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | sweep_antiphase | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | sweep_antiphase | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | sweep_stereo | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | sweep_stereo | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | tone_antiphase_1k | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | tone_antiphase_1k | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | tone_center_1k | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | tone_center_1k | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | tone_left_1k | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | tone_left_1k | 3/3 PASS | - |
| upmix-4.1.2-music | standalone | tone_right_1k | 3/3 PASS | - |
| upmix-4.1.2-music | renderer | tone_right_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | impulse | 4/4 PASS | - |
| upmix-4.1.2-movie | renderer | impulse | 4/4 PASS | - |
| upmix-4.1.2-movie | standalone | music_proxy | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | music_proxy | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | noise_pink_antiphase | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | noise_pink_antiphase | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | noise_pink_mono | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | noise_pink_mono | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | noise_pink_stereo | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | noise_pink_stereo | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | sine_100hz_center | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | sine_100hz_center | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | sine_40hz_center | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | sine_40hz_center | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | sweep_antiphase | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | sweep_antiphase | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | sweep_stereo | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | sweep_stereo | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | tone_antiphase_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | tone_antiphase_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | tone_center_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | tone_center_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | tone_left_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | tone_left_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | standalone | tone_right_1k | 3/3 PASS | - |
| upmix-4.1.2-movie | renderer | tone_right_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | impulse | 4/4 PASS | - |
| upmix-4.1.2-wide | renderer | impulse | 4/4 PASS | - |
| upmix-4.1.2-wide | standalone | music_proxy | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | music_proxy | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | noise_pink_antiphase | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | noise_pink_antiphase | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | noise_pink_mono | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | noise_pink_mono | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | noise_pink_stereo | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | noise_pink_stereo | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | sine_100hz_center | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | sine_100hz_center | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | sine_40hz_center | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | sine_40hz_center | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | sweep_antiphase | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | sweep_antiphase | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | sweep_stereo | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | sweep_stereo | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | tone_antiphase_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | tone_antiphase_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | tone_center_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | tone_center_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | tone_left_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | tone_left_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | standalone | tone_right_1k | 3/3 PASS | - |
| upmix-4.1.2-wide | renderer | tone_right_1k | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | impulse | 4/4 PASS | - |
| upmix-5.3.2-music | renderer | impulse | 4/4 PASS | - |
| upmix-5.3.2-music | standalone | music_proxy | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | music_proxy | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | noise_pink_antiphase | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | noise_pink_antiphase | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | noise_pink_mono | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | noise_pink_mono | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | noise_pink_stereo | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | noise_pink_stereo | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | sine_100hz_center | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | sine_100hz_center | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | sine_40hz_center | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | sine_40hz_center | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | sweep_antiphase | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | sweep_antiphase | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | sweep_stereo | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | sweep_stereo | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | tone_antiphase_1k | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | tone_antiphase_1k | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | tone_center_1k | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | tone_center_1k | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | tone_left_1k | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | tone_left_1k | 3/3 PASS | - |
| upmix-5.3.2-music | standalone | tone_right_1k | 3/3 PASS | - |
| upmix-5.3.2-music | renderer | tone_right_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | impulse | 4/4 PASS | - |
| upmix-5.3.2-movie | renderer | impulse | 4/4 PASS | - |
| upmix-5.3.2-movie | standalone | music_proxy | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | music_proxy | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | noise_pink_antiphase | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | noise_pink_antiphase | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | noise_pink_mono | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | noise_pink_mono | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | noise_pink_stereo | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | noise_pink_stereo | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | sine_100hz_center | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | sine_100hz_center | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | sine_40hz_center | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | sine_40hz_center | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | sweep_antiphase | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | sweep_antiphase | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | sweep_stereo | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | sweep_stereo | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | tone_antiphase_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | tone_antiphase_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | tone_center_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | tone_center_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | tone_left_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | tone_left_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | standalone | tone_right_1k | 3/3 PASS | - |
| upmix-5.3.2-movie | renderer | tone_right_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | impulse | 4/4 PASS | - |
| upmix-5.3.2-wide | renderer | impulse | 4/4 PASS | - |
| upmix-5.3.2-wide | standalone | music_proxy | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | music_proxy | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | noise_pink_antiphase | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | noise_pink_antiphase | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | noise_pink_mono | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | noise_pink_mono | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | noise_pink_stereo | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | noise_pink_stereo | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | sine_100hz_center | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | sine_100hz_center | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | sine_40hz_center | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | sine_40hz_center | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | sweep_antiphase | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | sweep_antiphase | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | sweep_stereo | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | sweep_stereo | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | tone_antiphase_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | tone_antiphase_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | tone_center_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | tone_center_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | tone_left_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | tone_left_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | standalone | tone_right_1k | 3/3 PASS | - |
| upmix-5.3.2-wide | renderer | tone_right_1k | 3/3 PASS | - |

合计：480 项检查，0 项失败。

### upmix-4.1.2-movie（4.1.2，8ch）

**电平（RMS dBFS）**

| 信号 | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| tone_center_1k | -15.0 | -15.0 | -30.0 | -30.0 | -42.7 | -42.7 | -102.8 |
| tone_left_1k | -15.0 | -240.0 | -22.1 | -26.6 | -46.5 | -51.9 | -108.8 |
| tone_antiphase_1k | -15.0 | -15.0 | -18.0 | -18.0 | -53.2 | -53.2 | -240.0 |
| sine_40hz_center | -39.6 | -39.6 | -62.0 | -62.0 | -153.1 | -153.1 | -15.5 |

**脉冲延迟（samples @ 48000 Hz）**

| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |
| --- | --- | --- | --- |
| FL | 0.0 | 0 | 0.00 |
| FR | 0.0 | 0 | 0.00 |
| SL | 18.0 | 867 | 18.06 |
| SR | 18.0 | 867 | 18.06 |
| HFL | 20.0 | 965 | 20.10 |
| HFR | 20.0 | 965 | 20.10 |
| SUB1 | 0.0 | 276 | 5.75 |

**粉噪倍频带电平（dB）**

| 频带(Hz) | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 32 | -60.9 | -61.3 | -74.3 | -74.4 | -176.8 | -176.9 | -34.8 |
| 63 | -41.4 | -41.3 | -51.4 | -51.5 | -152.2 | -152.5 | -38.8 |
| 125 | -34.5 | -34.0 | -41.4 | -41.1 | -128.9 | -128.6 | -50.2 |
| 250 | -33.2 | -32.8 | -38.8 | -38.6 | -105.0 | -105.0 | -71.3 |
| 500 | -33.2 | -32.6 | -38.8 | -38.5 | -81.7 | -81.4 | -95.1 |
| 1000 | -33.0 | -32.6 | -38.6 | -38.4 | -60.6 | -60.5 | -119.2 |
| 2000 | -32.9 | -32.5 | -38.6 | -38.4 | -50.1 | -49.8 | -143.1 |
| 4000 | -33.0 | -32.6 | -39.2 | -38.9 | -48.0 | -47.7 | -167.2 |
| 8000 | -33.0 | -32.5 | -44.0 | -43.7 | -50.2 | -49.9 | -182.3 |
| 16000 | -33.0 | -32.5 | -65.0 | -64.7 | -65.5 | -65.1 | -179.9 |

**通道相关性矩阵**（粉噪立体声，2–3.5s）

| | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| FL | +1.00 | -0.01 | +0.00 | -0.00 | +0.00 | +0.00 | +0.00 |
| FR | -0.01 | +1.00 | +0.01 | -0.01 | +0.00 | -0.00 | +0.00 |
| SL | +0.00 | +0.01 | +1.00 | -0.88 | -0.00 | +0.00 | +0.00 |
| SR | -0.00 | -0.01 | -0.88 | +1.00 | +0.00 | -0.00 | +0.00 |
| HFL | +0.00 | +0.00 | -0.00 | +0.00 | +1.00 | +0.83 | +0.00 |
| HFR | +0.00 | -0.00 | +0.00 | -0.00 | +0.83 | +1.00 | +0.00 |
| SUB1 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 |

**与 FL 的互相关（粉噪，±50ms）**

| 角色 | 延迟(ms) | r(lag0) | max\|r\| | 峰值滞后(ms) |
| --- | --- | --- | --- | --- |
| FR | 0.0 | -0.009 | 0.024 | 23.8 |
| SL | 18.0 | +0.006 | 0.741 | -18.1 |
| SR | 18.0 | -0.004 | 0.435 | -18.1 |
| HFL | 20.0 | +0.003 | 0.495 | -20.1 |
| HFR | 20.0 | +0.002 | 0.255 | -20.1 |
| SUB1 | 0.0 | +0.027 | 0.027 | -0.1 |

### upmix-4.1.2-music（4.1.2，8ch）

**电平（RMS dBFS）**

| 信号 | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| tone_center_1k | -15.0 | -15.0 | -240.0 | -240.0 | -51.4 | -51.4 | -102.8 |
| tone_left_1k | -15.0 | -240.0 | -27.0 | -27.0 | -57.4 | -57.4 | -108.8 |
| tone_antiphase_1k | -15.0 | -15.0 | -21.0 | -21.0 | -240.0 | -240.0 | -240.0 |
| sine_40hz_center | -39.6 | -39.6 | -240.0 | -240.0 | -162.4 | -162.4 | -15.5 |

**脉冲延迟（samples @ 48000 Hz）**

| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |
| --- | --- | --- | --- |
| FL | 0.0 | 0 | 0.00 |
| FR | 0.0 | 0 | 0.00 |
| HFL | 18.0 | 869 | 18.10 |
| HFR | 18.0 | 869 | 18.10 |
| SUB1 | 0.0 | 276 | 5.75 |

**粉噪倍频带电平（dB）**

| 频带(Hz) | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 32 | -60.9 | -61.3 | -77.7 | -77.7 | -186.6 | -186.6 | -34.8 |
| 63 | -41.4 | -41.3 | -54.7 | -54.7 | -162.2 | -162.2 | -38.8 |
| 125 | -34.5 | -34.0 | -44.5 | -44.5 | -138.5 | -138.5 | -50.2 |
| 250 | -33.2 | -32.8 | -42.0 | -42.0 | -114.7 | -114.7 | -71.3 |
| 500 | -33.2 | -32.6 | -42.0 | -42.0 | -91.2 | -91.2 | -95.1 |
| 1000 | -33.0 | -32.6 | -41.7 | -41.7 | -68.7 | -68.7 | -119.2 |
| 2000 | -32.9 | -32.5 | -41.8 | -41.8 | -55.0 | -55.0 | -143.1 |
| 4000 | -33.0 | -32.6 | -42.7 | -42.7 | -51.4 | -51.4 | -167.2 |
| 8000 | -33.0 | -32.5 | -49.3 | -49.3 | -53.4 | -53.4 | -182.3 |
| 16000 | -33.0 | -32.5 | -73.2 | -73.2 | -68.7 | -68.7 | -179.9 |

**通道相关性矩阵**（粉噪立体声，2–3.5s）

| | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| FL | +1.00 | -0.01 | +0.01 | -0.01 | +0.00 | +0.00 | +0.00 |
| FR | -0.01 | +1.00 | -0.01 | +0.01 | +0.01 | +0.01 | +0.00 |
| SL | +0.01 | -0.01 | +1.00 | -1.00 | -0.00 | -0.00 | +0.00 |
| SR | -0.01 | +0.01 | -1.00 | +1.00 | +0.00 | +0.00 | +0.00 |
| HFL | +0.00 | +0.01 | -0.00 | +0.00 | +1.00 | +1.00 | +0.00 |
| HFR | +0.00 | +0.01 | -0.00 | +0.00 | +1.00 | +1.00 | +0.00 |
| SUB1 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 |

**与 FL 的互相关（粉噪，±50ms）**

| 角色 | 延迟(ms) | r(lag0) | max\|r\| | 峰值滞后(ms) |
| --- | --- | --- | --- | --- |
| FR | 0.0 | -0.009 | 0.024 | 23.8 |
| SL | 15.0 | -0.001 | 0.598 | -15.1 |
| SR | 15.0 | +0.001 | 0.598 | -15.1 |
| HFL | 18.0 | +0.003 | 0.382 | -18.1 |
| HFR | 18.0 | +0.003 | 0.382 | -18.1 |
| SUB1 | 0.0 | +0.027 | 0.027 | -0.1 |

### upmix-4.1.2-wide（4.1.2，8ch）

**电平（RMS dBFS）**

| 信号 | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| tone_center_1k | -15.0 | -15.0 | -28.0 | -28.0 | -45.7 | -45.7 | -102.8 |
| tone_left_1k | -15.0 | -240.0 | -22.4 | -28.9 | -48.2 | -57.7 | -108.8 |
| tone_antiphase_1k | -15.0 | -15.0 | -19.0 | -19.0 | -51.7 | -51.7 | -240.0 |
| sine_40hz_center | -39.6 | -39.6 | -60.0 | -60.0 | -156.3 | -156.3 | -15.5 |

**脉冲延迟（samples @ 48000 Hz）**

| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |
| --- | --- | --- | --- |
| FL | 0.0 | 0 | 0.00 |
| FR | 0.0 | 0 | 0.00 |
| SL | 14.0 | 675 | 14.06 |
| SR | 17.0 | 819 | 17.06 |
| HFL | 17.0 | 821 | 17.10 |
| HFR | 19.0 | 917 | 19.10 |
| SUB1 | 0.0 | 276 | 5.75 |

**粉噪倍频带电平（dB）**

| 频带(Hz) | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 32 | -60.9 | -61.3 | -75.1 | -75.1 | -179.5 | -179.6 | -34.8 |
| 63 | -41.4 | -41.3 | -52.2 | -52.4 | -154.7 | -155.1 | -38.8 |
| 125 | -34.5 | -34.0 | -42.2 | -41.8 | -131.6 | -131.2 | -50.2 |
| 250 | -33.2 | -32.8 | -39.6 | -39.4 | -107.7 | -107.6 | -71.3 |
| 500 | -33.2 | -32.6 | -39.6 | -39.2 | -84.4 | -83.9 | -95.1 |
| 1000 | -33.0 | -32.6 | -39.4 | -39.1 | -62.6 | -62.4 | -119.2 |
| 2000 | -32.9 | -32.5 | -39.4 | -39.1 | -51.0 | -50.7 | -143.1 |
| 4000 | -33.0 | -32.6 | -40.4 | -40.0 | -48.5 | -48.1 | -167.2 |
| 8000 | -33.0 | -32.5 | -47.0 | -46.7 | -50.7 | -50.3 | -182.3 |
| 16000 | -33.0 | -32.5 | -70.9 | -70.4 | -66.0 | -65.5 | -179.9 |

**通道相关性矩阵**（粉噪立体声，2–3.5s）

| | FL | FR | SL | SR | HFL | HFR | SUB1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| FL | +1.00 | -0.01 | +0.00 | -0.01 | -0.01 | +0.00 | +0.00 |
| FR | -0.01 | +1.00 | +0.00 | -0.01 | -0.00 | +0.01 | +0.00 |
| SL | +0.00 | +0.00 | +1.00 | +0.06 | -0.00 | -0.00 | +0.00 |
| SR | -0.01 | -0.01 | +0.06 | +1.00 | +0.02 | -0.00 | +0.00 |
| HFL | -0.01 | -0.00 | -0.00 | +0.02 | +1.00 | +0.01 | +0.00 |
| HFR | +0.00 | +0.01 | -0.00 | -0.00 | +0.01 | +1.00 | +0.00 |
| SUB1 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 |

**与 FL 的互相关（粉噪，±50ms）**

| 角色 | 延迟(ms) | r(lag0) | max\|r\| | 峰值滞后(ms) |
| --- | --- | --- | --- | --- |
| FR | 0.0 | -0.009 | 0.024 | 23.8 |
| SL | 14.0 | +0.001 | 0.776 | -14.1 |
| SR | 17.0 | -0.008 | 0.361 | -17.1 |
| HFL | 17.0 | -0.007 | 0.536 | -17.1 |
| HFR | 19.0 | +0.001 | 0.168 | -19.1 |
| SUB1 | 0.0 | +0.027 | 0.027 | -0.1 |

### upmix-5.3.2-movie（5.3.2，10ch）

**电平（RMS dBFS）**

| 信号 | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| tone_center_1k | -15.0 | -15.0 | -18.0 | -30.0 | -30.0 | -102.8 | -102.8 | -102.8 | -42.7 | -42.7 |
| tone_left_1k | -15.0 | -240.0 | -24.0 | -22.1 | -26.6 | -108.8 | -108.8 | -108.8 | -46.5 | -51.9 |
| tone_antiphase_1k | -15.0 | -15.0 | -240.0 | -18.0 | -18.0 | -240.0 | -240.0 | -240.0 | -53.2 | -53.2 |
| sine_40hz_center | -39.6 | -39.6 | -73.9 | -62.0 | -62.0 | -15.5 | -15.5 | -15.5 | -153.1 | -153.1 |

**脉冲延迟（samples @ 48000 Hz）**

| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |
| --- | --- | --- | --- |
| FL | 0.0 | 0 | 0.00 |
| FR | 0.0 | 0 | 0.00 |
| C | 0.0 | 2 | 0.04 |
| SL | 18.0 | 867 | 18.06 |
| SR | 18.0 | 867 | 18.06 |
| SUB1 | 0.0 | 276 | 5.75 |
| SUB2 | 0.0 | 276 | 5.75 |
| SUB3 | 0.0 | 276 | 5.75 |
| HFL | 20.0 | 965 | 20.10 |
| HFR | 20.0 | 965 | 20.10 |

**粉噪倍频带电平（dB）**

| 频带(Hz) | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 32 | -60.9 | -61.3 | -98.1 | -74.3 | -74.4 | -34.8 | -34.8 | -34.8 | -176.8 | -176.9 |
| 63 | -41.4 | -41.3 | -73.9 | -51.4 | -51.5 | -38.8 | -38.8 | -38.8 | -152.2 | -152.5 |
| 125 | -34.5 | -34.0 | -53.1 | -41.4 | -41.1 | -50.2 | -50.2 | -50.2 | -128.9 | -128.6 |
| 250 | -33.2 | -32.8 | -41.9 | -38.8 | -38.6 | -71.3 | -71.3 | -71.3 | -105.0 | -105.0 |
| 500 | -33.2 | -32.6 | -39.2 | -38.8 | -38.5 | -95.1 | -95.1 | -95.1 | -81.7 | -81.4 |
| 1000 | -33.0 | -32.6 | -38.9 | -38.6 | -38.4 | -119.2 | -119.2 | -119.2 | -60.6 | -60.5 |
| 2000 | -32.9 | -32.5 | -38.7 | -38.6 | -38.4 | -143.1 | -143.1 | -143.1 | -50.1 | -49.8 |
| 4000 | -33.0 | -32.6 | -38.9 | -39.2 | -38.9 | -167.2 | -167.2 | -167.2 | -48.0 | -47.7 |
| 8000 | -33.0 | -32.5 | -39.9 | -44.0 | -43.7 | -182.3 | -182.3 | -182.3 | -50.2 | -49.9 |
| 16000 | -33.0 | -32.5 | -50.5 | -65.0 | -64.7 | -179.9 | -179.9 | -179.9 | -65.5 | -65.1 |

**通道相关性矩阵**（粉噪立体声，2–3.5s）

| | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| FL | +1.00 | -0.01 | +0.33 | +0.00 | -0.00 | +0.03 | +0.03 | +0.03 | +0.00 | +0.00 |
| FR | -0.01 | +1.00 | +0.36 | +0.01 | -0.01 | +0.03 | +0.03 | +0.03 | +0.00 | -0.00 |
| C | +0.33 | +0.36 | +1.00 | -0.00 | +0.01 | -0.00 | -0.00 | -0.00 | +0.00 | +0.00 |
| SL | +0.00 | +0.01 | -0.00 | +1.00 | -0.88 | -0.00 | -0.00 | -0.00 | -0.00 | +0.00 |
| SR | -0.00 | -0.01 | +0.01 | -0.88 | +1.00 | +0.00 | +0.00 | +0.00 | +0.00 | -0.00 |
| SUB1 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | -0.00 |
| SUB2 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | -0.00 |
| SUB3 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | -0.00 |
| HFL | +0.00 | +0.00 | +0.00 | -0.00 | +0.00 | -0.00 | -0.00 | -0.00 | +1.00 | +0.83 |
| HFR | +0.00 | -0.00 | +0.00 | +0.00 | -0.00 | -0.00 | -0.00 | -0.00 | +0.83 | +1.00 |

**与 FL 的互相关（粉噪，±50ms）**

| 角色 | 延迟(ms) | r(lag0) | max\|r\| | 峰值滞后(ms) |
| --- | --- | --- | --- | --- |
| FR | 0.0 | -0.009 | 0.024 | 23.8 |
| C | 0.0 | +0.333 | 0.468 | -0.0 |
| SL | 18.0 | +0.006 | 0.741 | -18.1 |
| SR | 18.0 | -0.004 | 0.435 | -18.1 |
| SUB1 | 0.0 | +0.027 | 0.027 | -0.1 |
| SUB2 | 0.0 | +0.027 | 0.027 | -0.1 |
| SUB3 | 0.0 | +0.027 | 0.027 | -0.1 |
| HFL | 20.0 | +0.003 | 0.495 | -20.1 |
| HFR | 20.0 | +0.002 | 0.255 | -20.1 |

### upmix-5.3.2-music（5.3.2，10ch）

**电平（RMS dBFS）**

| 信号 | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| tone_center_1k | -15.0 | -15.0 | -21.0 | -240.0 | -240.0 | -102.8 | -102.8 | -102.8 | -51.4 | -51.4 |
| tone_left_1k | -15.0 | -240.0 | -27.0 | -27.0 | -27.0 | -108.8 | -108.8 | -108.8 | -57.4 | -57.4 |
| tone_antiphase_1k | -15.0 | -15.0 | -240.0 | -21.0 | -21.0 | -240.0 | -240.0 | -240.0 | -240.0 | -240.0 |
| sine_40hz_center | -39.6 | -39.6 | -76.9 | -240.0 | -240.0 | -15.5 | -15.5 | -15.5 | -162.4 | -162.4 |

**脉冲延迟（samples @ 48000 Hz）**

| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |
| --- | --- | --- | --- |
| FL | 0.0 | 0 | 0.00 |
| FR | 0.0 | 0 | 0.00 |
| C | 0.0 | 2 | 0.04 |
| SUB1 | 0.0 | 276 | 5.75 |
| SUB2 | 0.0 | 276 | 5.75 |
| SUB3 | 0.0 | 276 | 5.75 |
| HFL | 18.0 | 869 | 18.10 |
| HFR | 18.0 | 869 | 18.10 |

**粉噪倍频带电平（dB）**

| 频带(Hz) | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 32 | -60.9 | -61.3 | -101.1 | -77.7 | -77.7 | -34.8 | -34.8 | -34.8 | -186.6 | -186.6 |
| 63 | -41.4 | -41.3 | -76.9 | -54.7 | -54.7 | -38.8 | -38.8 | -38.8 | -162.2 | -162.2 |
| 125 | -34.5 | -34.0 | -56.1 | -44.5 | -44.5 | -50.2 | -50.2 | -50.2 | -138.5 | -138.5 |
| 250 | -33.2 | -32.8 | -44.9 | -42.0 | -42.0 | -71.3 | -71.3 | -71.3 | -114.7 | -114.7 |
| 500 | -33.2 | -32.6 | -42.2 | -42.0 | -42.0 | -95.1 | -95.1 | -95.1 | -91.2 | -91.2 |
| 1000 | -33.0 | -32.6 | -41.9 | -41.7 | -41.7 | -119.2 | -119.2 | -119.2 | -68.7 | -68.7 |
| 2000 | -32.9 | -32.5 | -41.7 | -41.8 | -41.8 | -143.1 | -143.1 | -143.1 | -55.0 | -55.0 |
| 4000 | -33.0 | -32.6 | -41.9 | -42.7 | -42.7 | -167.2 | -167.2 | -167.2 | -51.4 | -51.4 |
| 8000 | -33.0 | -32.5 | -42.9 | -49.3 | -49.3 | -182.3 | -182.3 | -182.3 | -53.4 | -53.4 |
| 16000 | -33.0 | -32.5 | -53.5 | -73.2 | -73.2 | -179.9 | -179.9 | -179.9 | -68.7 | -68.7 |

**通道相关性矩阵**（粉噪立体声，2–3.5s）

| | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| FL | +1.00 | -0.01 | +0.33 | +0.01 | -0.01 | +0.03 | +0.03 | +0.03 | +0.00 | +0.00 |
| FR | -0.01 | +1.00 | +0.36 | -0.01 | +0.01 | +0.03 | +0.03 | +0.03 | +0.01 | +0.01 |
| C | +0.33 | +0.36 | +1.00 | +0.00 | -0.00 | -0.00 | -0.00 | -0.00 | +0.00 | +0.00 |
| SL | +0.01 | -0.01 | +0.00 | +1.00 | -1.00 | -0.00 | -0.00 | -0.00 | -0.00 | -0.00 |
| SR | -0.01 | +0.01 | -0.00 | -1.00 | +1.00 | +0.00 | +0.00 | +0.00 | +0.00 | +0.00 |
| SUB1 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | -0.00 |
| SUB2 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | -0.00 |
| SUB3 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | -0.00 |
| HFL | +0.00 | +0.01 | +0.00 | -0.00 | +0.00 | -0.00 | -0.00 | -0.00 | +1.00 | +1.00 |
| HFR | +0.00 | +0.01 | +0.00 | -0.00 | +0.00 | -0.00 | -0.00 | -0.00 | +1.00 | +1.00 |

**与 FL 的互相关（粉噪，±50ms）**

| 角色 | 延迟(ms) | r(lag0) | max\|r\| | 峰值滞后(ms) |
| --- | --- | --- | --- | --- |
| FR | 0.0 | -0.009 | 0.024 | 23.8 |
| C | 0.0 | +0.333 | 0.468 | -0.0 |
| SL | 15.0 | -0.001 | 0.598 | -15.1 |
| SR | 15.0 | +0.001 | 0.598 | -15.1 |
| SUB1 | 0.0 | +0.027 | 0.027 | -0.1 |
| SUB2 | 0.0 | +0.027 | 0.027 | -0.1 |
| SUB3 | 0.0 | +0.027 | 0.027 | -0.1 |
| HFL | 18.0 | +0.003 | 0.382 | -18.1 |
| HFR | 18.0 | +0.003 | 0.382 | -18.1 |

### upmix-5.3.2-wide（5.3.2，10ch）

**电平（RMS dBFS）**

| 信号 | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| tone_center_1k | -15.0 | -15.0 | -19.5 | -28.0 | -28.0 | -102.8 | -102.8 | -102.8 | -45.7 | -45.7 |
| tone_left_1k | -15.0 | -240.0 | -25.5 | -22.4 | -28.9 | -108.8 | -108.8 | -108.8 | -48.2 | -57.7 |
| tone_antiphase_1k | -15.0 | -15.0 | -240.0 | -19.0 | -19.0 | -240.0 | -240.0 | -240.0 | -51.7 | -51.7 |
| sine_40hz_center | -39.6 | -39.6 | -75.4 | -60.0 | -60.0 | -15.5 | -15.5 | -15.5 | -156.3 | -156.3 |

**脉冲延迟（samples @ 48000 Hz）**

| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |
| --- | --- | --- | --- |
| FL | 0.0 | 0 | 0.00 |
| FR | 0.0 | 0 | 0.00 |
| C | 0.0 | 2 | 0.04 |
| SL | 14.0 | 675 | 14.06 |
| SR | 17.0 | 819 | 17.06 |
| SUB1 | 0.0 | 276 | 5.75 |
| SUB2 | 0.0 | 276 | 5.75 |
| SUB3 | 0.0 | 276 | 5.75 |
| HFL | 17.0 | 821 | 17.10 |
| HFR | 19.0 | 917 | 19.10 |

**粉噪倍频带电平（dB）**

| 频带(Hz) | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 32 | -60.9 | -61.3 | -99.6 | -75.1 | -75.1 | -34.8 | -34.8 | -34.8 | -179.5 | -179.6 |
| 63 | -41.4 | -41.3 | -75.4 | -52.2 | -52.4 | -38.8 | -38.8 | -38.8 | -154.7 | -155.1 |
| 125 | -34.5 | -34.0 | -54.6 | -42.2 | -41.8 | -50.2 | -50.2 | -50.2 | -131.6 | -131.2 |
| 250 | -33.2 | -32.8 | -43.4 | -39.6 | -39.4 | -71.3 | -71.3 | -71.3 | -107.7 | -107.6 |
| 500 | -33.2 | -32.6 | -40.7 | -39.6 | -39.2 | -95.1 | -95.1 | -95.1 | -84.4 | -83.9 |
| 1000 | -33.0 | -32.6 | -40.4 | -39.4 | -39.1 | -119.2 | -119.2 | -119.2 | -62.6 | -62.4 |
| 2000 | -32.9 | -32.5 | -40.2 | -39.4 | -39.1 | -143.1 | -143.1 | -143.1 | -51.0 | -50.7 |
| 4000 | -33.0 | -32.6 | -40.4 | -40.4 | -40.0 | -167.2 | -167.2 | -167.2 | -48.5 | -48.1 |
| 8000 | -33.0 | -32.5 | -41.4 | -47.0 | -46.7 | -182.3 | -182.3 | -182.3 | -50.7 | -50.3 |
| 16000 | -33.0 | -32.5 | -52.0 | -70.9 | -70.4 | -179.9 | -179.9 | -179.9 | -66.0 | -65.5 |

**通道相关性矩阵**（粉噪立体声，2–3.5s）

| | FL | FR | C | SL | SR | SUB1 | SUB2 | SUB3 | HFL | HFR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| FL | +1.00 | -0.01 | +0.33 | +0.00 | -0.01 | +0.03 | +0.03 | +0.03 | -0.01 | +0.00 |
| FR | -0.01 | +1.00 | +0.36 | +0.00 | -0.01 | +0.03 | +0.03 | +0.03 | -0.00 | +0.01 |
| C | +0.33 | +0.36 | +1.00 | +0.01 | -0.01 | -0.00 | -0.00 | -0.00 | -0.01 | +0.00 |
| SL | +0.00 | +0.00 | +0.01 | +1.00 | +0.06 | -0.00 | -0.00 | -0.00 | -0.00 | -0.00 |
| SR | -0.01 | -0.01 | -0.01 | +0.06 | +1.00 | +0.00 | +0.00 | +0.00 | +0.02 | -0.00 |
| SUB1 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | +0.00 |
| SUB2 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | +0.00 |
| SUB3 | +0.03 | +0.03 | -0.00 | -0.00 | +0.00 | +1.00 | +1.00 | +1.00 | -0.00 | +0.00 |
| HFL | -0.01 | -0.00 | -0.01 | -0.00 | +0.02 | -0.00 | -0.00 | -0.00 | +1.00 | +0.01 |
| HFR | +0.00 | +0.01 | +0.00 | -0.00 | -0.00 | +0.00 | +0.00 | +0.00 | +0.01 | +1.00 |

**与 FL 的互相关（粉噪，±50ms）**

| 角色 | 延迟(ms) | r(lag0) | max\|r\| | 峰值滞后(ms) |
| --- | --- | --- | --- | --- |
| FR | 0.0 | -0.009 | 0.024 | 23.8 |
| C | 0.0 | +0.333 | 0.468 | -0.0 |
| SL | 14.0 | +0.001 | 0.776 | -14.1 |
| SR | 17.0 | -0.008 | 0.361 | -17.1 |
| SUB1 | 0.0 | +0.027 | 0.027 | -0.1 |
| SUB2 | 0.0 | +0.027 | 0.027 | -0.1 |
| SUB3 | 0.0 | +0.027 | 0.027 | -0.1 |
| HFL | 17.0 | -0.007 | 0.536 | -17.1 |
| HFR | 19.0 | +0.001 | 0.168 | -19.1 |

## 结论

- 所有信号级检查通过：CamillaDSP 实际输出与参考 DSP 仿真逐样本一致（max|diff| ~1e-8，float32 量化级），说明配置/滤波器/延迟/矩阵与设计一致。
- 独立生成的 CamillaDSP 配置与 tspi-channelmap 渲染路径（修复后）输出逐样本相同。
- 电平、频带、延迟、反相/差信号隔离均符合设计；未发现非预期的梳状/抵消（r(lag0)≈0，合成通道与前声道的最大相关出现在配置延迟处）。
- 已知设计取舍：SL/SR 为反相差信号（相关 -1）、HFL/HFR 为单声道高度（相关 +1），需要扩散感请用 wide 预设（左右延迟差）或 decor.mode=allpass。

## 2. 计时 / 实时因子（本机文件后端）

| preset | variant | 平均 RTF | 最小 RTF | 平均耗时(s) | 音频时长(s) |
| --- | --- | --- | --- | --- | --- |
| upmix-4.1.2-movie | renderer | 116.6 | 64.0 | 0.05 | 7.3 |
| upmix-4.1.2-movie | standalone | 117.3 | 62.6 | 0.05 | 7.3 |
| upmix-4.1.2-music | renderer | 113.3 | 64.1 | 0.06 | 7.3 |
| upmix-4.1.2-music | standalone | 112.7 | 51.1 | 0.06 | 7.3 |
| upmix-4.1.2-wide | renderer | 50.8 | 1.4 | 0.99 | 7.3 |
| upmix-4.1.2-wide | standalone | 50.3 | 0.8 | 2.09 | 7.3 |
| upmix-5.3.2-movie | renderer | 48.4 | 5.2 | 0.23 | 7.3 |
| upmix-5.3.2-movie | standalone | 42.0 | 7.0 | 0.24 | 7.3 |
| upmix-5.3.2-music | renderer | 87.6 | 4.4 | 0.12 | 7.3 |
| upmix-5.3.2-music | standalone | 83.7 | 3.9 | 0.14 | 7.3 |
| upmix-5.3.2-wide | renderer | 21.4 | 4.1 | 0.43 | 7.3 |
| upmix-5.3.2-wide | standalone | 20.3 | 4.9 | 0.40 | 7.3 |

说明：RTF = 音频时长 / 实际处理耗时，越大越快；文件后端不含 ALSA 实时等待，板端需乘实际负载系数（见板端测试一节）。
