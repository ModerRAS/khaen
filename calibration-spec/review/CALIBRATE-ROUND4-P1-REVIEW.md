# calibrate/ P1 批量修复复审（Round 4，2026-11-15）

> 对象：audio-stack P1 批量修复版；release sha256=@1f22a410a13a2d245780d20b975658a8a0e4d9e1ad322dd934340bf1fe5a4ce7@（1,683,456 B）。
> 只读 review + release 二进制回归；未改 calibrate/。上轮：@CALIBRATE-ROUND3-REVIEW.md@。

## 0. 结论

P1 批量修复 5 项中 **4 项已验证关闭**；**CamillaDSP 片段极性实现仍不完整**（Gain filter 定义了但没接入 pipeline，
纯反相通道的 pipeline 为空；且与 channelmap_roles 同时存在双反相风险）。另 PEQ 32 段上限未在代码里 clamp。
其余剩余项（1/3oct、pairs 自动、诊断进 JSON、带限 SNR、report/v1）audio-stack 已列为板端前再补。

## 1. P1 核验结果

| 项 | 状态 | 证据 |
| --- | --- | --- |
| reference_index 用 channels[].index | ✅ | @main.rs:340@ @reference_index = m.channel@ |
| fit 单通道 delay=0 | ✅ | @main.rs:258@ @delay_samples: 0@ + 注释“单通道 fit 为中间产物” |
| 默认名 CH<n> | ✅（merge/fit） | @main.rs:245/338@ @CH{channel}@；measure-multi/simulate 的 analysis 文件前缀仍 @chN@（不进入最终 YAML，P3） |
| PEQ clamp 契约内 | ✅ 部分 | @peq.rs:13@ freq 20..20000、q 0.1..10、gain ±18（比契约 ±24 更严，合法）；但 @--max-stages@ 未 clamp 到 32（@main.rs:236/313@ 直接传 fit_peq），>32 段仍可能生成非法 YAML |
| CamillaDSP 片段 Gain 反相 | ❌ 未接入 pipeline | 见下 |

## 2. 关键问题：CamillaDSP 片段的极性 filter 定义了但没被 pipeline 引用

@main.rs:274-310@：pipeline 在 294-296 行先 push（此时 names 只含 Delay/PEQ），之后 297-302 行才把
@cal_chN_pol@ 加进 names —— 但 pipeline Value 已经生成，极性 filter 不会出现在 pipeline 里。

复现（release 二进制，hand180 目录）：
```
filters:
  cal_ch1_pol: {type: Gain, parameters: {gain: 0.0, inverted: true}}
pipeline: []          # 纯反相通道没有任何 Filter step → 独立 CamillaDSP 消费时不反相
channelmap_roles: [{index: 1, inverted: true}]
```
即使有 Delay/PEQ 的通道，pipeline 也只列 delay/peq，不列 pol。

另外语义要二选一：片段同时给了 @pipeline@ 里的 Gain（修好后）和 @channelmap_roles[].inverted@。
- 独立 CamillaDSP 配置消费：应走 pipeline Gain；
- channelmap 渲染器消费：走 roles sources[].inverted；
两者同时应用会**双反相**。建议：片段里只实现一种（推荐 standalone 用 Gain），channelmap_roles 改为
advisory（例如加 @"apply": false@ 或明确 note “二选一，勿同时应用”），并在 README/CONTRACT 写明消费路径。

修法：把 297-302 的极性块移到 294-296 的 pipeline push 之前；对“只有反相、无 delay/PEQ”的通道也要生成
pipeline（当前 @if !names.is_empty()@ 会跳过）。单测补：invert-only 通道的 fragment.pipeline 含 Gain 名。

## 3. 其余观察

- @--max-stages@ 建议 @max_stages.min(32)@（契约 ≤32），或在 validator 之外再加一层防御。
- 新示例 @examples/offline{,-invert}/calibration.yml@ 仍通过我的 validator（0 error/0 warning）。
- 回归：hand（phase0+polarity-1）→ FAIL、hand180（phase180+polarity-1）→ PASS；新 simulate 反相+3.5 sample 延迟
  → pair PASS，全局 worst -1.58 dB @19897 Hz（整数归一化后 0.5 sample 残差在 20 kHz 的预期部分抵消，非回归）。
- cargo test 日志 11/11。

## 4. 建议

1. 修 CamillaDSP 片段：极性块前置 + invert-only 也进 pipeline；只保留一种消费路径（roles 标注 advisory）。
2. @max_stages@ clamp 32。
3. 剩余项按 audio-stack 计划（1/3oct、pairs 自动、诊断进 JSON、带限 SNR、report/v1）板端前补。
4. 板端 T1/T4/T5 数据出来后可发我跑独立 checker 复核。
