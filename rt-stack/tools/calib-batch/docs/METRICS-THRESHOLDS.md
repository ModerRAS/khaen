# 批量闭环观察指标与阈值（不是验收门）

> task-201 · 2026-11-17 · 变更必须升版本并在证据里标注；阈值只用于**离线自测与现场观察定位**。
> 验收权威：`rt-stack/spec/mimo-t6.md`（T6-M）与 `calibration-spec/ACCEPTANCE.md`（T6-cal）。
> 用户要求第 29 条明确：3 dB/15% 也只是工程观察指标，不是通过标准。

## 1. 信号链与预处理

1. 激励：指数扫频（Farina），冻结参数（start/end Hz、秒数、fade、电平、重复间隔）。
2. 采集：`tspi-calibrate measure-multi` 单次连续 arecord+playback（S24_3LE 裸流）；
   每点位一个 capture.raw，内含全部通道 × repeats 的 slot 时间线（pre + n*slot + post + 2 s）。
3. 起点：互相关在冻结 lead-in ±0.25 s 内定位实际起点（不接受相邻 repeat 的同等峰）。
4. IR：Farina 反卷积；每通道 repeats 取**中位数 IR**；IR 窗必须短于 repeat 间隔（防止
   把下一遍 sweep 的直达峰卷进来），上限 = min(配置 ir_seconds, interval-0.05 s)。
5. 质量门（观察门，不等同 T6 硬门）：SNR >= 25 dB、repeat delay 散差 <= 0.5 sample、
   repeat FR 散差 <= 0.8 dB（1/6 oct）；任一失败该通道/采集标 invalid。

## 2. 指标定义

| 指标 | 定义 | 用途 |
|---|---|---|
| FR | IR 的 1/6 oct 平滑幅频（1 kHz 归一），输出 1/24 oct 网格 | 每通道频响、高带/模态带偏差 |
| 瀑布/CSD 观察量 | 1/3 oct 带通 IR 的 Schroeder EDC 在 0/20/40/60/80/100/150/200/300/400 ms 的相对电平（dB，0=起点） | 余振/衰减随时间的形状 |
| EDC | Schroeder 反向累积能量 E(t)=∫_t^∞ h²(τ)dτ，归一化 0 dB | RT 与余振基础 |
| RT20 -> RT60 | EDC 在 -5 dB..-25 dB 区间线性拟合斜率 s(dB/s)，RT60 = -60/s；要求 R²>=0.90 且 EDC 动态范围够 | 混响时间 |
| EDC@100ms | EDC 在 100 ms 的读数（越负 = 余振越小） | "校准后余振是否变小"的直接观察量 |
| 座间 sigma | 同一频带/指标在 design 各点位之间的标准差 | 多位置一致性（只报不设闸，MIMO-ROADMAP §5.2） |
| 模态带 | 40/50/63/80/100/125/160/200/250 Hz（1/3 oct） | MIMO/模态均衡效果只在 20-300 Hz 声称 |
| 模态 FR 偏差 | 40-200 Hz 相对 200-500 Hz 均值的 RMS 偏差（每通道，design 平均） | 低频峰谷是否被压平 |
| 高带平坦度 | 300-10000 Hz 幅频相对自身均值的 std | 校正是否让中高带更平（而非整带平移） |
| holdout 一致性 | holdout 的模态 RT 改善 >= design 改善的 50%（同方向） | 泛化观察；不一致提示过拟合/位置依赖 |

## 3. 分类规则（观察，不是 PASS/FAIL）

设每模态带：相对变化 pc = (after-before)/|before|。

- **IMPROVED**：RT pc <= -10% 的模态带 >= 3 且 EDC@100ms 改善 >= 1 dB 的带 >= 2；
- **WORSE**：RT pc >= +5% 的模态带 >= 3 或 EDC@100ms 恶化 >= 1 dB 的带 >= 2；
- **FLAT**：不满足以上两类（局部小改善不改判）；
- **INSUFFICIENT**：有效采集 >20% 无效 / 关键采集缺失 / 无 RT 有效值。

常量位于 `calbatch/analysis.py` 的 `TH` 字典：
`rt_improve_frac=0.10`、`rt_worsen_frac=0.05`、`residual_improve_db=1.0`、
`residual_worsen_db=1.0`、`sigma_improve_db=0.5`、`fr_guard_worsen_db=1.0`、
`high_guard_worsen_db=1.0`、`improve_min_bands=3`、`improve_min_residual_bands=2`、
`worsen_min_bands=3`、`holdout_consistency_ratio=0.5`、`invalid_capture_ratio=0.2`。

## 4. 问题定位规则（供算法迭代）

按影响排序取前 20 条，字段：kind / band_hz / channel / metric / before / after / delta。

| kind | 触发 | 说明 |
|---|---|---|
| `modal_rt_worsened` | 模态带 RT 恶化 >=5% | 指出频带，优先查该带 MIMO/PEQ |
| `modal_residual_worsened` | 模态带 EDC@100ms 恶化 >=1 dB | 余振变大 |
| `channel_rt_worsened` | 单通道模态 RT 恶化 >=10% | 指出通道 |
| `modal_fr_dev_worsened` | 40-200 Hz FR 偏差恶化 >=1 dB | 低频峰谷/极性/延迟问题 |
| `high_band_flatness_worsened` | 300-10k 平坦度恶化 >=1 dB | 中高带校正副作用 |

## 5. 使用边界

- 单房间/单布局（4.1.2）/单麦型号（UMIK-1）样本；不得外推。
- MIMO 效果只在 20-300 Hz 声称；>300 Hz 只作 guard/观察。
- pilot（三麦固定）数据只作相对/排障，不进入 design/holdout/MIMO 结论，不得用于 T67/T6-M。
- 合成数据（`simulated: true`）只验证流水线可跑通与指标机器可判，不证明算法有效。
- 真实采集缺失时输出 INSUFFICIENT，不补数、不推断。
