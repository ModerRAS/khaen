# khaen-cal-batch：校准批量闭环（前后扫频 + 瀑布/RT + MIMO/holdout 对比）

> task-201 · 2026-11-17 · 状态：**离线流水线已自测 PASS；真实板端全部 NOT_RUN**
> 证据：`rt-stack/evidence/calib-batch-waterfall/` · 窗口步骤：`docs/BOARD-WINDOW-RUNBOOK-T67-T74.md` §14
> **观察量口径**：本工具输出的 IMPROVED/FLAT/WORSE 不是验收判据；验收权威仍是
> `rt-stack/spec/mimo-t6.md`（T6-M）与 `calibration-spec/ACCEPTANCE.md`（T6-cal），
> 用户要求第 29 条：3 dB/15% 也只是工程观察指标。

## 1. 目标与边界

一次命令跑完校准闭环：**前**多点扫频 -> 生成/应用滤波器与 MIMO 矩阵 -> **后**同点位复测 ->
输出每通道频响、瀑布/CSD 衰减、EDC/T20/RT60、MIMO 前后对比、独立 holdout、问题定位。

- 生产口径 = **单支 UMIK-1 移动多点**（design >=6 + holdout >=3；每次 3 遍）；
- **三麦固定（UMIK/MA1/ARC）= pilot only**，独立 lane、独立标注，不进入 design/holdout/MIMO 结论；
- 白天窗口 `08:30-12:00 / 14:30-18:00`；默认 -50 dBFS 起步；可闻升压需窗口授权；
- 板端只执行冻结命令（窗口内不现场生成/不现场分析）；分析在工作站执行（用户二阶段策略）。

## 2. 目录结构

```text
rt-stack/tools/calib-batch/
  khaen_cal_batch.py        CLI：plan / run / analyze / selftest / version
  calbatch/
    config.py              冻结配置 schema + fail-closed 校验
    dsp.py                 扫频/反卷积/FR/EDC/RT/瀑布/Biquad/SVG（numpy）
    measure.py             capture meta 布局、IR 提取、质量门
    model.py               离线合成 plant + fake apply（仅自测，不是真实算法）
    plan.py                窗口前冻结计划：步骤/argv/时间预算/RUN-PLAN.md
    runner.py              执行器：fake/cmd transport；fail-closed/restore
    analysis.py            前后对比/瀑布/RT/MIMO/holdout/问题定位/报告
    selftest.py            离线端到端自测（improved/worse/flat）
  configs/batch-config.production-4.1.2.json   生产配置模板（两轮 A/B）
  prep-example/            窗口前 prep 包装脚本示例（preflight/apply/restore）
  run-batch-window.sh      板端窗口内一键执行包装
  docs/METRICS-THRESHOLDS.md                   指标定义与观察阈值
```

## 3. 与现有实现的关系（只读复用，不改契约）

| 现有实现 | 本工具如何对齐 |
|---|---|
| `rt-stack/crates/calibration/src/job.rs`（JobStage 10 阶段 / JobRequest full/retest） | 批量流程把 T67 full job 拆成可一次跑完的 phase：preflight->freeze->baseline->apply->after->pilot->analyze；真实 apply 走 prep/apply.sh（REST job apply，B4 解锁后启用），**不重写 JobManager** |
| `stage_backend.rs`（fake/UMIK 采集、48k、-50 dBFS、SNR/xrun/峰值门） | 批量 fake transport 复用同一组口径常量（48k 生产、-50 起步、SNR>=25、峰值<=-1 dBFS）；真实采集调用 `khaen-calibrate measure-multi` |
| `peq_fit.rs`（1/48 oct 格、Q/S/gain 界限、D7 +3 dB boost 帽） | 分析报告记录 PEQ 相关观察（模态 FR 偏差/高带平坦度）；fake apply 的 PEQ 拟合只是演示，**不是 T64 算法** |
| `phase_fit.rs`（延迟/极性/相位 RMS 门） | 汇报质量门引用其 0.5 sample / 30 deg 口径作为观察参考；批量流程不改 phase 契约 |
| `positions.rs`（design>=6/holdout>=3、repeats>=3、SNR 25、repeat spread 门） | config 校验强制同样点数/复次；质量 CSV 记录 SNR、repeat delay/mag 散差 |
| `compile_apply.rs`（prepared/apply/rollback 状态机） | apply/restore 由 prep 脚本对齐；失败 fail-closed，rollback 只允许 last-good/mute（C 永不回退） |
| `docs/BOARD-WINDOW-RUNBOOK-T67-T74.md` §2/§4/§10 | 常量/口径/证据目录直接沿用；本工具新增 §14 批量步骤 |

## 4. 用法（三步）

### 4.1 窗口前（工作站，不碰板）

