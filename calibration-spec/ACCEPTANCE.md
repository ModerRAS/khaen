# 校准验收口径（延迟/极性/PEQ、应用后复测与 holdout、合并响应/相位抵消）

> 配套 CONTRACT.md（tspi-calibration/v1 / v1.1）。本文件给**可执行判定标准**：测什么、怎么算、阈值多少、失败怎么报。
> 所有阈值默认面向 48 kHz / 4.1.2 与 5.3.2（两布局共用；布局差异见 §6.3）；96 kHz 时以时间为准（采样点阈值按比例换算）。
> 版本：**2026-11-17（B2 修订）**。依据 `scratch/lead/doc-audit/CONSOLIDATED.md` §3.1/§4 B2（C-13/14/15/16/17/18 + B2 组 P1/P2），
> 证据 `scratch/lead/doc-audit/B2-EVIDENCE.md`。
> 口径基线：**K2** 高度全频/无 HP-LP/无人工延迟；**K4**（2026-11-16 23:37）完整校准=应用后复测 + holdout，T3 保留；
> **K7** loopback 实测（L/R 已验证、8/9=内部 USB loopback）。
> **已裁决（2026-11-17，Lead 确认）**：**D8** 4.1.2 SUB1 固定槽位 7/Line Out 8（Line Out 10/槽 9 仅为历史临时测试，不是候选）；**D3** 5.3.2 中置按设计与主箱相同为全频、中置相关全部后置（deferred）；**D7** 允许 **+3 dB boost**（含削波/headroom/总增益帽规则）；**D4** 目标曲线仅 flat + tilt10（旧 −3 dB@16k superseded）。见 §10.1。
> 仍待裁决/待 pilot：见 §10.2。

## 0. 测量前置条件（否则结果无效，直接判 FAIL / INCOMPLETE）

### 0.1 单座位、话筒方向与校准文件（C-18）

1. 本验收是**单座位（甜点）验收**；报告中必须记录麦坐标（相对甜点的前后/左右/高度，或 x/y/z）与麦高、照片/示意。
   多点覆盖不是本验收的默认要求；需要多点时用 optimizer 的 `--positions` 另立多点验收（MIMO/holdout 见 §0.6）。
2. 多声道/环绕在听音位测量：用 **UMIK-1 90° 校准文件、麦轴垂直朝天花板**（按 miniDSP 厂家说明）。0° 仅用于**单只音箱近场/轴上**测量。
   报告必须记录：`mic{model, serial, cal_file, sha256, orientation}`。
3. 未加载/未记录话筒校准文件时，报告必须声明“未应用话筒校准，目标曲线偏差只在相对意义上有效”，**不得声称 ±3 dB 目标达成**；
   工具 `--mic-cal` 尚未实现（C-18，落点待定），当前只能在 `recompute.py` 等后处理侧应用并在报告标注。
4. 话筒个体/方向误差是**共模误差**：T2 的“同麦同位置重复性”看不出它，不能当“测量已校准”的证据。

### 0.2 时间基准与时钟漂移（C-P1-2，裁决 4）

1. **相对相位必须在同一次采集会话内获得**：所有通道的扫频在同一个 arecord 采集中依次播放（通道间留 ≥0.5 s 静音），
   或使用同一 ADC 时钟、同一触发点分次采集并显式对齐。分次采集且没有公共时间基准 → 相位/延迟比较无意义。
2. 若用 UMIK-1（异步 USB 时钟），**必须同时报告** `clock_ppm` 与 `clock_ppm_uncertainty_ppm`；
   只有 `|clock_ppm| > 2 × uncertainty` 才可称“漂移”并允许 `--drift-correct` 重采样。
   `|drift| ≤ 100 ppm` 只是起点锁定的 sanity 上界，**不是**漂移证据；3 遍/2 个间隔的 ppm 只能当参考值，不得引用 REF 的 +12.84 ppm 作“已修正”。
3. N=1（不重复）时漂移不可估计，**不得**启用漂移重采样。带限/窄带通道按自身扫频通带统计。
4. “补偿后 10 kHz 残余相位 ≤15°”的时间跨度必须写死：默认按 10.4 s 跨度（3 遍 × 5 s，槽间 0.2 s）且要求 σ_ppm ≤0.4 ppm；
   或使用 ≥5 遍 / ≥60 s 数据。未达到重采条件 → 该指标 NOT_APPLICABLE，不得伪造 PASS。

### 0.3 渲染层与校准层

测量时渲染层处于 **direct 1:1**（或按 CONTRACT §2.2 mode=through_render 记录 render_offsets）；
校准层（delay/polarity/PEQ）必须清零（测量的是“加校准前”的物理链路）。

