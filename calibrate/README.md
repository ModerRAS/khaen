# khaen-calibrate — 多声道扬声器校准（Rust 单二进制）

逐通道对数扫频（默认 **-30 dBFS**；T1 电学环回固定 **-12 dBFS**，见板端 runbook，一次只响一个通道）→ MOTU 1:1 → UMIK 采集 → Farina 反卷积 →
每通道 **延迟 / 极性 / 相位** 分开报告 → PEQ + 延迟校准 YAML（可被 CamillaDSP 直接消费）→ 合并与相位抵消检查。

- 纯 Rust 单二进制（无 Python），测量命令调用系统 `aplay`/`arecord`（S24_3LE）。
- 本工具只做测量/分析/拟合；不启动或重启音频服务。

## 构建与测试

```bash
cargo build --release
cargo test          # 离线测试（sweep/Farina/分数延迟/PEQ 拟合/合并抵消）；用例数随版本变化，以命令输出为准
```

## 快速用法（板端，MOTU=card4，UMIK=card3；块长用 256 稳定）

```bash
# 0) 只测一个通道（其余 21 通道静音；-30dBFS）
./khaen-calibrate measure --out cap-ch0.raw --channel 0 \
  --playback hw:4,0 --playback-channels 22 \
  --capture hw:3,0 --capture-channels 2 \
  --rate 48000 --seconds 5 --level-db -30 --pre-seconds 1 --post-seconds 1

# 1) 分析：Farina 逆滤波 + 相位斜率分数延迟；输出 fr CSV / impulse CSV / analysis JSON
./khaen-calibrate analyze --capture cap-ch0.raw --capture-channels 2 --capture-channel 0 \
  --channel 0 --out-prefix results/ch00 --impulse-out results/ch00.impulse.csv \
  --rate 48000 --seconds 5 --pre-seconds 1 --post-seconds 1

# 2) 每通道 fit（也可直接对 results 目录 merge）
./khaen-calibrate fit --analysis results/ch00.analysis.json --out results/ch00.calibration.yml --max-stages 8

# 3) 合并 + 相位抵消检查（pair 必须是“同源信号 + 互补 LR4 分频”，fc 等于 profile 某 HP/LP）
#    当前 4.1.2：FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7（ch6 空）；只有 FL/FR 与 SUB1 是真分频对
./khaen-calibrate merge --dir results --out calibration.yml \
  --names FL,FR,SL,SR,HFL,HFR,SUB1 --layout 4.1.2 --crossovers 80 \
  --pairs FL:SUB1:80,FR:SUB1:80 --max-stages 8
# 5.3.2（NEEDS_HW，十只/三 SUB 接好后才跑）：
# ./khaen-calibrate merge --dir results --out calibration.yml \
#   --names FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR --layout 5.3.2 --crossovers 80 \
#   --pairs FL:SUB1:80,FR:SUB1:80 --max-stages 8
# 注意：高度（HFL/HFR）全频、无 2 kHz 分频；中置 C 按设计与主箱相同为全频（D3 2026-11-17），
#       当前无中置 → C 的 HP80/低频重定向/C:SUB pair/C 测试全部 deferred（NEEDS_HW，不在 P1 验收内）；
#       C/SL/SR 低频不汇总 SUB → 不能当 pair。
```

- 逐通道 sweep 文件也可单独生成：`sweep --out sweep-ch0.raw --channels 22 --channel 0`、`sweep-all --dir sweeps --channels 22`。
- 测量链公共延迟不写入校准：`delay_samples` 是**施加到该通道的延迟补偿** `D_i = round(max_j t_j - t_i)`（t=实测到达），
  已归一化到 min=0；`D=0` 的通道是**到达最晚**的参考通道，`reference_index` 应指向它（方向定义见 CONTRACT §3.1）。
- 块长：测量/分析不依赖块长；64/128 合法，但板端稳定测量建议 256；**32 帧未验证，不作为验收依据**。
- **4.1.2 运行态 mapping（D8 2026-11-17 裁决 = 最终）**：`FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7`（Line Out 8；槽 6 空）。
  Line Out 10（槽 9/ch9）只是当时 Line Out 8 不出声的**临时测试接法，不是设计候选**：不得再写待核实/二选一，不要生成 index=9 的校准。

## 输出契约（khaen-calibration/v1 与 v1.1）

