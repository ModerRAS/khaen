# measured design spec v2（tspi-cal-measured-design/v2）

> 状态：2026-11-22 代码/单测/离线实验完成；**真实多点测量窗口、apply 后 holdout remeasure、
> T3 复测、T6 质量门、T68/T69 仍 NOT_RUN**。本文是 schema 与语义说明，不是验收结论。
> 实现：`rt-stack/crates/calibration/src/measured_design.rs`；实验证据：
> `rt-stack/evidence/calib-chain-20261121/multipoint-20261122/`。

## 0. 目的

旧 spec（v1）只有单个 `design_position`，validation 只能是 `same_as_design` 代理
（`pilot_single_position_same_as_design_proxy`），无法表达 T63 要求的
「design 与 validation/holdout 独立分组」。v2 增加可选 `design_positions[]` 与
`validation_positions[]`，为生产多点窗口（目标 T6：6 design + 3 holdout）做代码准备。

## 1. 向后兼容

| 场景 | 行为 |
| --- | --- |
| `schema: tspi-cal-measured-design/v1` | 原样接受；单 `design_position` + 必须 `validation = pilot_single_position_same_as_design_proxy`；构建行为与历史一致（曲线 = design×通道 + V1_proxy×通道；phase 只 design） |
| `schema: tspi-cal-measured-design/v2` + 单 `design_position` | 兼容写法；无 `validation_positions[]` 时仍必须 proxy，且只允许 1 个 design 点 |
| v2 + `design_positions[]`（>=2） | 必须给 `validation_positions[]`（>=1）与 `validation = independent_validation_positions`；**不允许 proxy 冒充 holdout** |
| v1 文件出现 v2 数组字段 | 稳定拒绝（`E_WIRING_MEASURED_SPEC`） |
| v2 同时出现 `design_position` 与 `design_positions[]` | 稳定拒绝（歧义） |

## 2. v2 字段

### 2.1 顶层（在 v1 基础上新增）

| 字段 | 类型 | 必填 | 语义 |
| --- | --- | --- | --- |
| `schema` | string | 是 | `tspi-cal-measured-design/v2` |
| `design_positions` | array<Position> | v2 二选一 | 参与 T64/T65 fit 的 design 测点（>=1） |
| `design_position` | Position | v2 二选一 | 单点兼容写法（与 `design_positions` 互斥） |
| `validation_positions` | array<Position> | 独立 holdout 必填 | 独立 validation/holdout 测点（>=1）；不参与拟合 |
| `validation` | string | 见上 | `independent_validation_positions`（有 validation_positions）或 `pilot_single_position_same_as_design_proxy`（无，仅单 design 点） |
| 其余（`session_id/layout/sample_rate_hz/target/provenance/channel_bands/dense_to_dac/roles/container_channels/unused_outputs/compile/modal_artifact/fit_options/crossover`） | 同 v1 | 同 v1 | 不变 |

### 2.2 Position

| 字段 | 类型 | 必填 | 语义 |
| --- | --- | --- | --- |
| `label` | string | 是 | T63 测点 label；全 spec（design+validation）唯一，非空/trim/<=64 字符 |
| `mic_orientation` | string | 是 | 听音位 plan 要求 `90deg`（T63/ACCEPTANCE §0.1.2；否则 plan 校验拒绝） |
| `weight` | number | 否 | T63 权重，默认 1.0；有限且 >0（设计权重和会写进证据） |
| `channels` | array<Channel> | 是 | 该测点逐通道 analysis；所有位置必须覆盖同一组通道名 |

### 2.3 Channel

| 字段 | 类型 | 必填 | 语义 |
| --- | --- | --- | --- |
| `name` | string | 是 | 通道/role 名（如 FL/FR/SL/SR/SUB1；同一位置内不得重复） |
| `analysis` | path | 是 | 旧工具 analysis JSON（`tspi-channel-measurement/v1`；相对 spec 目录或绝对路径） |
| `diag` | path | 否 | 槽位 nominal/actual onset；提供时优先用 `actual_start - nominal_start` 作相位重建延迟 |

### 2.4 示例（2 design + 1 独立 validation）

```json
{
  "schema": "tspi-cal-measured-design/v2",
  "session_id": "multipoint-pilot-20261122",
  "layout": "stereo-2.0",
  "sample_rate_hz": 48000,
  "target": "flat",
  "validation": "independent_validation_positions",
  "provenance": "synthetic_multipoint_fixture_offline_not_measured",
  "design_positions": [
    {"label": "D1", "mic_orientation": "90deg", "weight": 1.0, "channels": [
      {"name": "FL", "analysis": "D1-FL.analysis.json"},
      {"name": "FR", "analysis": "D1-FR.analysis.json"}]},
    {"label": "D2", "mic_orientation": "90deg", "weight": 0.8, "channels": [
      {"name": "FL", "analysis": "D2-FL.analysis.json"},
      {"name": "FR", "analysis": "D2-FR.analysis.json"}]}
  ],
  "validation_positions": [
    {"label": "V1", "mic_orientation": "90deg", "channels": [
      {"name": "FL", "analysis": "V1-FL.analysis.json"},
      {"name": "FR", "analysis": "V1-FR.analysis.json"}]}
  ],
  "channel_bands": [{"channel": "FL", "band_hz": [80.0, 16000.0]},
                    {"channel": "FR", "band_hz": [80.0, 16000.0]}],
  "dense_to_dac": [0, 1],
  "roles": [{"role_index": 0, "role_id": "FL", "dac": 0, "outputs": [0]},
            {"role_index": 1, "role_id": "FR", "dac": 1, "outputs": [1]}],
  "container_channels": 8,
  "unused_outputs": [2, 3, 4, 5, 6, 7],
  "compile": {"block_size": 128, "n_in": 2, "container_channels": 8,
              "low_band_d": 0, "l_g": 8, "l_p": 8},
  "modal_artifact": null
}
```

