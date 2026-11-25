# calibrate/ Round-5 签收（P1 收尾，2026-11-15）

> release sha256=@69a8b09730b3447c5281e9d224eb6dbd0bedbd4bb084952370410d84c5d20fc3@（1,684,480 B）；
> cargo test 11/11；只读+release 二进制回归，未改 calibrate/。

## 结论：Round-4 两个问题均关闭，设计侧 P1 收尾通过，可进板端验收。

| Round-4 问题 | 状态 | 证据 |
| --- | --- | --- |
| 片段极性 Gain 未接入 pipeline | ✅ 关闭 | @main.rs:294-307@ 极性块在 pipeline push 之前；hand180 fragment：@pipeline: [{channels:[1], names:[cal_ch1_pol]}]@（纯反相通道也生成 pipeline）；hand→FAIL、hand180→PASS 回归正常 |
| 双反相风险 | ✅ 关闭 | @channelmap_roles@ 改为 advisory：@apply: false@ + note “pipeline 与 roles 二选一，勿同时应用”；fragment note 同步 |
| --max-stages 未 clamp 32 | ✅ 关闭 | @main.rs:242/321@ @.min(32)@；fit/merge 均生效 |
| 示例/回归 | ✅ | offline/offline-invert 过 validator（0 error/0 warning）；cargo test 11/11 |

## 剩余（不阻塞板端）

- 1/3oct ripple、pairs 自动生成、measure 诊断（period/buffer/actual_start/lag）入 JSON、200Hz–8kHz 带限 SNR、report/v1 —— audio-stack 计划板端前补。
- 板端 T1/T4/T5 排期：v14 的 12h soak 结束（约 2026-11-16 06:19）后执行；数据由 audio-stack 发我，用 calibration-spec/tools/check_pair_response.py 独立复算并出验收报告。

## 设计侧交付状态

CONTRACT/ACCEPTANCE/validator/checker/review（Round1-5）齐备；P0 全关、P1 批量收尾完成；等板端数据做 T1/T4/T5 验收。