```yaml
schema: khaen-calibration/v1.1
sample_rate: 48000
reference_index: 3          # 相对延迟=0 的通道
layout: 4.1.2
created_unix: 1794736489
tool: khaen-calibrate
version: 0.1.0
note: merged multi-channel calibration
channels:
- index: 0                # 播放设备通道号
  name: FL                # 角色 id（必填、唯一）
  delay_samples: 20       # 相对修正（整数样本，min=0）
  polarity: normal        # normal|invert
  filters:                # ≤32 段。v1 只有 Peaking；v1.1 还可以是 shelf（type 不能省）
  - {freq: 80.0, q: 1.2, gain_db: -6.0}                              # type 省略 = peaking
  - {type: high_shelf, freq: 4000.0, q: 0.7, gain_db: -2.0}          # q = RBJ slope S
```

新写出的文件是 `schema: khaen-calibration/v1.1`。v1 文件仍只接受 peaking，且 `q` ∈ [0.1, 10]。

- peaking 的 `q` 是 Q。v1.1 要求有限且 ≥ 0.1，窄模态可以 >10。走 channelmap 时板端仍只接受 [0.1, 10]。
- `low_shelf` / `high_shelf` 的 `q` 是 RBJ shelf slope S，不是 Q，必须在 [0.1, 1.0]。`fit` 默认 S=0.7。
- CamillaDSP 片段：peaking 写 `q`；shelf 必须写 `slope: 12*q`（dB/oct）。S=1 就是 slope=12，这是 CamillaDSP 的上限。shelf 的 Q 型与 S 型是不同参数化（无固定容差），禁止混用（P2-5/P2-6）。

同时输出 `<out>.camilladsp.yml`：`filters`（Delay/Biquad）+ `pipeline`（每通道 Filter step）+
`channelmap_roles`（反相提示），可直接合并进渲染后的 CamillaDSP 配置。`<out>.validation.json` 为抵消检查报告
（80–20k 与分频点，`margin_db = sum_db - max_single_db`，<-6dB 为 FAIL、<-3dB 为 WARN）。
**契约口径（2026-11-17）**：这个“全通道求和”判据假设所有通道同内容播放、忽略 render 层延迟/增益，无物理意义，
只作 warning，**不得进入 overall_status/overall_pass**（C-P1-5）。工具当前仍会据此置 FAIL，验收时按 ACCEPTANCE 复核并在报告注明。

## 目标曲线（D4，2026-11-17 裁决 = 最终）

`--target` 只用于拟合/报告，**不写入校准 YAML**。内置目标曲线只有两条：

| id | 定义 | 说明 |
| --- | --- | --- |
| `flat` | 20/1k/20k 全 0 dB | **默认**；仅报告/拟合，不改输出电平 |
| `tilt10` | **20 Hz 0 dB → 20 kHz −10 dB，对数频率常数斜率**（≈−1.0034 dB/oct；点表可用 20/200/2000/20000 → 0/−3.32/−6.64/−10 dB 或由工具生成） | 参考增益**独立于曲线形状**（曲线只定义相对形状，绝对参考电平另设） |

**旧 `tilt`（−3 dB@16k）已 superseded**（2026-11-17）：不再作为默认或可用目标曲线，不得再引用为 tilt10。
其它曲线只能通过**导入扩展**（`--target @file.yml#id`）使用，不设默认、不进交付验收。

> **已落地（task-103，2026-11-17）**：`calibrate/target-curves.yml` 已更新为仅 `flat`+`tilt10`（旧 `tilt` 删除，`--target tilt` 返回错误）；`cargo test` 覆盖 tilt10 斜率与旧 tilt 拒绝。
> 归一化：报告统一以 1 kHz 为 0 dB 参考（与曲线参考增益独立）。

## 增益/boost 与 headroom（D7，2026-11-17）

房间校准 PEQ/目标拟合**允许 +3 dB boost**，同时必须满足：

- 单段 `filters[].gain_db > 0` ≤ **+3.0 dB**（producer 硬帽；超过应钳制/不输出，不应写进校准文件）；
- 每通道正向总和 Σmax(gain,0)：>6 dB warning、>12 dB 拒绝（总增益帽，不含 `channels[].gain_db` trim）；
- **削波/headroom**：应用后**预测最大累计响应峰值 ≤ −6 dBFS**（headroom ≥6 dB）；实测/扫频/节目素材峰值 ≤ −1 dBFS；
  不满足时减少 boost 或加负 trim，**不得**靠降低播放电平规避（节目峰值不可控）；
- 仍不可填（需要 >+3 dB）的谷列入 `unfilled_dips`，不作为 FAIL；T3 目标偏差 ±3 dB 的统计频段包含 ≤+3 dB boost 区。

