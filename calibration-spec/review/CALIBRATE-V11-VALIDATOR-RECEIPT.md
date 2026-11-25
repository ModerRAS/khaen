# v1.1 校验器收尾签收（2026-11-16）

> 范围：只改 `calibration-spec/`（`calibrate/` 全部只读，未改 Rust）；不新增依赖（stdlib + PyYAML）。
> 对象：Lead 补丁后的 `tools/validate_calibration.py`（v1/v1.1 支持）与主入口 `khaen-calibrate validate`。
> 复现：驱动工作区 `C:\WorkSpace\Claw\scratch\lead\spec-check\run_cases.py`（23 条用例）；修正后输出 `report.txt`，修正前快照 `report-before-fix.txt`（当时 20 条，已覆盖 Q=15 误报与 v1 可读取正例；后补的 3 条 v1 示例/手工 v1.1 正例见 `report.txt`）。
> **2026-11-17 补注（C-P1-7）**：本回执的“23 条结论一致”只覆盖已测的 schema/type/q/freq/边界类用例；`name` 唯一、polarity 必填与 ±1、reference 指向 delay=0、layout 交叉校验、未知字段 warning、through_render/render_offsets 等规则 Rust 主入口尚未实现，两校验器**并非全规则一致**。T0 要求 Rust 主入口 + Python 独立复核都通过；差异表见 CONTRACT §5.1。

## 结论

1. **v1/v1.1 规则实现正确**：同一批 23 条用例上，Python 校验器与 Rust `khaen-calibrate validate` 的 OK/FAIL 结论完全一致
   （v1 仍只允许 peaking 且 Q≤10；v1.1 允许 peaking/low_shelf/high_shelf，Q 只要求有限且 ≥0.1，无上限）。
2. **发现并修复 1 个真实缺陷**：修正前，`schema: khaen-calibration/v1.1` + 纯 Peaking + **Q=15** 的文件会被 Python 校验器
   误标「v1.1 文件只含 Peaking 且 Q≤10：v1 消费方可直接读取」——v1 消费方实际会拒绝 Q>10。现在 Q>10 会清除
   `v1_compatible`，与 Rust 主入口一致（Rust 对该文件不给「v1 可读取」后缀）。
3. **「v1 可读取」分支确实会触发**：三个正例均命中——真实 producer 输出 `calibrate/target/robust-fixture/calibration.yml`、
   当天 `merge` 重新生成的 v1.1（`scratch/lead/spec-check/merge/calibration-v11.yml`）、以及手工把 v1
   `calibrate/target/offline-remerge.yml` 改成 v1.1（含 4 段 peaking，Q≤6.5）。另补一条口径：该提示改为**只在文件无 error 时**
   给出，避免 FAIL 文件同时宣称「可直接读取」。
4. **主入口与独立复核的差异仅剩提示口径**（不影响 PASS/FAIL），见下方「与主入口的差异」。

## 用例矩阵（修正后）

