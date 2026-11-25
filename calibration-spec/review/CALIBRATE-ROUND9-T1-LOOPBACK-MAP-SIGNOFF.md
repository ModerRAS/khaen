# T1 非 1:1 环回（loopback-map）签收（Round-9，2026-11-15）

> release sha256=@490448857e110e6ed7b260c39bd83f62ea0704c977dc30c8ae803de885b3201a@（1,745,408 B）；
> cargo test 11/11；只读 review + release 二进制，未改 calibrate/。

## 结论：T1 非 1:1 回环（out9→in7）实现正确，T1 partial 方案可执行；设计侧 ACCEPTANCE 已更新为支持任意 loopback-map。

## 1. 实现核验

- @--loopback-map 9:7@（1-based，可多对，如 @9:7,1:1@；@--loopback-channels 1,2@ 保留为 1:1 简写）：
  - 解析 @play:capture@，0/非法格式报错（实测 @0:7@ rc=1、格式错误有明确提示）；
  - loopback 模式**强制** @--verify-mapping@（否则映射/隔离不校验，结果无效）；
  - 映射校验：播放 outN 的最大能量必须出现在指定 inM，否则 FAIL；隔离：其它采集通道相对指定 inM ≤ -60 dB；
  - diag.json 记 @loopback_map_1based@、@loopback_mode@、@isolation_other_max_db@、@capture_channel_analyzed@；
  - 默认名 @OUT9_IN7@；usage 有 --loopback-map 示例。
- capture-channels 下限：@max(capture_1based)@ 即可（in7 → 需 ≥7 通道）；实测 @--loopback-map 9:7 --capture-channels 7@ 通过参数校验（板端 runbook 用 8 也合法）。
- 回归：cargo test 11/11；示例 validator 0/0；hand→FAIL、hand180→PASS。

### 小问题（P2，板端前建议修）

1. @--verify-mapping@ 缺失的报错在**硬件采集之后**才触发（@main.rs:683@ 采集 → @689-692@ 才检查）：
   板端忘带该参数会先跑完一次 10–15s 采集才报错。建议把 loopback+verify 检查移到 @raw::measure_channel@ 之前。
2. 报错文案写的是 @--loopback-channels 必须...@，对 @--loopback-map@ 也适用，建议改成“loopback 模式（--loopback-map/--loopback-channels）”。
3. 若同时给 @--loopback-map@ 与 @--loopback-channels@，当前 map 优先且不报错；建议显式拒绝或 WARN（防误用）。

## 2. runbook 核验（已修正）

- T1：@--loopback-map 9:7 --names OUT9_IN7 --playback hw:4,0 --playback-channels 22 --capture hw:4,0 --capture-channels 8 --verify-mapping@；
  物理已确认 out9 线路输出→in7 线路输入、-30dBFS 安全；判读覆盖映射/隔离/延迟/极性，claim 写 @partial (out9->in7 only); 1:1 mapping and other channels not verified@。
- T2/T4：UMIK capture-channels 已改 1；T4 5.3.2 已改 10 路 @--channels 0..9 --names FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR@（FL0 FR1 C2 SL3 SR4 SUB1=5 SUB2=6 SUB3=7 HFL8 HFR9）。
- T4 negative-controls 已先省略；report 会记 @negative_controls.status=not_run@（板端后补 N1–N5 或最终报告声明）。

## 3. 设计侧同步：ACCEPTANCE §3.1 已更新

- 允许非 1:1 环回：@--loopback-map play:capture@（1-based，可多对）；**至少 1 对**即可 partial（推荐 ≥2 对；1 对须声明“单路径抽检”）。
- 报告字段改为：@t1.loopback_map_1based@、@covered_outputs_1based@、@covered_inputs_1based@、@uncovered_outputs_1based@、
  @loopback_mapping@、@isolation_max_other_db@、@mitigation@、@claim@。
- claim 必须声明非 1:1 路径不代表 1:1 映射；T1 partial ≠ T1 PASS；T2–T5 全过且有替代证据时总体验收写 @PASS_WITH_T1_PARTIAL@。
- 本次最终报告将记录：
  @T1: partial (out9->in7 only); 1:1 mapping and other channels not verified; remaining outputs via file backend + T4 acoustic evidence@

## 4. 板端数据要求（soak 结束后）

- T1：t1 的 analysis.json/diag.json/日志（含 mapping/isolation 值、loopback_map）；记录物理接线与 -30dBFS 安全确认。
- T2/T4：fr.csv/analysis.json/pairs.json/report.json；T4 另附文件后端校验报告；negative_controls 若未跑需在 report 标 not_run。
- T5：64/128 的 camilla.log/status.csv/dmesg。
- 数据发我后按 ACCEPTANCE T1–T5（T1 partial）复算并出最终验收报告。