> **已落地（task-103，2026-11-17）**：`--boost-cap-db` 默认 **3**（`>3` 钳 3、`<0` 归 0；`--boost-cap-db 0` 仍可显式 cut-only）。`fit` 的 `.optimizer.json`（每通道）与 `merge` 的 `.report.json`（`boost_headroom[]` + `boost_cap_db`/`clip_risk_any`）记录：`boost_filters`、`applied_boost_db`、`total_boost_db`、`max_predicted_gain_db`、`predicted_peak_dbfs`、`headroom_db`、`clip_risk`、`headroom_reference_dbfs`（默认 -18 dBFS，与总正向帽 12 dB 配套：-18+12=-6；可在报告中追溯）。`clip_risk=true` 时 `merge` 的 `overall_status=FAIL`/`overall_pass=false`。

## 离线示例（无硬件）

```bash
./khaen-calibrate simulate --dir examples/offline --channels 4 --delays 0,3.5,10,20 \
  --polarities 1,1,1,1 --names FL,FR,SL,SR --pre-seconds 0.5
./khaen-calibrate merge --dir examples/offline --out examples/offline/calibration.yml \
  --names FL,FR,SL,SR --layout synthetic-4 --crossovers 80,2000
```

离线结果：`examples/offline/calibration.yml`（PASS，worst 9.03 dB）、`.camilladsp.yml`、`.validation.json`；
`examples/offline-invert/` 为反相通道示例（校准后仍 PASS，说明极性修正生效）。详见 `OFFLINE-VALIDATION.md`。

## 板端验证（24h soak 结束后）

1. MOTU 1:1 → UMIK 实测 4 通道（FL/FR/SL/SR 或实际布局），确认 delay/polarity/phase 与物理一致；
2. `merge` PASS 后，把 CamillaDSP 片段并入 channelmap 渲染配置，用 64/128 帧各跑一次播放/测量；
3. 真实分频点/重叠频段做合成响应复测；H16 到货后再测 16ch 容器。

**验收前置（2026-11-17）**：
- CueMix/MOTU 主输出 trim（`kMainTrim`）置 0（否则 −28 dB 会把信号/SNR 整体压低，T2 SNR 20 dB 很可能同源）；T1 固定 −12 dBFS；
- UMIK-1 用 **90° 校准文件、麦轴朝天花板**（听音位多声道；0° 仅单只近场），报告记录 cal 文件 sha256 与方向；
- **T3 应用后复测必须做（K4）**：禁止 `PASS_OPEN_LOOP`/免测；补 **T6 holdout**（验证点不参与拟合）；
- T4 pair 口径：只保留真实互补 LR4 分频（4.1.2：FL:SUB1:80、FR:SUB1:80；高度/环绕/中置不参与）；
- `negative_controls` 5/5 才可能 PASS（未跑 → INCOMPLETE）；
- 细节见 `calibration-spec/ACCEPTANCE.md`（2026-11-17）与 `BOARD-T1-T5-RUNBOOK.md`（v15）。

## P0 修复（2026-11-15，按 calibration-spec ACCEPTANCE / review）

- **公共时间基准**：新增 `measure-multi`：一次 arecord 连续采集，所有通道扫频在同一会话内按槽位顺序播放（槽间静音 ≥0.8s），再按槽位切分分析；跨通道 delay/相位直接可比（单通道 `measure` 仅用于单路/T1，不可用于跨通道相位）。
- **交叉对判据**：`merge --pairs "FL:SUB1:80,FR:SUB1:80"` 输出 `<out>.pairs.json`：Δφ(fc)≤30°、CD(fc)≥-3dB、minCD≥-6dB、notch≥-6dB、ripple≤3dB。
  `band_dev`（原 `target_dev`，实为“相对自身带内均值”）与 `level_diff(fc)` 只作 warning，不作硬门；pair 必须是**同一源信号 + 互补 LR4**，
  fc 必须等于 profile 某 HP/LP 频率；高度/环绕/中置（effect channel）不构成 pair（C-15）。
- **负向用例自检**：cargo test 含 N1 反相、N2 错延迟（**0.25 ms = 12 samples @2 kHz，=1/(2·fc)**）、N3 单边 +6dB PEQ、N5 SUB 2ms 延迟均判 FAIL；
  N4 映射错由 `measure-multi --verify-mapping`（多通道 1:1 环回采集）检测，不属于 pair checker 项。
- **64/128 帧**：`measure/measure-multi` 支持 `--period-size/--buffer-size`（默认 128/512；范围 32..4096，设备拒绝时 aplay/arecord 报错透传）。
- **极性链**：`analyze/measure-multi` 支持 `--polarity-offset normal|invert`。注意范围：T1 电学环回只标定 **MOTU DAC/ADC 段**极性，
  不代表声学链（UMIK+功放+音箱）的绝对极性；声学绝对极性按 UMIK 手册约定（正压→正样本）或用已知极性的近场参考麦标定并记录（C-P1-12）。