### 0.4 激励电平与 CueMix 主输出 trim（C-16）

1. **T1 电学环回固定驱动 −12 dBFS**（电学路径不削波即可；不得再用 −30 dBFS）。声学 T2/T4 默认 −30 dBFS；
   若带限 SNR <25 dB，提高到 −12 dBFS 重测（仍须不削波）。
2. 测量前把 CueMix / MOTU 主输出 trim（`kMainTrim`）**置 0**；无法置 0 时必须记录实际值并说明补偿方式。
   已知 −28 dB trim 会把环路/声学信号整体压低 28 dB，−30 dBFS 激励下 T1 只能测到 −53 dB 的噪声底（正确做法见 §3）。
3. 报告必须记录 `drive_level_dbfs`、`k_main_trim_db`、`noise_floor_dbfs`（本机采集噪声底，实测）。

### 0.5 削波与 SNR 有效性门（C-P1-11）

1. 采集峰值 ≤ **−1 dBFS**（任何通道无数字削波），报告写 `capture_peak_dbfs`。
2. 每次测量 SNR：200 Hz–8 kHz ≥ **25 dB**；带限通道用自身扫频通带（报告 `snr_band_hz`）≥25 dB。否则重测（提高电平/降噪）。
3. 工具目前没有 `capture_peak` / 带限 SNR 硬门（见 B2-EVIDENCE “工具待办”）；缺失这些字段的测量**不可判定**，不得记 PASS。

### 0.6 holdout（K4，2026-11-16 23:37）

1. **设计点（design）与验证点（holdout）必须分离**：拟合（delay/polarity/PEQ/目标曲线）只允许用 design 点；
   holdout 点**不参与拟合**，只用于应用后复测的独立验证。
2. holdout 至少 1 点（建议 2–3 点：甜点 ±0.3 m 前后/左右，或不同麦高），报告逐点残差与汇总（均值/最大值）。
3. holdout 未测量/未报告 → 总体验收 **INCOMPLETE**，禁止写 PASS（见 §9）。
4. MIMO/上混的 holdout 指标单列（见各自验收），不得用校准拟合点冒充足。

### 0.7 T3 保留（K4）

**T3（应用校准后逐通道复测）是必须项。** 完整校准 = 应用后复测 + holdout；
禁止 `PASS_OPEN_LOOP`、禁止免测签字。旧文档“校准产物只出文件、不要求应用后复测”（E2E-ACCEPTANCE / PROJECT-STATUS，2026-11-16 前）**作废**。

### 0.8 环境记录

设备名、通道数与宽度（MOTU 22ch 设备 vs 布局有效槽位）、采样率、chunksize（目标 64/128）、温度/备注；
报告必须可复现（附命令、原始数据 sha256）。

**4.1.2 运行态 mapping（D8，2026-11-17 裁决 = 最终）**：`FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7`（Line Out 8；槽 6 空）。
Line Out 10（槽 9）只是当时 Line Out 8 不出声的**临时测试接法**，不是设计候选；不得再写“待核实/二选一”，也不得把 index=9 写进校准文件。

## 1. 分层验收（T0–T6）

| 层 | 内容 | 必须 |
| --- | --- | --- |
| T0 | YAML 契约校验（CONTRACT §5；Rust 主入口 + Python 独立复核） | 是 |
| T1 | MOTU 环回：逐通道映射、隔离、延迟/极性/FR 精度 | 是 |
| T2 | UMIK-1 实测：重复性、漂移、SNR | 是 |
| T3 | 应用校准后逐通道复测（延迟/极性/PEQ） | 是（K4） |
| T4 | **合并响应 / 相位抵消**（交叉点、多低音、整组） | 是 |
| T5 | 实时链路（64/128 帧、0 xrun、不破坏 RT 线程） | 是 |
| T6 | **holdout 验证**（验证点不参与拟合） | 是（K4，§5.2） |

任一层 FAIL、或 T3/T6 未执行、或 holdout 未报 → 整份校准不可交付；报告必须给出失败项与原始数据引用。

## 2. 符号与计算定义

- `H_c(f)`：通道 c 的复频响（含幅度 dB 与相位 deg；相位已去掉测量链公共延迟/线性相位项，保留通道间差）。
- 频响网格：对数 20 Hz–20 kHz，≥ 1/24 oct；检查用 1/3 oct 平滑（除非另注）。
- **交叉对（pair）**：**同一源信号、互补分频（目标 LR4）的两个通道**；标称交叉频率 fc 必须等于 profile 中某 HP/LP 的频率。
  **effect channel 不构成 pair**：高度/环绕/中置等与 SUB 内容不同、或低频未重定向到 SUB 的通道，按单通道验收（极性 convention、残余延迟、文件后端映射证据）。
  多低音按 §6.2 的“SUB 组等效 H_sub”与主箱成对。配置需显式列出 pairs 与 fc。