```bash
cd rt-stack/tools/calib-batch
python3 khaen_cal_batch.py plan \
  --config configs/batch-config.production-4.1.2.json \
  --out ~/khaen-batch/plan
# 输出 round-A/ round-B/，各含 plan.json / commands.txt / RUN-PLAN.md / time-budget.json
# plan 会校验：schema、48k、-50 dBFS、白天窗、可闻授权、UMIK 90deg+cal sha256、
#            design>=6 + holdout>=3 + repeats>=3、pilot 分离、命令模板、时间预算是否超窗
```

窗口前还要准备（每项冻结 hash 记入证据）：
1. `prep/preflight.sh`、`prep/apply.sh`、`prep/restore.sh`（从 `prep-example/` 复制并填入窗口真实路径/端点）；
2. `khaen-calibrate` 二进制与 `calibration-spec/mics/umik1_718-7696_90deg.txt` 在板端就位；
3. 点位标记（P1..P9，label/role 已进 config）；三麦固定位架好（pilot 不进签收）。

### 4.2 窗口内（板端，只执行）

```bash
python3 khaen_cal_batch.py run \
  --plan plan/round-A/plan.json \
  --transport cmd \
  --work /var/lib/khaen-rt-stack/evidence/$(date +%Y%m%d)-calib-batch-A
# 只跑 plan 冻结的 argv；每步写 steps/<NN-id>/{stdout.log,stderr.log,exit.json}
# + captures/<step>.meta.json；任一步失败 fail-closed（preflight/measure/apply），
# 自动执行 restore（如 config.restore_after_batch=true），exit 4
```
板端不需要 numpy（只执行命令与取证）；不需要人工逐步执行。

### 4.3 窗口后（工作站，分析 + 报告）

```bash
# 把板端 work 目录拉回后
python3 khaen_cal_batch.py analyze --work <work目录> --out ~/khaen-batch/analysis-A
# 输出 analysis/{REPORT.md,comparison.json,MANIFEST.json,quality.csv,fr/,rt/,waterfall/,plots/}
```

## 5. 输出与指标（全部为观察量）

- 每通道/点位频响：`fr/<group>/<point>-<before|after>.csv`（1/24 oct 网格）；
- RT/EDC：`rt/<group>/<point>-<before|after>.csv`（1/3 oct 带，T20 拟合并外推 RT60；EDC@100ms 余振）；
- 瀑布/CSD：`waterfall/<group>/<point>-<timepoint>-<channel>.csv`（1/3 oct 带 EDC 在 0..400 ms 的采样矩阵）；
- 前后对比：`comparison.json`（design/holdout/pilot、verdict、issues、quality）；
- 图：`plots/` 下 RT before/after、CSD 余振热图等 SVG；
- 问题定位：按频带/通道/点位列出恶化项（RT 恶化 >=5% 或 EDC@100ms >=1 dB 等，见 docs/METRICS-THRESHOLDS.md）。

分类规则与阈值见 `docs/METRICS-THRESHOLDS.md`；**不产生 PASS/FAIL**。

## 6. 离线自测

```bash
python3 khaen_cal_batch.py selftest --out <证据目录>
```
- fake transport 合成 9 点位（6 design + 3 holdout）+ 三麦 pilot 的 before/after 记录；
- 断言：plan 负向用例被拒、IMPROVED/WORSE（含问题定位）/FLAT 三场景分类正确、
  holdout 不参与 apply 拟合、产物齐全、全流程 NOT_RUN 标注；
- 结果见证据包 `selftest/selftest-result.json` 与 `SELFTEST-REPORT.md`。

## 7. 依赖与运行环境

| 场景 | 依赖 |
|---|---|
| plan（工作站/板端） | python3.8+，**仅标准库** |
| run --transport cmd（板端） | python3.8+，仅标准库；khaen-calibrate/prep 脚本由窗口准备 |
| analyze / fake 自测（工作站） | numpy>=1.20（无 numpy 时 analyze 明确报错，不静默降级） |

板端零 numpy 依赖已用 `python3 stdlib-only-check.py` 验证（阻断 numpy 导入后 plan + `run --transport cmd --dry-run` + version 均 exit 0）。

## 8. 退出码

| 码 | 含义 |
|---|---|
| 0 | 成功 |
| 2 | 用法/配置校验失败（fail-closed，未产生任何板端动作） |
| 3 | NOT_RUN/阻塞（预留；真实 apply B1/B4 未解锁时由 prep 脚本返回） |
| 4 | 执行失败（采集/apply 失败，已尝试 restore） |
| 5 | 自测失败 |

## 9. 诚实性声明

- 本工具**未碰板端、未执行任何真实测量/apply**；证据中的采集与前后结果若标 `simulated: true` 均为合成数据。
- 真实房间前后瀑布/RT/MIMO 结论 = **NOT_RUN**，需窗口执行 + 独立复算后才可写观察结论。
- fake apply 只是合成演示，不是 T64/T65 算法，不能用于声称"校准算法有效"。
- 不允许把本工具的分类写成 PASS；不允许用 predicted/fake 数据替代 T3/T6 实测。