- **UMIK-1 48k**：`--mic umik` 且 rate≠48000 直接报错（96k 需换麦克风/漂移补偿，当前不支持）。
## Round-2 P0 修复（2026-11-15）

- **验证极性只取校准字段**：merge/pair 的合成响应用 `channels[].polarity`（normal/invert）修正，不再乘实测 `meas.polarity`；实测 FR 相位本身已含物理极性。N1 用真实数据形态（相位 180° + 极性 invert）验证：未校准时 FAIL、校准 invert 后 PASS。
- **arecord 也参数化**：capture 侧同样带 `--period-size/--buffer-size`（与 aplay 一致，默认 128/512）。
- **measure-multi 切窗**：按 `--playback-offset-seconds`（默认 0.35s，arecord 先起偏移）生成名义槽位，再用互相关**逐槽检测扫频实际起点**后切窗；尾部余量 ≥1s，不再因固定偏移截尾；日志打印每槽 nominal/actual_start/lag。
- **FR 导出**：`*.fr.csv` 头注明相位参考，并新增 `re,im` 复频响列；analysis JSON 含 `phase_ref`/总 `delay_samples`，外部 checker 可独立重建 pair 响应（绝对相位 = phase_deg - 360·f·delay_samples/fs）。
## P1 快速修复（P0 关闭后，2026-11-15）

- `reference_index` 改为**播放通道号**（`channels[k].index`），不再用文件循环下标；
- 单通道 `fit` 产物 `delay_samples=0`（中间产物，merge 才做相对归一化）；
- 默认角色名改大写 `CH<index>`（符合角色 id 约定；真实布局请显式传 `--names FL,FR,...`）；
- PEQ clamp 收敛到契约范围：freq 20–20k、q 0.1–10、gain ±18 dB；`--max-stages` ≤22（producer 预算；契约校验上限 32，C-P2-4）；
- CamillaDSP 片段新增真正的极性实现：`Gain` filter `{gain:0, inverted:true}` 进入 pipeline（不再只是 channelmap 提示）。
- 仍未做（后置）：1/3 oct 纹波平滑改为频率窗、`--pairs` 与 `--crossovers` 联动、measure-multi 诊断入 JSON、带限/重复 SNR、`khaen-calibration-report/v1` 完整报告（tests[]/holdout/mic 字段）。
- **2026-11-17 B2 已知缺口**（详见 `calibration-spec/review` 与 `scratch/lead/doc-audit/B2-EVIDENCE.md`）：
  ① `global_validation` 仍进 overall（C-P1-5）；② `negative_controls` 不参与 overall_pass（C-P1-6）；
  ③ 无 `capture_peak_dbfs`/带限 SNR 硬门（C-P1-11）；④ 无 `--inject-*`（C-P1-10）；⑤ 无 `--mic-cal`（C-18，落点待定）。
- **2026-11-17 已落地（task-103）**：target-curves.yml=flat+tilt10（旧 tilt 删除）、`--boost-cap-db` 默认 3/钳制、fit/merge D7 boost/headroom 报告字段。**D8**（4.1.2 SUB1 固定槽位7/禁用槽9）属 calibration-spec/板端部署范围，不在本工具代码；runbook/tools board README 已按 D8 写死。
## Round-4 片段极性修复（2026-11-15）

- CamillaDSP 片段的极性 `Gain {gain:0, inverted:true}` 现在在 **pipeline push 之前** 加入 names；**纯反相通道也会生成 pipeline**（hand180 实测 pipeline 含 `cal_ch1_pol`）。
- `channelmap_roles` 改为 **advisory**（`apply:false` + 说明），与 pipeline 二选一，避免双反相；README/片段 note 写明消费路径。
- `--max-stages` 在 fit/merge 均已 `min(32)`。
## report 小项（Round-6 复审，2026-11-15）

- `merge --chunksize 64|128` 写入 `report.environment.chunksize`（不再硬编码 0；T5 用真实值）；
- `merge --negative-controls <json>` 把 N1–N5 结果并入 `report.negative_controls`（未给→`status: not_run`）；
- 未给 `--pairs` 时：`pair_validation.status=not_run`、`overall_status=INCOMPLETE`、`overall_pass=false`（不再假绿）；
- `overall_status`：PASS / INCOMPLETE / FAIL。

## 多遍扫频 / 鲁棒极性 / 时钟漂移（2026-11-16）