- **证据等级**：`evidence: predicted`（direct 测量 + 模型预测，非应用后实测）或 `measured`（应用后复测）。
  T4 的 pair 指标默认是 predicted，**不得**当应用后实测；T3/T6 必须是 measured。
- 相位差：`Δφ(f) = wrap(arg H1(f) - arg H2(f))`，单位 deg，wrap 到 (-180,180]。
- 相干和：`S(f) = H1(f) + H2(f)`。
- **抵消深度**：`CD(f) = 20log10|S(f)| - 20log10(|H1(f)| + |H2(f)|)`（≤0 dB；0=完全同相，-∞=完全抵消）。
- 纹波：`ripple = max_f 20log10|S(f)| - min_f 20log10|S(f)|`（在指定带宽内）。

## 3. T1 — MOTU 环回精度（工具/链路自检）

用 1:1 环回（MOTU 输出→输入；或数字环回）逐通道验证工具本身。**范围**：T1 电学环回只标定 MOTU
“播放 DAC + 采集 ADC”这一段的极性/延迟/映射；**不代表声学链（UMIK + 功放 + 音箱）的绝对极性**。
声学绝对极性按 UMIK 手册约定（正压→正样本）或已知极性的近场参考麦标定并记录（C-P1-12）。

| 指标 | 阈值 | 方法 |
| --- | --- | --- |
| 逐通道映射 | 目标通道 SNR ≥ 30 dB；其它采集通道相对指定输入 ≤ −60 dB（模拟）/ ≤ −80 dB（数字），**且比本机采集噪声底低 ≥10 dB**确定测量有效 | 播放通道 k 扫频（**−12 dBFS**，`kMainTrim=0`），检查采集各通道能量；报告 `noise_floor_dbfs` |
| 延迟准确度 | 注入 0/1/5/10 ms → 测得误差 ≤ 0.5 sample | 每档 5 次取均值/最大值；注入方式见下 |
| 延迟重复性 | 5 次标准差 ≤ 0.2 sample | 同一通道重复 |
| 极性检测 | 故意反相 → 100% 判 invert；正常 → 0 次误判（10 次） | 注入 Gain −1 |
| FR 精度 | 注入已知 PEQ（例：1 kHz +6 dB Q1）→ 测得 FR 与理论差 ≤ 0.5 dB（100 Hz–10 kHz，1/24 oct） | 理论=RBJ 双二阶级联 |
| 相位精度 | 去掉整数延迟后，与理论相位差 ≤ 5°（100 Hz–5 kHz） | 同上 |

- **注入方式（C-P1-10）**：工具尚无 `--inject-delay-samples/--inject-peq`。当前用一份**冻结的 CamillaDSP 测试配置**做注入，
  在 `render_offsets` 记录注入量，报告写配置 sha256 与注入值；不得口述“注入了 0/1/5/10 ms”而无配置证据。
- **测量链公共延迟/极性**：T1 只覆盖 MOTU 段；后续声学测量若要统一极性，用 `--polarity-offset` 并按上面“范围”声明，不得称“整体极性已标定”。

### 3.1 T1 partial（硬件环回线不足时的降级规则）

当板端环回线不足时，T1 允许记为 **partial**。环回不必是 1:1：可用 `--loopback-map play:capture`（1-based，可多对，
例如 `9:7` 表示 MOTU out9→in7）覆盖任意可安全接线的输出/输入对。必须满足：

- 覆盖对：至少覆盖 **1 个映射对**（推荐 ≥2 对；仅 1 对时必须在报告声明“单路径抽检”）。覆盖的 output/input 必须逐通道列出（1-based）。
- 覆盖项：映射（播放 outN 的最大能量必须出现在指定 inM；其它采集通道相对 ≤ −60 dB **且低于实测噪声底 ≥10 dB**）、
  链路延迟/极性抽检（0/1/5/10 ms 注入，误差 ≤0.5 sample、重复性 ≤0.2 sample）必须在覆盖对上完成。
