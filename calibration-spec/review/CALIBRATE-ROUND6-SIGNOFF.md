# calibrate/ Round-6 签收（剩余 P1 收尾，2026-11-15）

> release sha256=@a45dd7e5ef8029fbe6b68b437dd51f0fb47d73923224d4a5955feb148b63e2b7@（1,721,856 B）；
> cargo test 11/11；只读+release 二进制验证，未改 calibrate/。

## 结论：剩余非阻塞 P1 三项均已实现，设计侧收尾完成；板端 T1/T4/T5 数据到位后即可出 ACCEPTANCE 报告。

| 项 | 状态 | 证据 |
| --- | --- | --- |
| 1/3 oct 频率窗纹波 | ✅ | @main.rs:585-596@ 按每点 ±1/6 oct 频率窗平滑后取 max-min；示例 pair FL:FR@2k ripple=**0.49dB**（与声明一致） |
| measure-multi 诊断 JSON | ✅ | @main.rs:690-701@ 每通道 @<name>-chNN.diag.json@：nominal_start/actual_start/lag_samples/period_size/buffer_size/playback_offset_seconds/capture_file；T1/T5 可追溯 |
| merge report/v1 | ✅ | @main.rs:427-446@ 输出 @<out>.report.json@：schema/tspi-calibration-report/v1、tool、created_unix、calibration_file.path+sha256、layout、sample_rate、environment、global_validation、pairs、overall_pass；实测 report 的 cal sha256 与实际文件一致 |
| 未给 --pairs WARN | ✅ | @main.rs:325-327@ 未提供时 stderr WARN，提示 T4 应显式指定交叉对 |
| 示例/回归 | ✅ | offline/offline-invert 过 validator（0 error/0 warning）；hand→FAIL/hand180→PASS；cargo test 11/11 |

## 报告 schema 对照 ACCEPTANCE §8 的剩余小项（不阻塞板端，建议板端报告阶段补）

- @environment.chunksize@ 目前硬编码 0：T5 需要真实 64/128，建议 report 接收/记录实际 chunksize（或由板端 wrapper 覆盖）。
- report 缺 @negative_controls@（ACCEPTANCE §8 示例字段）：建议板端 T4 时把 N1–N5 的结果并入 report（producer 可读取外部结果文件合并）。
- @overall_pass@ 在“未给 --pairs”时仍可能为 true（pairs 为空）：建议改为 @pair_validation: not_run@ 或 overall_pass=false，避免报告误读为 T4 已通过。
- 带限/重复 SNR 按 audio-stack 计划留在板端重复测量层（T2）。

## 板端排期

v14 的 12h soak 预计 **2026-11-16 06:19** 结束；之后按 T1（MOTU 1:1 + --verify-mapping）→ T4（真实布局 merge --pairs → report/v1）→ T5（64/128 帧，记录 chunksize/xrun）执行。
数据（fr.csv + analysis.json + pairing/report）发我后，用 @../tools/check_pair_response.py@ 独立重建绝对相位复算，并对照 ACCEPTANCE 逐项出报告。
