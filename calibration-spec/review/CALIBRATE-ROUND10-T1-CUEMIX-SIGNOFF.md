# T1 partial + CueMix 路由条件签收（Round-10，2026-11-15）

> release sha256=@0cee00af58b9806286d84ad574dbc2344d8af70ac9ac736ae8c32b6a81303c00@（1,745,408 B）；
> cargo test 11/11；只读 review + release 二进制，未改 calibrate/。

## 结论：三个 P2 已修完并验证；CueMix 实际路由与 T1 安全前提已记录；**T1 partial（out9→in7）可按 runbook 执行**。

## 1. P2 修复核验

| P2 | 状态 | 证据 |
| --- | --- | --- |
| verify-mapping 检查在 arecord 之前 | ✅ | @main.rs:663-667@ 在 @raw::measure_channel@（691）之前检查；实测缺 verify 时立即报错，无 “measure-multi …” 采集启动行、无 arecord spawn |
| 文案覆盖 map/channels | ✅ | 报错为 “@--loopback-map/--loopback-channels 必须同时带 --verify-mapping@” |
| 同时给 map/channels 报错 | ✅ | @main.rs:637-639@ 直接报错；实测 rc=1 |
| 回归 | ✅ | 示例 validator 0/0；hand→FAIL、hand180→PASS；cargo test 11/11 |

## 2. CueMix 路由与 T1 安全前提（runbook 已记录）

- 用户当前 CueMix：**in7→out7 ON、in8→out8 ON；in7→out9 none；其它 input→output none；out7/out8 downstream disconnected**；T1 时 out7/out8 不接后级；
- 物理：MOTU **out9（线路输出）→ in7（线路输入，无 48V）**，-30dBFS 扫频峰值 ≈0.03 FS，线路对线路安全；
- T1 命令：@--loopback-map 9:7 --names OUT9_IN7 --playback hw:4,0 --playback-channels 22 --capture hw:4,0 --capture-channels 8 --verify-mapping@；
- **最终 insert 模式必须关闭 in7→out7、in8→out8 及全部 input→output 直通**，否则硬件直通与 USB DSP 双路径并存；报告需记录关闭证据。
- 设计侧 ACCEPTANCE §3.1 已加入：
  - T1 前记录 CueMix/矩阵路由状态（截图+文本）与 out7/out8 downstream 状态；
  - 最终 insert 模式关闭全部 input→output 直通并留证；
  - 报告字段新增 @t1.cuemix_routing@、@t1.final_insert_direct_routes@。

## 3. T1 partial 报告口径（最终验收将采用）

```
t1.status: partial
loopback_map_1based: [{play: 9, capture: 7}]
covered_outputs_1based: [9]
covered_inputs_1based: [7]
uncovered_outputs_1based: [1..8, 10..22]
loopback_mapping: pass
isolation_max_other_db: <= -60
cuemix_routing: {in7_to_out7: on, in8_to_out8: on, in7_to_out9: none, other_input_to_output: none, out7_out8_downstream: disconnected}
final_insert_direct_routes: disabled
mitigation: [file_backend, t4_acoustic]
claim: partial: electrical loopback out9->in7 only; 1:1 mapping and other channels not verified; remaining outputs via file backend + T4 acoustic evidence
```
T1 partial ≠ T1 PASS；T2–T5 全过且替代证据齐全时总体验收写 @PASS_WITH_T1_PARTIAL@。

## 4. 板端执行顺序（soak 结束后）

1. 记录 CueMix 状态截图；确认 out7/out8 不接后级；
2. T1：按 runbook 跑 @--loopback-map 9:7 --capture-channels 8 --verify-mapping@，收 analysis/diag/日志（mapping/isolation）；
3. T2：UMIK capture-channels=1，3 次重复；
4. T4：5.3.2 十路映射（FL0 FR1 C2 SL3 SR4 SUB1=5 SUB2=6 SUB3=7 HFL8 HFR9），pairs + report/v1；negative-controls 暂 not_run；
5. T5：64/128 各 10 min，0 xrun；
6. 最终 insert 模式关闭全部 input→output 直通，留证；
7. 数据（fr.csv/analysis.json/diag/pairs/report/日志）发我复算。