- **电平/trim**：驱动固定 −12 dBFS；记录 `kMainTrim`（最好置 0）与 `noise_floor_dbfs`；未记录 → T1 判 INCOMPLETE。
- 非 1:1 环回（如 out9→in7）只验证该物理路径的映射/隔离/延迟/极性，**不代表 1:1 通道映射已验证**，报告 claim 必须写明。
- 未覆盖通道的替代证据：①文件后端渲染校验（通道映射/采样值/上混路径，不依赖声卡）；②T4 声学测量中每个未覆盖输出至少出现在一个被测量的 pair 或单通道 FR 中（并在报告列出对应证据文件）。
- **硬件路由前提（必须在报告记录）**：T1 前记录 CueMix/矩阵路由状态（截图 + 文本，如 in7→out7、in8→out8、
  in7→out9、其它 input→output、out7/out8 downstream 是否断开）；T1 时 out7/out8 等被环回牵涉的输出不得接后级；
  最终 insert 模式必须关闭全部 input→output 直通（in7→out7、in8→out8 等），否则硬件直通与 USB DSP 双路径并存；
  报告需记录该状态与关闭证据。
- 报告字段（tspi-calibration-report/v1 或最终验收报告）必须包含：
  ```json
  "t1": {
    "status": "partial",
    "loopback_map_1based": [{"play": 9, "capture": 7}],
    "covered_outputs_1based": [9],
    "covered_inputs_1based": [7],
    "uncovered_outputs_1based_5_3_2_10ch": [1, 2, 3, 4, 5, 6, 7, 8, 10],
    "uncovered_outputs_1based_4_1_2_22ch": [1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
    "drive_level_dbfs": -12,
    "k_main_trim_db": 0,
    "noise_floor_dbfs": -95.0,
    "isolation_max_other_db": -62.0,
    "isolation_margin_over_floor_db": 33.0,
    "loopback_mapping": "pass",
    "cuemix_routing": {"in7_to_out7": "on", "in8_to_out8": "on", "in7_to_out9": "none", "other_input_to_output": "none", "out7_out8_downstream": "disconnected"},
    "final_insert_direct_routes": "disabled",
    "mitigation": ["file_backend", "t4_acoustic"],
    "claim": "partial: electrical loopback out9->in7 only; 1:1 mapping and other channels not verified; remaining outputs via file backend + T4 acoustic evidence"
  }
  ```
  （`uncovered_outputs` 按布局给对应 1-based 示例：5.3.2=10 路域；4.1.2=22ch 设备域，有效槽位 1-based 为 FL1 FR2 SL3 SR4 HFL5 HFR6 SUB1=8（槽 7 空）。）
- **T1 partial 不等于 T1 PASS**：在补齐全部通道环回前，T1 状态只能写 partial；若 T2–T6 全过且未覆盖通道有上述替代证据，
  总体验收可写 `PASS_WITH_T1_PARTIAL`（§9），并必须在报告开头写范围声明；不得写成 `T1 PASS`。
- T1 partial 不豁免 T4/T6；T4 仍须用声学测量覆盖实际全部输出（含未环回通道）。

## 4. T2 — UMIK-1 房间实测重复性

同位置连续 3 次完整测量（麦方向/校准文件见 §0.1；trim 见 §0.4）：

| 指标 | 阈值 | 备注 |
| --- | --- | --- |
| 通道延迟重复性 | 3 次极差 ≤ 0.3 sample | engineering default，待 pilot（C-P1-3） |
| 极性 | 3 次一致（且与物理接线一致） | SUB 听音位符号不作依据 |
| 幅频重复性 | 1/12 oct 平滑、100 Hz–10 kHz，任一点极差 ≤ 0.4 dB | 房间模态区/位置微变可能超限；先 pilot 再收紧 |
| 时钟漂移 | 报告 `clock_ppm` + `clock_ppm_uncertainty_ppm`；只有 \|ppm\|>2σ 才算漂移；补偿后 10 kHz 残余相位 ≤15°（跨度见 §0.2） | N=1 不可估计，不得重采样 |
| SNR | 200 Hz–8 kHz ≥ 25 dB（带限通道用自身通带） | 与 §0.5 同一口径 |
| 削波 | 峰值 ≤ −1 dBFS | 同上 |

T2 的数值阈值都是 **engineering default，待 pilot**：先用现有 3 次 T2 数据 + 加测 5–10 次给出极差/σ 再定稿（C-P1-3）。
当前第一次 T2（SNR≈20 dB）很可能受 −28 dB trim 影响（§0.4），必须按新条件重跑后才可作证据。

## 5. T3 — 应用校准后逐通道验证（必须，K4）

应用 YAML 到目标 profile/配置后，按同一测量流程复测：