| # | 用例 | Python 校验器 | Rust `validate` | 预期 |
| --- | --- | --- | --- | --- |
| 1 | v1 合法：`calibrate/examples/offline/calibration.yml` | OK 0 error 0 warn | PASS | OK |
| 2 | v1 合法：`calibration-spec/examples/calibration-5.3.2.example.yml` | OK 0/0 | PASS | OK |
| 3 | v1 合法：`calibrate/examples/offline-invert/calibration.yml` | OK 0/0 | PASS | OK |
| 4 | v1 合法：`calibration-spec/examples/calibration-4.1.2.example.yml` | OK 0/0 | PASS | OK |
| 5 | v1.1 `high_shelf`：`calibrate/target/fit-tilt.yml`（producer fit 输出） | OK 0/0（无 v1 提示） | PASS（无后缀） | OK |
| 6 | v1.1 `low_shelf`（合成） | OK 0/0 | PASS | OK |
| 7 | v1.1 纯 Peaking（真实 producer：`target/robust-fixture/calibration.yml`，空 filters） | OK；WARN v1 可读取 | PASS（v1 可读取） | OK + 提示 |
| 8 | v1.1 纯 Peaking（当天 `merge` 生成，空 filters） | OK；WARN v1 可读取 | PASS（v1 可读取） | OK + 提示 |
| 9 | v1.1 纯 Peaking（手工：v1 offline-remerge 改 schema；4 段 peaking，Q≤6.5） | OK；WARN v1 可读取 | PASS（v1 可读取） | OK + 提示 |
| 10 | v1.1 纯 Peaking Q=10（边界） | OK；WARN v1 可读取 | PASS（v1 可读取） | OK + 提示 |
| 11 | v1.1 Q=0.1（边界） | OK；WARN v1 可读取 | PASS（v1 可读取） | OK + 提示 |
| 12 | v1.1 Peaking **Q=15** | OK；**无**提示 | PASS（无后缀） | OK（修正前误报 v1 可读取） |
| 13 | v1.1 Q=0.05 | FAIL：`q 必须是有限数且 ≥ 0.1` | FAIL（同） | FAIL |
| 14 | v1.1 `q: .nan` | FAIL：同上 | FAIL（同） | FAIL |
| 15 | v1.1 freq=19 | FAIL：`freq 必须在 [20, 20000.0]` | FAIL：freq 超出范围 | FAIL |
| 16 | v1.1 freq=20000（边界） | OK；WARN v1 可读取 | PASS（v1 可读取） | OK |
| 17 | v1.1 freq=20001 | FAIL：freq 越界 | FAIL | FAIL |
| 18 | v1.1 未知 type `notch` | FAIL：`type 不认识` | FAIL | FAIL |
| 19 | v1.1 两个 `high_shelf` | OK（校验器未强制「各一段」） | PASS | 见待裁决项 3 |
| 20 | **v1 中含 `high_shelf`** | FAIL：`v1 只允许 peaking` | FAIL | FAIL |
| 21 | v1 peaking Q=12 | FAIL：`q 必须在 [0.1,10]` | FAIL：`q v1 必须 ≤ 10` | FAIL |
| 22 | v1 Q=10（边界） | OK 0/0 | PASS | OK |
| 23 | 未知 schema `khaen-calibration/v2` | FAIL：schema 必须是 v1 或 v1.1 | FAIL | FAIL |

代表输出（修正后）：

```text
$ python3 tools/validate_calibration.py scratch/lead/spec-check/cases/v11_q15_peaking.yml      # 修正前会多一条 WARN v1 消费方可直接读取
OK    .../v11_q15_peaking.yml
结果：errors=0 warnings=0

$ python3 tools/validate_calibration.py scratch/lead/spec-check/cases/v11_peaking_q_le10_handmade.yml
OK    .../v11_peaking_q_le10_handmade.yml
  WARN   v1.1 文件只含 Peaking 且 Q≤10：v1 消费方可直接读取
结果：errors=0 warnings=1

$ khaen-calibrate validate scratch/lead/spec-check/cases/v11_peaking_q_le10_handmade.yml
PASS .../v11_peaking_q_le10_handmade.yml（v1 可读取）

$ khaen-calibrate validate calibrate/target/fit-tilt.yml
PASS calibrate/target/fit-tilt.yml          # high_shelf（无 v1 提示）
```

## 与主入口的差异（advisory，不影响 PASS/FAIL）

1. Rust `validate` 对 **v1** 文件也打印「（v1 可读取）」后缀（`check.rs`：`v1_readable = (v1 || v11) && v1_ok && ...`）。
   对 v1 文件该后缀语义上多余，建议 audio-stack 收窄为仅 v11 时输出。
2. Rust 在文件已有 freq/q error 时仍向 stderr 打「v1 消费方可读」warning；Python 修正后仅在无 error 时给该提示
   （避免「FAIL 同时宣称可直接读取」）。两者判定一致。
3. CONTRACT §10 写「架滤波器……最多各一段」，但 Rust/Python 校验器当前都不强制（producer `peq::maybe_shelves` 每通道
   最多一段，正常输出不会越界；只有手写文件可能）。待 Lead 裁决：校验器补 warning/error，或 §10 注明为 producer 约定。
   本轮未改（不改契约字段语义）。

## 改动文件

- `tools/validate_calibration.py`：v1.1 中 `q > 10` 时 `v1_compatible = False`（不再误报「v1 可读取」）；
  「v1 可读取」提示增加 `not c.errors` 条件；docstring 同步。
- `README.md`：标题/版本、文件表、新增「校验入口」（Rust 主入口 + Python 独立复核）、状态区 2026-11-16 两条。
- `CONTRACT.md`：状态行、§2/§2.1（schema/q/type 的 v1.1 说明）、§3.4（v1.1 实际 type 集合）、§5（v1.1 差异范围）、
  §8（主入口+复核命令）、§10（校验入口分工、v1 可读取提示的准确条件）。契约字段语义未改。
- `review/CALIBRATE-V11-VALIDATOR-RECEIPT.md`：本文件。