## 3. 构建语义（measured -> FitWiringDesign）

1. 逐位置逐通道读 analysis（fs 必须与 spec 一致；公共频带 = 所有位置/通道 sweep 交集）；
2. T64 输入 `curves`：全部真实位置 × 通道（design + 独立 validation 都进入曲线，
   validation 只用于报告/预测评估；**T64 聚合只用 design 位置**）；
3. T65 输入 `phase_responses`：**只 design 位置 × 通道**；validation 位置绝不给复响应
   （T65 以 `E_PHF_HOLDOUT_CONTAMINATION` 拒绝 holdout 进入相位拟合）；
4. T63 `MultipointPlan`：design 位置 role=design，validation 位置 role=validation，
   label/weight/mic_orientation 原样带入；v1/proxy 时追加 `V1_proxy` validation 测点（显式 proxy）；
5. 测量摘要 `measurement_sha256`：单点 legacy 保持历史口径（`channel:sha`）；
   多点口径含 position（`position:channel:sha`）。

### 3.1 T64 多点聚合口径（与 peq_fit.rs 实现一致，证据里写明）

| 用途 | 语义 |
| --- | --- |
| 模态（modal）曲线 | 每频点对 design 位置做线性幅度加权平均；同频跨座位差 >6 dB 时对偏离座位降权；模态检测的突出度用跨 design 位置最大值（任一位置 >= 门限的模态都要切） |
| 宽带（wide）曲线 | 每频点 design 位置功率加权平均，再做 1/3 oct 滑动平均 |
| rule_2a/2b 拟合质量门 | 只由 design 位置决定；validation 的 2a/2b 单列 `holdout_rule_*`，不并入 pass 位、不参与调参 |
| 逐位置残差 | T64 报告 `channels[].modal_residual_per_position` 同时包含 design 与 validation（role 字段区分） |
| 座位间 spread | measured summary/多点证据按 design 位置逐频点 max-min 报告 max/rms/worst_freq |

## 4. 证据 JSON

### 4.1 measured summary（`tspi-cal-measured-summary/v1`，新增键）

- `design_positions` / `validation_positions`：label 数组；
- `positions`：逐位置 `{label, role, weight, mic_orientation, channels[]}`；
- `multipoint`：
  - `design_count/validation_count/validation_mode`；
  - `t6_minimums`：6 design + 3 validation 要求，`points_met/status`（低于则
    `POINTS_BELOW_T6_MINIMUM`；即使点数满足也只写 `POINTS_MET_NOT_ACCEPTED`）；
  - `fit_aggregation`：design-only、模态/宽带聚合口径、逐位置残差来源；
  - `seat_spread`：逐通道 `{max_spread_db, rms_spread_db, worst_freq_hz}`；
  - `holdout`：`mode/evaluation/remeasure_after_apply/participates_in_fit`。

### 4.2 多点证据（`tspi-cal-measured-multipoint/v1`）

由 `multipoint_evidence_json(&FitWiringDesign, &PeqFitReport)` 生成（示例：
`evidence/.../multipoint-20261122/report/multipoint-report.json`）：

- `counts/t6_minimums/positions/fit_aggregation/seat_spread`；
- `channels[].per_position_residuals[]`：逐通道逐位置（design+validation）的
  `modal_rms_before/after_peq/after_p`、`modal_max_after_p`、rule pass 位；
- `channels[].holdout_rule_*`：validation 的 2a/2b 单列；
- `holdout`：**`evaluation = PREDICTED_FROM_MEASURED_CURVES_AT_VALIDATION_POSITIONS`**，
  `remeasure_after_apply = NOT_RUN`，`participates_in_fit = false`；
- `not_run` / `limits`：真实多点窗口/T3/T6/T68/T69/T54 READY artifact 等。

> 诚实口径：holdout 现在是「用拟合出的 P 作用到 validation 位置**实测曲线**的 predicted 评估」，
> 不是 apply 后真实 remeasure；真实 holdout remeasure 与两轮复现仍 NOT_RUN。

## 5. 稳定错误码

| code | 语义 |
| --- | --- |
| `E_WIRING_MEASURED_SPEC` | spec 结构非法（schema/v1-v2 混用/歧义/重复 label/非法 weight/proxy 多点等） |
| `E_WIRING_MEASURED_FILE` | analysis/diag 读取或 JSON 解析失败 |
| `E_WIRING_MEASURED_DATA` | 数据不满足 fit（fs 不一致、通道集合不一致、公共频带过窄、点不足等） |
| `E_WIRING_MEASURED_NOT_WIRED` | 需要但未接线的生产件（T54 READY artifact 等） |

## 6. 当前边界（NOT_RUN）

- 真实多点测量窗口（UMIK/MOTU 逐位置采集，design>=2 + holdout>=1）**未执行**；本仓实验输入为合成曲线；
- apply 后 holdout remeasure、T3 复测、T6 质量门、T68/T69 两轮复现**未执行**；
- T54 READY modal artifact：pilot 点数 < T6 最低要求，measured 路径恒 INCOMPLETE，compile 需显式实验旁路；
- host plan-arena（fake/ALSA）当前只加载 factorized plan；fused NOT_WIRED，离线实验用
  `--compile-force-factorized` 显式强制并记录在 experiment 证据中。