| 指标 | 阈值 |
| --- | --- |
| 应用链校验 | `tspi-channelmap validate/render` rc=0；独立 CamillaDSP 配置 `camilladsp --check` rc=0；记录 apply 前后 diff/sha256 |
| 残余延迟 | 实测延迟 − 目标延迟（参考通道归一）≤ 0.5 sample；且 ≤1 sample 为 hard fail |
| 极性 | 与 YAML 一致（100%），参考通道 normal |
| 幅频校正效果 | 应用后实测 vs “原始实测 + 校准模型预测”偏差 ≤ 1 dB（100 Hz–10 kHz，1/12 oct） |
| 目标偏差 | 相对目标曲线偏差 ≤ ±3 dB（100 Hz–10 kHz；**允许 ≤ +3 dB boost 填谷**，D7）；仍不可填的 > +3 dB 谷列入 `unfilled_dips` 清单、不作为 FAIL；参考点 1 kHz 归一化 |
| boost/headroom（D7） | 单段 `filters[].gain_db>0` ≤ **+3.0 dB**（producer 硬帽）；每通道正向总和 ≤6 dB（>6 warning）/≤12 dB（>12 拒绝）；**预测最大累计响应峰值 ≤ −6 dBFS**（headroom ≥6 dB）；实测峰值 ≤ −1 dBFS（§0.5）；不满足则减少 boost 或加负 trim |
| PEQ 实现 | 应用 PEQ 前后的实测差 vs RBJ 理论差 ≤ 0.75 dB（100 Hz–10 kHz） |
| shelf 实现 | 见 §5.1（type/slope 正确、`camilladsp --check` 通过） |
| 削波 | 扫频/节目素材峰值 ≤ −1 dBFS；任何通道无数字削波 |
| 通道隔离 | 单通道激励时其它通道 ≤ −60 dB（1:1 或电学环回；声学测量不适用，改看串音报告） |
| holdout | 见 §5.2；未做 → T6 未执行、总体验收 INCOMPLETE |

T3 的测量必须是 **measured**（应用后实测），不得用 predicted 代替；测量条件与 T2 相同（同麦同位置同电平）。

### 5.1 v1.1 shelf 的应用（2026-11-16 裁决 5）

校准里若有 `low_shelf` / `high_shelf`，应用验收再加三条。`q` 是 RBJ slope S，必须在 [0.1, 1.0]：

1. `tools/apply_calibration.py --channelmap` 写出的 profile，经 `tspi-channelmap render` 后，对应滤波器是 `Lowshelf` 或 `Highshelf`，且 `slope = 12 * q`（S=1 时 slope=12.0）。shelf 的 q=1.01 或 2.0 必须在校验阶段失败，不能留下可执行配置。
2. 这份渲染配置 `camilladsp --check` 退出码 0。板端用真实 ALSA 设备；没有 ALSA 的环境可以把 devices 换成 Stdin/Stdout（filters 与 pipeline 不动）再查。
3. peaking 在 v1.1 契约里可以 q>10，channelmap 路线仍然只接受 [0.1, 10]。超过时拒绝整份应用，不能静默丢掉这一段。

示例：`examples/calibration-v11-highshelf.example.yml`（两通道 high_shelf，q=0.7，片段 slope=8.4）。

### 5.2 T6 — holdout 验证（K4）

1. 从 T2/T4 的测量点里划出 holdout 点（§0.6），这些点**不参与**任何拟合或目标曲线设计；应用校准后按 T3 流程重测。
2. 判据：holdout 点逐点“应用后实测 vs 预测/目标”偏差与 §5 同表；汇总报告均值、最大值、点数与坐标。
3. 若 holdout 点与 design 点在同一位置、同一麦高（伪 holdout）→ 无效，判 INCOMPLETE。
4. 报告字段：`holdout: {n_points, positions, design_points, residual_mean_db, residual_max_db, passed}`。

## 6. T4 — 合并响应 / 相位抵消（核心）

> T4 的输入是 direct 测量 + PEQ/延迟/极性模型（`evidence: predicted`）；应用后必须再用 T3/T6 的 measured 数据复核。
> 本节所有阈值都是 **engineering default，待 pilot**（C-P1-3/C-P1-4）；改硬门必须在报告声明并写来源。

### 6.1 交叉对判据（每个 pair、fc）

在重叠带 `B = [fc/2, 2fc]` 上计算：

1. **交叉点相位**：`|Δφ(fc)| ≤ 30°`（LR4 理想 0°；一阶/二阶设计按目标相位另设，须在报告里写 target）。
2. **带内相位偏离**：在 |H1|、|H2| 均 ≥ 各自带内最大值 −10 dB 的频点上，按 `|H1|·|H2|` 加权平均 `|Δφ| ≤ 45°`（权重定义写进报告）。
3. **抵消深度**：`CD(fc) ≥ −3 dB`；且 `min_{f∈B} CD(f) ≥ −6 dB`。
4. **无陷波**：`min_{f∈B} 20log10|S(f)| ≥ max( max|H1|, max|H2| ) − 6 dB`。
5. **合成平坦度**：1/3 oct 平滑后，`ripple(S) ≤ 3 dB`（匹配单元；不匹配单元按目标曲线另设，需在报告声明）。
6. **额外监测项（只报 warning，不硬 FAIL）**：`band_dev`（原 `target_dev`，语义为“相对自身带内均值”，与 ripple 重叠）、
   `level_diff(fc)`（高程/环绕等本就不同电平的角色会误杀）。硬门只允许 §6.1 的 1–5。

