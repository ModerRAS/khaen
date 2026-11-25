# calibrate/ 复审 Round 3（P0 修复验收，2026-11-15）

> 对象：audio-stack Round-2 P0 修复版；release @target/release/khaen-calibrate.exe@
> sha256=@e364fee99bb8189046c5054e80300fb848d11fea4005ca0ed6cb5ff285bdc6e7@（与声明一致，1,682,432 B）。
> 方法：只读 review + release 二进制复现 + 独立重建 FR 复频响跑我的 pair checker；未改 calibrate/。
> 上轮：@CALIBRATE-ROUND2-REVIEW.md@；契约/验收：@../CONTRACT.md@ / @../ACCEPTANCE.md@。

## 0. 结论

**Round-2 的 4 项 P0 均已按要求修复，复审通过（P0 关闭）**：
极性验证改校准字段、arecord period/buffer、measure-multi 逐槽检测起点、FR re/im+phase_ref。
cargo test 11/11；示例 YAML 通过 validator；外部 checker 可用导出数据独立复算并与其 pairs.json 一致。
剩余 P1 均为“契约/报告/产物质量”项（不阻塞板端 T1/T4，但建议板端前顺手修）。

## 1. P0 关闭证据

| P0 | 状态 | 验证方式与结果 |
| --- | --- | --- |
| P0-2 极性只取校准字段 | ✅ 关闭 | @phase_sum_margin@/@check_pair@ 均改为 @cal_pols@（@main.rs:208,361,523@）；复现：hand（phase0+polarity-1）→ pair **FAIL**；hand180（phase180+polarity-1）→ **PASS**；新 simulate 反相 2ch → **PASS**。N1 单测改为真实数据形态：实测 phase180 + 校准 normal → FAIL、校准 invert → PASS（@main.rs:731-738@），11/11 通过 |
| P0-1 arecord period/buffer | ✅ 关闭 | @raw.rs@ arecord/aplay 均带 @--period-size/--buffer-size@；measure/measure-multi 范围校验 32..4096、buffer≥period（实测非法 period=8 → rc=1） |
| P0-3 measure-multi 切窗 | ✅ 关闭（待板端验证） | @main.rs:615-640@：名义 @pre+i*slot+playback-offset(默认0.35s)@，±0.25s 窗口互相关检测实际起点，@start=wlo+lag@，尾部 @start+sweep+1s@，日志打印 nominal/actual_start/lag；不再固定 0.2s/0.5s。板端 T1 仍需实测（含不同 USB 启动延迟） |
| P0-5 UMIK 48k | ✅ 关闭 | measure/measure-multi 对 @--mic umik@ 且 rate≠48000 报错（实测 rc=1） |
| 外部验收（上轮建议） | ✅ 关闭 | @fr.csv@ 头含 phase_ref，新增 re/im；analysis JSON 含 @phase_ref@；实测 re/im 与 db 最大偏差 6e-6、phase 与 atan2 偏差 ≤0.005°。按 @absolute_phase = phase - 360 f delay/fs@ 重建 FL/FR 后跑独立 checker：Δφ(fc)=-15.0°、CD(fc)=-0.07dB、minCD=-0.30dB、notch=5.58dB、ripple=0.37dB，与其 pairs.json（-15.44°/-0.079/-0.301/5.45/0.51）一致 |

## 2. 仍开放的 P1（不阻塞板端，建议板端前小改）

- @main.rs:337@ @reference_index = i@（文件循环下标）仍非 @channels[k].index@（m.channel）；缺通道/乱序会错。
- @main.rs:258@ fit 单通道 @delay_samples@ 未归一化（单通道文件 min 应为 0）；单独使用会被 validator 判非法。
- @main.rs:245/335/465/641@ 未给 @--names@ 时默认名仍 @ch{index}@ 小写，不符合角色 id 正则。
- @peq.rs:13@ Biquad clamp 仍 freq 10..22000、q≤20，超出契约（freq≥20、q≤10）；拟合输出可能被 validator 拒绝。
- @main.rs:268-308@ CamillaDSP 片段极性仍只有 @channelmap_roles@ 提示，未在 filters/pipeline 内实现反相。
- @main.rs:382@ 报告仍 @khaen-calibration-validation/v1@，与 ACCEPTANCE §8 @khaen-calibration-report/v1@（pairs/负向用例/环境/文件 sha256）不一致。
- @main.rs:551-556@ ripple 注释写 1/3 oct，实际 5 点滑动（≈0.2 oct）；建议按 1/3 oct 平滑。
- @main.rs:567-568@ @target_dev@、@level_diff@ 是 ACCEPTANCE 未要求的额外硬 FAIL 条件（level_diff 对交叉点两侧设计电平不同的系统可能误报）；建议降为 warning 或可配置。
- @main.rs:394@ @--pairs@ 仍可选：未给时只做全局 margin，覆盖不了 §6.1；建议给了 @--crossovers@ 就强制/自动生成 pairs（至少文档要求必填）。
- @main.rs:640@ measure-multi 的 nominal/actual_start/lag 只打印到 stdout，未写入 analysis JSON；T5/T1 报告需要可追溯的 period/buffer/起点，建议入 JSON。
- @dsp.rs:171-173@ SNR 仍 IR 尾部粗估，未做 200Hz–8kHz 带限与 ≥3 次重复/中位数（T2）。

## 3. 板端验收建议（下一步）

1. 先跑 T1（MOTU 1:1 电学环回，UMIK 不参与极性绝对参考）：
   @measure-multi --channels 0,1,2,3 --verify-mapping --polarity-offset ...@，核对每槽 actual_start 与 lag、逐通道映射、延迟/极性误差（±0.5 sample/100%）。
2. 再跑 T4：真实扬声器布局（4.1.2/5.3.2）逐通道测量 → merge --pairs（每分频点）→ 用导出的 fr.csv+analysis JSON 独立复算（我的 checker 可直接用绝对相位重建结果）。
3. T5：64/128 帧各一轮播放/测量，记录 period/buffer 与 xrun；当前 arecord 已参数化，需在板端确认设备接受。
4. 报告按 ACCEPTANCE §8 统一 schema；P1 四项（reference_index、fit 归一化、角色名、PEQ clamp）建议板端前修掉，避免 YAML 被 validator 拒。

## 4. 本轮复审用到的独立验证

- validator：@examples/offline/calibration.yml@、新 simulate 产物 → 0 error/0 warning。
- 独立 pair checker（calibration-spec/tools/check_pair_response.py）：由 fr.csv+analysis.json 重建绝对相位后复算 FL/FR@2000，指标与工具内 pairs.json 一致。
- 复现目录：@scratch/calib-verify/hand@、@hand180@、@siminv2@、@fr-recon@。
