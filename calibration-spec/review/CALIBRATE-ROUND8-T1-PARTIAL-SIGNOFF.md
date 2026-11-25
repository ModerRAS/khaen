# T1 partial 方案签收（Round-8，2026-11-15）

> 对象：audio-stack release sha256=@c89865b5c0451dc4978c5d2e1649a74bdf9a3c82c8fbf0a5d657328a7a774b3f@（1,742,848 B）；
> cargo test 11/11；只读 review + release 二进制，未改 calibrate/。

## 结论

T1 partial（2 根 TRS 仅覆盖 out1/out2）**可接受为降级验收**，不阻塞 T4/T5；ACCEPTANCE T1 已加入
partial 规则与报告字段（§3.1）。loopback 实现基本正确；发现 2 个代码小问题 + 3 个 runbook 会导致板端
T4 失败/数据无效的问题，建议板端前修。

## 1. loopback 实现核验

- @--loopback-channels 1,2@ → 内部 0,1；播放 outN 时从采集 inN 取信号（@main.rs:634-645,679-680@）；
- @--verify-mapping@ 时校验最大能量通道==播放通道，并算其它采集通道相对隔离，> -60 dB 直接 FAIL（@main.rs:692-708@）；
- diag.json 增 @loopback_mode/@loopback_channels_1based/@isolation_other_max_db@；默认名 LOOP1/LOOP2；usage 有 T1 抽检示例（@main.rs:725-736,755@）；
- 实测 guard：@--capture-channels 1@ → 报“需 capture-channels >= 2”（rc=1）。

### 代码小问题（建议板端前修，P2）

1. @--loopback-channels 0,1@ 会被 @saturating_sub(1)@ 静默映射成 out1（0→0），没有拒绝 0；应校验输入 ≥1（1-based）并对 0 报错。
2. loopback 模式**不强制** @--verify-mapping@：不传时不做映射/隔离检查、直接测 delay/极性；建议 loopback 模式自动隐含 verify（或至少 WARN/报错要求显式传）。

## 2. runbook（BOARD-T1-T5-RUNBOOK.md）需要修正的 3 处（会导致 T4 数据无效）

### 2.1 T4 的 5.3.2 通道映射错了（重要）

runbook 第 49 行用：
```
--channels 0,1,2,3,4,5,6,7 --names FL,FR,C,SUB1,SL,SR,HFL,HFR --layout 5.3.2
```
实际 layout-presets（audio/overlay/etc/khaen-audio/layout-presets.yml）5.3.2 是 10 路：
FL0 FR1 C2 **SL3 SR4 SUB1=5 SUB2=6 SUB3=7 HFL8 HFR9**。
按 runbook 跑会把 SUB1 标到 ch3（实际 SL）、SL 标到 ch4（实际 SR）、HFL 标到 ch6（实际 SUB2）、HFR 标到 ch7（实际 SUB3），
且缺 SUB2/SUB3/HFL/HFR 的独立测量 → T4 校准/报告全部错位。
建议命令：
```
--channels 0,1,2,3,4,5,6,7,8,9 \
--names FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR --layout 5.3.2
```

### 2.2 T2/T4 的 capture-channels 与 UMIK-1 不符

runbook 的 T2/T4 用 @--capture hw:3,0 --capture-channels 2@；UMIK-1 是 1 通道，按 2 通道打开可能直接失败。
建议先 @arecord -D hw:3,0 --dump-hw-params@ 确认，通常改 @--capture-channels 1@；T1 用 MOTU 采集 2 通道是对的。

### 2.3 T4 的 negative-controls 文件未创建

merge 命令带 @--negative-controls /var/lib/khaen-calib/t4/negative_controls.json@，但 runbook 没给生成步骤；
文件不存在时 merge 会直接报错。建议先写一个 N1–N5 结果 JSON（或板端前先省略该参数，T4 报告再补）。

## 3. 已同步的设计侧修订

- ACCEPTANCE.md §3.1 新增 **T1 partial 规则**：
  - 覆盖集逐通道 1-based 列出（至少 2 路），覆盖通道完成 mapping/isolation/延迟/极性抽检；
  - 未覆盖通道必须有①文件后端渲染校验 + ②T4 声学 pair/单通道 FR 证据，并在报告列出；
  - 报告必须含 @t1.status=partial@、covered/uncovered、loopback_mapping、isolation_max_other_db、mitigation、claim；
  - **T1 partial ≠ T1 PASS**；T2–T5 全过且替代证据齐全时总体验收写 @PASS_WITH_T1_PARTIAL@（或等价），报告开头写范围声明。
- 本次签收后，最终验收报告会按上述字段记录 @T1: partial (electrical loopback out1/in1, out2/in2 only); 8ch mapping not verified@。

## 4. 板端数据要求

- T1 partial：t1 的 analysis.json/diag.json/日志（含 isolation 值）；
- T4：逐通道 fr.csv/analysis.json、pairs.json、report.json、negative_controls；另附文件后端校验报告；
- T5：64/128 的 camilla.log/status.csv/dmesg 片段；
- 数据发我后按 ACCEPTANCE T1–T5（T1 partial）复算并出最终报告。