> 判定顺序：先看 1/2/3（相位/抵消），再看 4（陷波），最后 5（纹波）。任一 FAIL 即该 pair FAIL。
> **工具内嵌的“全通道求和”`global_validation` 只作为 warning**，不得进入 `overall_status/overall_pass`（C-P1-5；
> 它假设所有通道同内容播放、忽略 render 层延迟/增益，无物理意义）。任何实现不得用它否决合法 pair。

### 6.2 多低音组（仅 5.3.2 的 SUB1/2/3；当前 4.1.2 只有 SUB1，不适用）

在 30–80 Hz：

1. 任意两 SUB 在 40–80 Hz 的平均 `|Δφ| ≤ 30°`；
2. 三只 SUB 相干和 `S_sub = Σ H_sub`：`CD(f) ≥ −3 dB` 在 60 Hz，`≥ −6 dB` 在 30–80 Hz；
3. 合成后 1/3 oct 纹波 ≤ 4 dB（30–80 Hz）；
4. 与主箱在 80 Hz 交叉：按 6.1 用“**SUB 组等效 H_sub**”对 FL/FR 主箱做 pair 判据（不逐只 SUB 与主箱成对）。

### 6.3 pair 范围（C-15；D3 已裁决 2026-11-17）

| 布局 | 可执行 pair | 说明 |
| --- | --- | --- |
| **4.1.2（当前）** | 分频/低切**可配置**，不写死 80/100；当前目标：主箱 FL/FR **HP55**、环绕 SL/SR **HP60**（同型号、下潜约 40 Hz），SUB1 **LP 与主箱互补**（最终值按实测下潜/失真调） | 低切值属于配置项；MIMO 低频参与者为 FL/FR/SL/SR/SUB1，故 55–200 Hz 有 5 个声源可用，低于主箱/环绕低切的部分才仅剩 SUB1；C 不存在；HFL/HFR 全频、不参与 G |
| **5.3.2（NEEDS_HW）** | 主箱 `FL/FR:(SUB 组等效):80`；**中置 C 按设计与主箱相同为全频**（当前无中置）——C 的 HP80/低频重定向/C:SUB pair/C 相关测试**全部 deferred**，不在 P1 验收内 | 十只/三 SUB 接好后才验收；C 相关项等中置到货/设计变更后再启动，**不得**写 C:SUB pair 或把 C 当已验收 |
| 高度/环绕/中置（effect channel） | 不参与 pair | 单通道验收：极性 convention、T3 残余延迟、文件后端映射证据（§3.1 替代证据） |

- **禁止**把 `C:SUB1`、`SL:SUB1`、`SR:SUB1`、`FL:HFL:2000`、`FR:HFR:2000` 当 pair（内容不同/无分频/高度已按 K2 全频）。
  §6.2 的“SUB 组”也不包括 C/SL/SR（它们低频不汇总）。
- 编排器/merge 必须断言：**每个 pair 的 fc 等于 profile 中某 HP/LP 频率**；否则报错而不是“算一个和看看”。
- 整组/全频段求和检查（原 §6.3）取消硬判据，只保留“窄带缺口/陷波”单通道报告项（>6 dB 缺口列频点），不进入 overall。

### 6.4 负向用例（必须检测出来）

对一份“好”的校准，故意注入以下故障，验收检查器必须判 FAIL 并指出对应 pair/指标；5/5 检出、0 误报（好配置连测 3 次全 PASS）。
`negative_controls` 未跑 → T4=INCOMPLETE（§8/§9）。

| # | 注入 | 预期失败项 |
| --- | --- | --- |
| N1 | 交叉对其中一路 polarity 反相 | Δφ(fc)≈180°、CD(fc) 明显 < −3 dB |
| N2 | **2 kHz 交叉对一路加 0.25 ms 延迟（= 1/(2·fc)，180° @2 kHz；48k 下 12 samples）** | Δφ(fc)≈180°、CD(fc) 远小于 −3 dB（等幅时 −∞）；minCD 在 1 kHz / 3 kHz 附近出现零点 |
| N3 | 交叉点附近 +6 dB PEQ（单边） | 纹波/目标偏差失败 |
| N4 | FL/FR 通道映射对调 | 由 T1 `--verify-mapping` 判映射失败；T3 极性/通道归属失败（不是 pair checker 项） |
| N5 | SUB 相对主箱延迟 2 ms | 80 Hz pair 相位/抵消失败 |

