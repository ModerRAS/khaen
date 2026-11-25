# calibrate/ 板端前签收（Round-7，2026-11-15）

> release sha256=@efd83318b399bb799354b40b8fafcc3dc1f3cec31ace2803e9c7638dd11ecc80@（1,735,680 B）；
> cargo test 11/11；只读+release 二进制验证，未改 calibrate/。

## 结论：Round-6 三个报告小项全部关闭；**设计侧交付完成，进入板端验收阶段**。

| Round-6 小项 | 状态 | 证据 |
| --- | --- | --- |
| report.environment.chunksize | ✅ | 新增 @--chunksize@（@main.rs:430/449@）；实测 merge --chunksize 128/64 → report.environment.chunksize 分别为 128/64；示例 report=128 |
| report.negative_controls | ✅ | @--negative-controls <json>@；实测传 2 条 → status=provided/items 2；未传 → status=not_run/items=[]（@main.rs:432-439,453@） |
| 无 --pairs 的 PASS 语义 | ✅ | @main.rs:431/452/454/455@：pair_validation=not_run、overall_status=INCOMPLETE、overall_pass=false；有 pairs 且 pass → pair_validation=pass/overall_status=PASS/overall_pass=true（实测两种场景均符合） |
| 回归 | ✅ | 示例 validator 0 error/0 warning；hand→FAIL、hand180→PASS；cargo test 11/11 |

## 板端验收准备（下一步）

v14 的 12h soak 预计 **2026-11-16 06:19** 结束。之后按：
1. **T1**：MOTU 1:1 电学环回 + @measure-multi --verify-mapping@；核对 diag.json 的 nominal/actual_start/lag、逐通道映射、延迟 ±0.5 sample、极性 100%；记录链路极性 @--polarity-offset@。
2. **T4**：真实布局（4.1.2/5.3.2）逐通道测量 → @merge --pairs <各分频点> --chunksize <实测> --negative-controls <N1–N5 json>@ → report/v1；用 @../tools/check_pair_response.py@ 由 fr.csv+analysis.json 重建绝对相位独立复算。
3. **T5**：64/128 帧各 10 min，xrun=0、CPU 变化 ≤10%；report 记录 chunksize。
4. 数据（fr.csv/analysis.json/report/日志）发我，按 ACCEPTANCE T1–T5 出最终验收报告。

## 交付清单（设计侧）

- 规范：CONTRACT.md、ACCEPTANCE.md
- 工具：tools/validate_calibration.py、tools/check_pair_response.py
- 示例：examples/calibration-4.1.2/5.3.2.example.yml
- 评审记录：review/CALIBRATE-DRAFT-REVIEW.md（R1）、ROUND2/3/4/5/6/7 各轮
- 状态：P0 全关；P1 全部收尾（Round-7 关闭最后 3 项）；板端 T1/T4/T5 待执行。