单点判据 `sign(IR[argmax|IR|])` 在听音位会被近反射带偏：峰的位置可以不变，符号却在正负之间翻。新测量把这个判据留在 `polarity_estimators.ir_peak` 里，**不让它否决**直达声和低频相关。

不带 `--repeats`（默认 1）且不带 `--robust-polarity` 时，`polarity` 仍是原来的 IR 峰符号，已有字段数值不变；诊断字段会额外写出投票结果。`--repeats`>1 或 `--robust-polarity` 时，`polarity` 改为投票结果。

主票是三路，三者同号即为 `polarity_confidence=high`：

1. `direct_window`：在全局峰之前，找 |IR| 包络上第一个 ≥ 峰值 30% 的点（首达），对 `[t0, t0+2 ms]` 求和取符号。
2. `lowband`：100–500 Hz 带限互相关的符号（扫频达不到这段时改用扫频自身通带）。
3. `matched`：全带互相关的符号。

`phase_vote`（去掉分数延迟后看 100/200/300/400 Hz 的 `cos(phase)`）和 `ir_peak` 只记录。主票不齐时才把 `phase_vote` 算进多数决，这时置信度最高是 `low`。跨遍再投一次：有一遍反号则整体为 `low`。

| 参数 | 默认 | 作用 |
| --- | --- | --- |
| `--repeats N` | 1 | 每槽把同一条扫频连放 N 遍，1..=8。N=1 时槽长与旧版相同 |
| `--repeat-gap SEC` | 0.2 | 遍与遍之间的静音，只在 N>1 时插入 |
| `--robust-polarity` | 关 | N=1 时也把投票结果写入 `polarity` |
| `--drift-correct` | 关 | 用测到的 ppm 做 sinc 重采样后再分析 |
| `--max-ppm` | 100 | 超过，或相对标称栅格的残差 > 0.25×间隔，则 `onset_invalid=true`、`clock_ppm_ok=false`，该遍不进延迟/ppm 聚合 |
| `--capture2 DEV` | 无 | 第二路 arecord 与主采集同时起，再 aplay。参考结果写 `<name>-chXX.analysis.json`，并在主 JSON 的 `reference` 里摘要 |
| `--capture2-channels/channel/name` | 20 / 0 / REF | 参考麦通道。`measurement_role=reference` 的文件 merge 会跳过 |
| `--require-high-confidence` | 关 | merge 时置信度不是 high 就报错 |
| `--allow-sub-polarity-from-listening-position` | 关 | 否则 SUB（名字以 SUB 开头，或扫频截止 ≤200 Hz）在听音位置信度为 unknown 时**不写** polarity |
| `--sub-reference-nearfield` | 关 | 参考麦放在低音炮近场时，SUB 极性改跟参考麦 |
| `--cuemix-guard URL --cuemix-token TOKEN` | 关 | 测量前 `POST /api/motu/direct-mute {"mute":true}`，结束后恢复。只接受 `http://host:port` |

`clock_ppm = (实测重复间隔 / 标称间隔 − 1) × 1e6`。有效起点少于 3 个时不估计 ppm（`clock_ppm` 为空、`clock_ppm_ok=false`）。`--drift-correct` 把采集按这个 ppm 拉回标称时钟后再算延迟。槽间间隔少于 3 个时 `session.diag.json` 的 `slot_clock_ppm` 为空。`khaen-calibration/v1` 的必填字段不改；新字段只出现在 analysis / diag / report。

选峰前对采集和扫频做带通（全带 80–8000 Hz，窄带按扫频起止收进 90%），再用归一化相关 `|c|/sqrt(Σseg²·Σsrc²)` 取峰，并丢掉偏离重复栅格 5% 或 2000 样本以上的孤立峰。带通峰会偏早，滞后在 ±1200 样本内收回全带相关最大的位置。参考麦在 `clock_ppm_ok=false`、`onset_invalid=true`、`snr_db<20` 或极性 `unknown` 时写入 `reference.valid=false` 和主 JSON 的 `reference_invalid`，不改主麦 `polarity`。只有参考结果有效且和主麦不一致时，才跟随参考麦并把置信度降为 `low`。

双设备采集（`--capture2`）且没有显式 `--playback-offset-seconds` 时，aplay 前的等待和名义偏移都是 1.0 s，不再用单设备的 0.35 s。单设备仍是 0.35 s。

SUB 的听音位极性不可当校准依据：20–130 Hz 落在房间模态里，同一只低音炮换个麦的位置符号会稳定地相反。报告里看 `sub_polarity_basis`。有近场参考麦时加 `--sub-reference-nearfield`。