> N2 必须用 0.25 ms（或参数化为 1/(2·fc)）；0.5 ms @2 kHz = 360°，两个“预期失败项”在 fc 上反而通过，是无效负向用例。
> 校准报告里 N2 的注入值同步写 0.25 ms / 12 samples。

## 7. T5 — 实时链路（64/128 帧）

- 测量工具播放/采集使用 ALSA period ≤ 128 帧（aplay/arecord `--period-size 64/128`）；设备拒绝时明确报错，不得静默降级到 1024。
- 测量流程允许停 CamillaDSP（MOTU 独占）；恢复后按下列验收：
  - chunksize = 64 与 128 各跑 **至少 60 min**（10 min 只作 screening，不得写 PASS；C-P1-9 由 B3 定稿），xrun = 0（journal 计数）；
  - CPU 统计口径：camilladsp 进程 `utime+stime` / 墙钟时间，≥10 min、同一绑核与频率策略、记录空载本底；应用校准前后变化 ≤10%；
  - 应用校准后不改变 RT 线程优先级/绑核（`ps -eLo` 对比）；
  - 64/128 帧下播放扫频/节目素材无爆音、无断流；24 h 长跑由交付阶段另测。
- 工具进程自身不得设置实时优先级抢占（普通 nice），测量期间 systemd 服务状态与日志保留。

## 8. 报告格式（机器可读 + 人读）

producer/verifier 输出 `tspi-calibration-report/v1` JSON（**缺字段=不可判定，不得补写 PASS**）：

```json
{
  "schema": "tspi-calibration-report/v1",
  "tool": {"name": "tspi-calibrate", "version": "0.1.0"},
  "created_unix": 1794725000,
  "calibration_file": {"path": "...", "sha256": "..."},
  "layout": "4.1.2", "sample_rate": 48000,
  "environment": {
    "playback": "hw:motu,0", "capture": "hw:UMIK1,0", "chunksize": 128,
    "drive_level_dbfs": -12, "k_main_trim_db": 0, "noise_floor_dbfs": -94.2,
    "mic": {"model": "UMIK-1", "serial": "718-7696", "cal_file": "90deg.txt",
            "sha256": "...", "orientation": "90deg-cal-axis-ceiling"},
    "position": {"x_m": 0.0, "y_m": 0.0, "z_m": 1.2, "note": "sweet spot"}
  },
  "tests": [
    {"id": "T1.mapping", "tier": "T1", "pass": true, "evidence": "measured",
     "metrics": {"isolation_db": -72.3, "noise_floor_dbfs": -94.2, "snr_db": 55.1}, "thresholds": {"isolation_db": -60}},
    {"id": "T6.holdout", "tier": "T6", "pass": true, "evidence": "measured",
     "metrics": {"residual_mean_db": 0.8, "residual_max_db": 2.1, "n_points": 2}, "thresholds": {"residual_max_db": 3.0}},
    {"id": "T4.pairs", "tier": "T4", "pass": true, "evidence": "predicted",
     "metrics": {"FL:SUB1:80": {"delta_phase_fc_deg": -6.2}}, "thresholds": {"delta_phase_fc_deg": 30}}
  ],
  "holdout": {"n_points": 2, "positions": ["±0.3m-left", "±0.3m-right"],
              "design_points": 1, "residual_mean_db": 0.8, "residual_max_db": 2.1, "passed": true},
  "negative_controls": [{"id": "N1", "pass": true, "detected": true, "failed_pair": "SUB1-FL"}],
  "overall_status": "PASS",
  "overall_pass": true
}
```

- `tests[]` 必须覆盖 T0–T6；每条带 `tier`、`pass`、`evidence`（`predicted`/`measured`）、`metrics`、`thresholds`。
- `negative_controls` 必须 5/5 且全部 `detected=true` 才可能 overall PASS；缺省 `not_run` → `overall_status=INCOMPLETE`。
- `overall_pass` 只有在：T0 pass、T3/T6 measured 且 pass、所有声明 pairs pass、negative_controls 5/5 时才为 true。
- 无 `holdout` 字段 / T3 未执行 / negative_controls 未跑 → `overall_status=INCOMPLETE`。
- `global_validation`（若工具给出）只能作为 warning 字段，不得参与 `overall_pass`。

人读报告（Markdown）必须包含：每通道 delay_samples/polarity/PEQ 表、应用前后 FR 图/CSV 路径、
T0–T6 每项 PASS/FAIL/NOT_APPLICABLE 与实测值、holdout 逐点残差、边界/失败点、复现命令、原始数据 sha256、话筒校准文件与方向。

## 9. 判定汇总

| 档 | 条件 | 禁止的表述 |
| --- | --- | --- |
| **PASS** | T0–T6 全 PASS + negative_controls 5/5 检出 + 重复性 3/3 + holdout 已报且达标 + 话筒校准已记录 | — |
| **PASS_WITH_T1_PARTIAL** | §3.1 条件（T1 partial 有替代证据）+ 其余同 PASS | 不得写“T1 PASS / 1:1 映射已验证” |
| **INCOMPLETE** | T3 或 T6 未执行、holdout 未报、negative_controls 未跑、话筒校准/trim/噪声底未记录 | 不得写 PASS / “免测通过” |
| **FAIL** | 任一 T 层 FAIL、负向用例漏检、校准文件不可复现 | — |

- **禁止 `PASS_OPEN_LOOP`/“不要求应用后复测”口径**（K4 作废旧裁决，2026-11-16 23:37）。
- “有条件通过”只用于非关键项（如 SNR 略低但已重测并记录），必须在报告标注并由 Lead/用户确认，不得替代 INCOMPLETE。
- 旧 STATUS/E2E-ACC 中 T1 曾过/T2 SNR 不足/T4 未完成/T5 未过的转述不构成本文件意义的 PASS；按新条件重跑后重新签。

## 10. 裁决与执行方式

### 10.1 已裁决（2026-11-17，用户/Lead 确认；写入本文件与 CONTRACT §11）

| # | 裁决 | 本文件落地 |
| --- | --- | --- |
| **D8** | 4.1.2 SUB1 唯一物理口 = **Line Out 8（槽位 7）**；Line Out 10/槽 9 仅历史临时测试，**不是候选** | §0.8 运行态 mapping；§3.1 uncovered_outputs 4.1.2 例（SUB1 1-based=8） |
| **D3** | 5.3.2 中置按设计与主箱相同为**全频**；当前无中置 → C 的 HP80/低频重定向/C:SUB pair/C 相关测试**全部 deferred（NEEDS_HW，不在 P1 验收内）**；4.1.2 pair 仅 `FL:SUB1:80`、`FR:SUB1:80` | §6.3 pair 范围；§6.2 多 SUB 仅 5.3.2 |
| **D7** | **允许 +3 dB boost**（房间校准 PEQ/目标拟合）；同时执行削波/headroom 与总增益帽规则 | §5 T3 目标偏差 + boost/headroom 行；§0.5 削波门；CONTRACT §3.3/§5 规则 9 |
| **D4** | 目标曲线仅 **flat + tilt10**（20 Hz 0→20 kHz −10 dB，log-f，参考增益独立）；旧 −3 dB@16k 曲线 **superseded**；其它曲线仅导入扩展、不设默认 | calibrate/README“目标曲线”；CONTRACT §10/§11（target-curves.yml 由 audio-stack 更新，不在本任务写域） |

### 10.2 仍待裁决/待 pilot（不阻塞本文件执行）

| # | 项 | 当前执行口径 | 裁决后动作 |
| --- | --- | --- | --- |
| C-P1-3 | T2/T4 数值阈值（0.3 sample、0.4 dB、30/45° 等）无 pilot | 标 engineering default，先用现有 3 次 T2 + 加测 5–10 次统计 | pilot 后收紧/放宽并更新 |
| C-P1-7 | Rust 主入口校验规则少于契约（name 唯一/polarity 必填与 ±1/reference delay=0 warning/layout 交叉/未知字段/through_render） | T0 要求 **Rust 主入口 + Python 独立复核** 都跑；缺一不得签 | audio-stack 补齐后移除“必须双跑”备注 |
| C-P1-10 | 工具无 `--inject-*` | §3 用冻结 CamillaDSP 配置 + render_offsets + sha256 | 工具支持后简化 |
| C-P1-11 | 工具无 capture_peak/带限 SNR 硬门 | §0.5 要求报告字段；缺失=不可判定 | 工具实现后作为自动 FAIL 门 |
| C-P1-9 | T5 窗口（10 min screening vs 60 min×2） | §7 写“≥60 min，10 min 仅 screening”；最终以 B3 定稿为准 | D6 裁决后统一 |
| C-18 | 话筒校准落点（工具内建 --mic-cal vs 仅 recompute 后处理） | §0.1 要求记录 cal 文件/方向；未接工具前声明相对有效 | 工具内建后报告字段改必填 |