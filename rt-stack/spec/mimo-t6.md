# MIMO-T6 校准实际验收规格 — 低频 MIMO 应用后实测 / 独立 holdout / 两轮复现

> 2026-11-16 · rt-acceptance（WBS **T73** / team task-29） · 文档 schema **khaen-mimo-t6/v1** · 报告 schema **khaen-mimo-t6-report/v1**
> **命名（2026-11-17 修 I01）**：本文件定义 **MIMO-T6（记 T6-M）**，是 B2 `calibration-spec/ACCEPTANCE.md` §5.2 的普通校准 holdout 验证（记 **T6-cal**）之上的 **MIMO 专项实测门**。两者**都须独立 PASS**（T31 签收需双证据）；T6-M 不替代、不合并、不 supersede T6-cal；报告必须用 `t6_kind` 与 `t6_cal` 字段区分。
> 状态：**SPEC-ONLY（v1.2，2026-11-17 按 Cursor v0.9 终审 F-19 修：I_use ≥3.0 dB 与 T20 下降 ≥15% 改为预声明工程观察项，不参与 PASS/FAIL）**。本文件只冻结验收规格：未实现校验器、未测量、未上板、未应用校准、未跑 holdout、未 git commit。门阈值是**工程起始值**（WBS §5 / DEC §2.8 授权的默认值），不是已实测结果或物理保证；其中 I/T20 改善只是**工程观察目标**（§8.2.1），不决定验收 PASS/FAIL。评审/终审修复记录见 `evidence/T73/REVIEW-FIXES.md`。
> 写域：projects/khaen-audio-dsp/rt-stack/spec/mimo-t6.md + rt-stack/evidence/T73/。不改 calibration-spec/**（旧 CONTRACT/ACCEPTANCE 只读参考），不改 rt-stack 其它文件。
> 正确性声明：本规范**独立于旧 calibration-spec 契约**：不复用其字段名、报告 schema、T0–T5 判定结构或阈值表；旧契约只作为 T0–T4/T1partial 的**证据口径参考**（见 §1.2）。任何冲突按 §0.3 权威链裁决。

---

## 0. 定位、独立性与权威链

### 0.1 本规范是什么

MIMO-T6（T6-M）是新自研 RT-first 栈（DSP-HOST-DECISION §1–§2）的**MIMO 校准实际验收层**：在目标 4.1.2 多声道 profile 中，由新 host/daemon 完成真实扫频、UMIK 声学采集、PEQ/FIR/delay/polarity 与真实 5×5 低带 MIMO 系数应用后，验证：

1. 应用后**同场基线 vs MIMO-on 复测**的实测改善（I）达到**预声明工程观察目标（仅观察，不参与 PASS/FAIL，§8.2.1）**；
2. 在**从未用于调参的独立 holdout 测点**上重复报告该观察值（holdout 隔离与证据完整性参与 PASS，观察值本身不参与）；
3. 低带**模态衰减（有效 T20）**相对基线下降达到**预声明工程观察目标（仅观察，不参与 PASS/FAIL，§8.2.1）**；
4. 以上结论在**至少两轮独立完整 job** 中可复现；
5. 测量条件被冻结、证据可哈希回溯、负对照可被自动检出；
6. 结论由**独立 checker 从 raw 复算**，不由设计器/daemon 自报。

T6-M 是 P1 门 G-cal/G-real 中"MIMO 实测"部分的可判规格（WBS §5、§8；改善值为工程观察，判定看证据完整性/隔离/保护门）；它不替代 T31 总门，也不替代设计侧 T53/T54 的 predicted 产物门。**层级关系**：B2 T6-cal（普通校准 holdout）→ 本文件 T6-M（MIMO 实测门）；T31 的 G-real 证据必须同时包含两者（T6-cal pass + T6-M pass）。

### 0.2 非目标（本规范不定义）

- 不定义系数设计算法（PEQ fit / solver / 正则 / target 设计）：T50–T52、T64–T65；
- 不定义 5×5 数学与运行时卷积语义：T03 `reference/SPEC.md`、T07–T12；
- 不定义 REST/WS/channelmap 兼容面：T05 `spec/compat-matrix.md`、T42/T70；
- 不重新定义 B2 的 T0–T5 与 **T6-cal** 的字段/阈值/流程，也不修改 `calibration-spec/CONTRACT.md`、`ACCEPTANCE.md`；T6-cal 仍须独立 PASS，本文件只在其上增加 MIMO 门；
- 不设正式 1/2/4 核 speedup、128/64 生产 worst-case 与 1h soak 门（属 P2 T46/T30；P1 只要求测量窗口内 xrun/deadline 记录真实）；
- 不实现上混/下混；T6 只在显式 5 参与者低带矩阵内测量，stereo_strict 与多声道 profile 各自独立验收（WBS §3、DEC §1）。

### 0.3 权威链与冲突裁决

1. 用户最新明确要求（含 2026-11-17 对 Cursor v0.9 终审 F-19 的裁决：**I_use ≥3.0 dB 与 T20 下降 ≥15% 只是工程观察，不得决定验收 PASS/FAIL**；落地见 §8.2.1）；
2. `scratch/lead/planner/DSP-HOST-DECISION.md`（下称 DEC）；
3. `scratch/lead/planner/KHAEN-RT-STACK-WBS.md`（下称 WBS）；
4. **本文件**（T6 专用验收字段/阈值/失败规则）；
5. 旧 `calibration-spec/CONTRACT.md`、`ACCEPTANCE.md`（只读参考；其 T0–T5 证据口径按 §1.2 引用）。

本文件与旧契约冲突时：T6 判定以本文件为准；旧文件由 B2 批次另修，**不在本任务写域**。阈值只能按 §11.5 升版改，不得测后放宽。

### 0.4 与旧 calibration-spec 的关系

| 项 | 处理 |
|---|---|
| `calibration-spec/CONTRACT.md`、`ACCEPTANCE.md` | **T73 未写入**（写域只有本文件 + `evidence/T73/`）。只读引用其 T0–T4/T1partial 证据口径（§1.2）。T73 读取时 hash 在 `evidence/T73/READONLY-BEFORE.sha256`；**执行期间并发 B2 任务（task-33 spec-checker）修订了这两个文件**，T73 不依赖其当前内容，before/after 与归属记录见 `evidence/T73/CONCURRENT-CHANGES.md`。 |
| 旧 `khaen-calibration-report/v1` schema | **不复用**。T6 使用独立 `khaen-mimo-t6-report/v1`（§9），字段与语义由本文件定义。 |
| 旧 T5（64/128 帧 0 xrun） | **T6-M 测量窗口内必须 0 xrun/deadline miss/clip**，否则该窗口无效；正式 64/128 长窗口/1h soak 属 P2，不作 T6-M 门。 |
| 旧 T1 partial | 引用其报告字段与"partial 不等于 PASS"精神（`ACCEPTANCE.md` §3.1）；T6 另有声学输出身份门，partial 不豁免（§1.3）。 |
| 旧 T3（应用后逐通道复测） | 每轮 T6-M 必须附 T3-equivalent 复测证据；旧阈值只作参考，T6-M 声学 I/T20 为**预声明工程观察项（不参与 PASS/FAIL，§8.2.1）**。 |
| 旧 T6（B2 `ACCEPTANCE.md` §5.2，普通校准 holdout，记 T6-cal） | 设计/验证点分离后应用后复测；≥1 holdout 点，残差与 §5 同表 | **T6-M 的独立前置**：同一轮 job 必须附 T6-cal 报告（`n_points/positions/design_points/residual_mean_db/residual_max_db/passed`）；T6-cal 未 PASS → T6-M 不能 PASS | T6-M 不替代 T6-cal；T6-cal 的 ≥1 点不满足 MIMO 的 ≥3 点/两轮/NC-10 要求 |

### 0.5 本轮状态

- 本任务**只写规格与证据清单**：未实现 T6 测量/校验工具，未接板，未采集，未应用系数，未生成任何报告；
- 文中所有表格数字、公式、字段均为**规范要求**，不是测量结果；
- 报告 schema 示例（§9.2）标记 `example: true`，不得当作真实报告；
- 未测项清单见 §11.4。

---

## 1. T6 定义、前置与证据分层

### 1.1 T6 定义（normative）

T6 = **低频 MIMO 校准实际验收**：对一个已由新栈完成设计并应用的系数集（每 path FIR / G 矩阵嵌入 / P_post / 保护），在真实房间、真实 4.1.2 输出路由、UMIK 独立 USB ADC 1ch 声学采集条件下，按 §2–§6 冻结条件完成：

1. 应用前设计点原始测量（design raw，仅 design 测点）→ 设计/求解 → 系数 artifact 冻结；
2. 应用后同场**基线（G=I，同 D0）与 on（G 应用）**两态复测（含 holdout 测点）；
3. 独立 checker 从 raw 复算 I、T20（工程观察值）、guard、保护与预测一致性；
4. 至少两轮独立完整 job；
5. 负对照与近零规则执行。

T6 的判定对象是**证据完整性、holdout 隔离与保护/guard 门**，不是设计报告预测；实测改善 I/T20 是**预声明工程观察**（§8.2.1），不决定 PASS/FAIL；预测值只用于 honest-check（§4.9）。

### 1.2 旧 T0–T4/T1partial 引用口径（只读；T6 消费方式）

下表把 `calibration-spec/ACCEPTANCE.md` 的旧分层映射为 T6 的**前置证据**。旧阈值**只作参考/下限**，T6 不继承其字段与判定结构；T6 自身测量有效性阈值见 §3.5。

| 旧层（参考） | 旧口径摘要（只读引用，非 T6 字段） | T6 消费方式 | T6 不继承/明确区分 |
|---|---|---|---|
| T0 YAML 契约校验 | 旧 CONTRACT §5 校验规则 | 新栈 artifact 的 schema/version/hash/role/stale/prepare-apply ack 证据（T13/T60/T66）必须 pass，作为 T6 前置 | 不要求旧 CONTRACT 字段；不把旧校验器结果当 T6 证据 |
| T1 MOTU 环回/映射/隔离/延迟/极性 | `ACCEPTANCE.md` §3 + §3.1 T1 partial 字段 | 消费为测量链/路由证据：环回映射、隔离、延迟/极性抽检；partial 必须带 loopback_map/covered/uncovered/cuemix_routing/final_insert 声明 | 不继承旧绝对隔离阈值与驱动电平（审计 C-16）；T6 另加声学输出身份门（§1.3） |
| T2 UMIK 重复性/漂移/SNR | `ACCEPTANCE.md` §0.2/§0.5/§4（B2 现行） | **对齐 B2 现行口径**：3 次重复、延迟极差 ≤0.3 sample、1/12 oct 幅频 ≤0.4 dB、极性一致；按 B2 §0.2 报告 `clock_ppm`+`clock_ppm_uncertainty_ppm`，只有 \|ppm\|>2σ 才算漂移（100 ppm 仅 sanity 上界），补偿后 10 kHz 残余相位 ≤15°（跨度 10.4 s 且 σ_ppm ≤0.4 ppm，或 ≥5 遍/≥60 s）；SNR 200 Hz–8 kHz ≥25 dB（带限用自身通带）；峰值 ≤−1 dBFS。T6-M 另加低带 63–250 Hz ≥25 dB 双门并要求 ≥3 次 off 重复 | T6-M 不重新定义 T2/T6-cal；T2 通过不代替 T6-M 改善观察（§8.2.1）；不得另立互斥 ppm/SNR 门 |
| T3 应用后逐通道复测 | `ACCEPTANCE.md` §5 | 每轮必须附 T3-equivalent：实际生效版本/ack、逐通道延迟/极性/PEQ/目标偏差复测 | 旧 T3 阈值仅用于链路正确性参考；T6 声学 I/T20 为**预声明工程观察项（不参与 PASS/FAIL，§8.2.1）** |
| T4 合并响应/相位抵消 | `ACCEPTANCE.md` §6（pair 判据 + N1–N5 负向） | T6 要求 T4 证据状态为 pass（或明确声明）；4.1.2 现行 pair 只接受 FL:SUB1@80、FR:SUB1@80（审计 C-15）；单通道/声学 FR 覆盖每个输出 | T6 不重复 pair 判据细节；T4 不替代 T6 低带矩阵改善观察（§8.2.1） |
| T5 实时链路（64/128 帧） | `ACCEPTANCE.md` §7 | **T6-M 测量窗口内必须 0 xrun/deadline miss/clip 并如实记录**；异常窗口无效（不得"仅记录后仍 PASS"） | 正式 64/128 长窗口/1h soak 属 P2；T6-M 只要求测量窗口 0，不把整链 KPI 当 MIMO 效果门 |

引用锚点：T1 partial 规则 `calibration-spec/ACCEPTANCE.md` §3.1；T2 `§0.2/§0.5/§4`；T3 `§5`；T4 `§6`；T5 `§7`；**T6-cal `§5.2`（B2 现行，仍须独立 PASS）**（均为只读参考，本文件不改其内容）。上述章节号为 **T73 读取快照 + B2 现行版本的并集**；T6-M 所需字段/判据以本文件 §1.3 的显式清单为准；旧文件 hash/变化见 `evidence/T73/CONCURRENT-CHANGES.md`。

### 1.3 T1 partial 的消费规则（normative）

1. T1 若为 partial，报告必须原样记录 `t1.status=partial`、`loopback_map_1based`、`covered_outputs/inputs_1based`、`uncovered_outputs_1based`、`cuemix_routing`、`final_insert_direct_routes`、`mitigation`、`claim`（字段语义只读引用旧 §3.1）；
2. **T1 partial 绝不豁免 T6-M**：T6-M 要求每个参与输出（4.1.2：FL/FR/SL/SR/SUB1 对应 DAC 0/1/2/3/7）在 T4 声学逐通道 FR **或 §3.8 声学输出身份检查（T6-AID）** 中有明确物理输出归属证据；缺任一参与输出的声学归属 → T6-M **INCOMPLETE**（I-08），不是 PASS；
3. T1 partial 的"未覆盖输出"不得以"其它通道正常"外推；声学归属证据必须带文件/测点/hash；
4. T6 报告必须在 `prerequisites.t1_partial` 与 `verdict.limits` 中写明范围限制，禁止写"T1 PASS"或"全通道已验证"。

### 1.4 判定主体与独立性（normative）

- **producer**（daemon/设计器）负责测量、设计、应用与产出 artifact/predicted；**checker**（T68 独立 checker 或等价独立实现）只从 raw + 冻结 artifact 复算，不得调用 producer 的聚合函数或报告值作为自身结论；
- checker 的输入必须是 §9.4 的 raw 证据包与 hash 链；checker 输出必须可重复（同输入→同输出，无随机种子、无时间依赖）；
- producer 报告的 I/T20 与 checker 复算值若不一致 → 以 checker 为准；不一致本身记 FAIL-08（报告与原始数据不一致）；
- 判定人（reviewer）只依据 checker 报告 + 人工检查表（§10）签字；不得只签 producer 自报。

### 1.5 T6 不替代的门

- 设计侧 T53/T54 的 predicted/artifact 门仍须独立 PASS；T6 不因为 predicted 好看而减免；
- T3/T4/T5 前置证据缺失时 T6-M 不能判 PASS（§8.4 I-01）；
- **同轮 B2 T6-cal 未 PASS**（或 holdout 报告缺失/伪 holdout）时 T6-M 不能判 PASS（I-01）；T6-cal 不因 T6-M 通过而免测；
- P2 性能门（T46/T30）不因 T6-M 通过而自动满足。

---

## 2. 冻结测量条件（FC-01..FC-16）

以下条件必须在第一轮任何测量开始前写入 `khaen-mimo-t6-frozen/v1` 冻结清单（§2.1），hash 后冻结（`frozen_id`）。两轮共用同一冻结清单；任何一项变化 → 该轮测量无效（重测）或 T6 INCOMPLETE。

| ID | 冻结项 | 冻结/可判要求 | 违反判定 |
|---|---|---|---|
| FC-01 | 房间与布局 | room id/名称、房间尺寸/处理、听音位、扬声器摆位记录（照片/草图 hash）；两轮之间不得移动扬声器/家具/话筒基准标记 | 无记录 → I-02；移动/未记录变化 → 该轮无效 |
| FC-02 | profile/通道映射 | 4.1.2 participants=FL/FR/SL/SR/SUB1，dense_to_dac=[0,1,2,3,7]，容器 scatter=[0,1,2,3,4,5,7]（7 角色容器映射），ch6 空；**G_eff 的 5 行 dense=0..4 只映射到 DAC 0/1/2/3/7；HFL/HFR（dense 无）只走 P_post identity/保护，不参与低带矩阵**；overlay layout-presets 解析 hash + 实际设备宽度校验；不得硬编码快照 | hash 不符/设备宽度不符 → I-02；静默改映射 → FAIL-08 |
| FC-03 | 图差异唯一化 | baseline 与 on 图**只允许** g（与声明的 P_post 不变）不同：same fs/block/R/A_map/P_post/D0/protection/limiter/volume/mute/gain；两图 graph_id/plan hash 与差异清单入清单 | 多出未声明差异 → I-02（若蓄意 → FAIL-08） |
| FC-04 | 公共 D 与相位基准 | 预声明整数公共 D0（samples）与相位基准（电学 reference 通道 preferred；否则声明 direct-sound onset 基准）；baseline/on/两轮同一 D0；只扣 D0，其它延迟不事后平移 | 未声明/前后不一致/事后平移 → FAIL-07 |
| FC-05 | render frame/slot 与时间轴 | 每个测量文件记录 playback `render_frame_index`（起始帧）、`slot`/通道、capture `capture_sample_index`、`session_id`、`epoch`/`graph_id`；时间轴映射（ppm/重采样）方法声明；**字段契约见 §2.1 `timeline` 与 §9.1 `rounds[].measurements[]`**；缺失 → 测量无效 | 缺失映射 → I-02；错误映射导致错位 → I-10 |
| FC-06 | 时钟与漂移 | **对齐 B2 §0.2**：必须同时记录 `clock_ppm` 与 `clock_ppm_uncertainty_ppm`；只有 \|ppm\| > 2×uncertainty 才可称漂移并启用校正（方法预声明并一致应用）；`\|ppm\| ≤100` **仅 sanity 上界**；补偿后 10 kHz 残余相位 ≤15° 且跨度写死（默认 10.4 s、σ_ppm ≤0.4 ppm，或 ≥5 遍/≥60 s）；N=1 不得估计漂移/启用重采样；字段见 §2.1 `clock` | 未记录 uncertainty/漂移未按 2σ 处理 → I-02；超限未校正 → 该文件无效 |
| FC-07 | 电平与增益 | 数字激励电平、播放增益、ADC 增益、话筒增益、master/main trim（如 kMainTrim）在两轮/两态间固定；kMainTrim 若非 0 必须记录并补偿；中频（400–2000 Hz）电平不变性 ≤0.2 dB | 电平变化 >0.2 dB 或未记录 trim → I-02；事后归一 → FAIL-07 |
| FC-08 | 话筒/校准/位置 | UMIK-1 序列号、miniDSP cal 文件（sha256）、朝向（多声道听音位按 90° 校准/朝天花板声明）、每测点位置标记；任何移动后重新记位置；未应用 cal 不得声称 ±3 dB 类绝对目标 | 缺失/朝向未声明 → I-02；位置未记录 → I-02 |
| FC-09 | 设备独占与 CueMix | 测量/校准期间唯一音频 owner（无并发播放/录音进程）；T1 前 CueMix/矩阵路由快照（文本 sha256 + 截图 sha256，字段见 §2.1 `cuemix`）；final insert 模式必须关闭全部 input→output 直通并留证；out7/out8 等被环回输出不接后级 | 未独占/双路径并存 → I-02；静默改用户直通 → FAIL-08 |
| FC-10 | 温度与物理状态 | 每轮/每态记录温度（字段见 §2.1 `temperature{baseline_c,on_c,rounds_c}`）；baseline 与 on 之间、两轮之间 \|ΔT\| ≤2 K；超过且未声明补偿 → 该轮无效（重测）；扬声器旋钮/设置动过 → 该轮无效 | 超限/未记录 → I-02 或无效轮 |
| FC-11 | 激励信号 | 低频专用扫频/IR 激励文件（path+sha256+fs+时长+fade+前后静音+重复次数）在 design/off/on/round 各处相同（或按声明 set 一一对应）；电平/ramp/clip-stop 参数声明 | 文件/电平不一致 → I-02；替换激励 → FAIL-08 |
| FC-12 | 保护/限幅状态 | limiter threshold/release、max_gain、保护 HP/LP、mute/volume 状态在两态间一致；参考节目（PR-A 不相关馈送 / PR-B 全同相最坏）文件 hash 冻结 | 状态变化未声明 → I-02；绕过保护 → FAIL-02 |
| FC-13 | 软件/工具/artifact 版本 | host/daemon/checker/分析脚本版本与 sha256；graph_id、coeff manifest hash、calibration artifact hash、target 声明 hash、tuning inputs hash 全部记录 | 缺失 → I-02；轮内替换工具 → I-11 |
| FC-14 | 冻结时点 | 冻结清单 `frozen_at_utc` 必须早于第一轮 holdout 首次采集；seal commit（含测点/checker/分析脚本/artifact 及跨轮预注册，字段见 §2.1 `seal`）在采集前提交 | 时点不成立 → I-04 |
| FC-15 | 测量窗口有效性 | 每个测量文件（字段见 §2.1 `measurement_validity`）：无 xrun/deadline miss/失锁/丢块；ADC 峰值 ≤ -1 dBFS；**SNR 双门**：B2 全带 200 Hz–8 kHz ≥25 dB（带限用自身通带）+ T6-M 低带 63–250 Hz ≥25 dB；**off/design 重复 ≥3 次**（与 B2 T2 对齐），on 重复 ≥2 次；否则该文件无效并重测 | 有 xrun/削波/SNR 不足/重复不足未重测 → I-06/I-07 |
| FC-16 | 禁止事后改动 | 冻结后禁止改 target/D0/positions/quality mask/analysis script/checker/thresholds；任何改动必须新建冻结版本并重测全部轮次（旧数据不得复用） | 事后改参数 → FAIL-07 |

### 2.1 冻结条件清单 schema（khaen-mimo-t6-frozen/v1）

必填字段（机器可判，缺失 → I-02）：

| 字段 | 类型 | 说明 |
|---|---|---|
| schema | string | 必须 = `khaen-mimo-t6-frozen/v1` |
| frozen_id | string | 唯一冻结 id（进入所有测量文件与报告） |
| created_utc / frozen_at_utc | string(ISO8601) | 生成/冻结时刻；冻结时刻必须早于 holdout 首采 |
| scope | object | layout/participants/dense_to_dac/fs_hz/block/chunksize |
| room | object | id/name、尺寸/处理、听音位、**摆位照片/草图 sha256（photo_sha256/plan_sha256）**、温度目标 |
| graph | object | baseline_graph_id、on_graph_id、diff_summary、**baseline_version、on_version**、common_d_samples、phase_ref、participants、dense_to_dac、scatter |
| devices | object | playback/capture 稳定 ID、实际 hw params（rate/format/channels/period/buffer） |
| mic | object | model/serial/cal_file_sha256/orientation/position |
| stimulus | array | path/sha256/bytes/fs_hz/level_dbfs/seconds/repeats/role |
| gains | object | digital_level_dbfs/playback_gain_db/adc_gain_db/mic_gain_db/main_trim_db |
| protection | object | limiter/protection 配置、参考节目文件 hash |
| positions | object | design[]、holdout[]（label/role/weight 必填；空间坐标可选） |
| **timeline** | object | **FC-05 必填**：`render_frame_index`/`slot`/`capture_sample_index`/`session_id`/`epoch`/`graph_id`/`timeline_method`（ppm 映射或重采样）+ 每文件 `sidecar_sha256` |
| **clock** | object | **FC-06 必填**：`clock_ppm`/`clock_ppm_uncertainty_ppm`/`drift_significant`（\|ppm\|>2σ）/`correction_method`/`residual_phase_10k_deg`/`residual_span_s`/`sigma_ppm` |
| **temperature** | object | **FC-10 必填**：`baseline_c`/`on_c`/`rounds_c[]{round,design_c,on_c}`/`delta_max_k`；ΔT>2 K 时 `compensation` |
| **measurement_validity** | array | **FC-15 必填**：每文件 `{file_sha256, xrun, deadline_miss, lost_lock, dropped_blocks, capture_peak_dbfs, snr_band_hz, snr_db, snr_lowband_db, repeats, repeats_effective, valid}` |
| **seal** | object | **FC-14/§5.3 必填**：`seal_id`/`seal_committed_utc`/`points_sha256`/`analysis_script_sha256`/`checker_sha256`/`artifact_sha256`/`predicted_script_sha256`/`cross_round_prereg_sha256`（§5.6） |
| **cuemix** | object | **FC-09 必填**：`snapshot_text_sha256`/`snapshot_image_sha256`/`routing{input_to_output}`/`final_insert_direct_routes`/`out7_out8_downstream` |
| software | object | host/daemon/checker/analysis script 版本+sha256 |
| policy | object | quality mask 阈值、near-zero 规则、统计聚合、T20 有效窗参数（所有"先冻结"项） |
| sha256 | string | 本清单规范化 JSON 的 sha256（自 hash 不含本字段；方法声明） |

**FC → schema 字段闭包（I03）**：FC-01→`room.photo_sha256/plan_sha256`；FC-02→`graph.participants/dense_to_dac/scatter`+`scope`；FC-03→`graph.diff_summary/baseline_version/on_version`；FC-05→`timeline`；FC-06→`clock`；FC-07→`gains`；FC-08→`mic`；FC-09→`cuemix`；FC-10→`temperature`；FC-11→`stimulus`；FC-12→`protection`；FC-13→`software`；FC-14→`seal`；FC-15→`measurement_validity`；FC-16 为程序性规则（§8/F-07）。任何 FC 行缺对应字段 → I-02。

### 2.2 每轮测量序列（可执行顺序；R0–R9）

| 步骤 | 内容 | 产出/证据 |
|---|---|---|
| R0 | 前置验证：T0–T4-equivalent 证据、T1 partial 声明、设备独占、CueMix 快照、冻结清单 hash | prerequisites 记录 |
| R1 | 开启 session（session_id、设备重新打开、温度/位置记录） | session manifest |
| R2 | 仅 design 测点采集 design raw（off 态：G=I、D0） | design raw + hash |
| R3 | 设计/求解 → 系数 artifact（coeff WAV/manifest/target/预测输入声明）冻结 | design artifact hash |
| R4 | 应用（prepare→commit ack），记录 actual graph_id/version | apply ack 证据 |
| R5 | T3-equivalent 逐通道复测（delay/polarity/PEQ/target/隔离） | 复测证据 |
| R6 | holdout 密封：holdout 测点列表/分析脚本/checker hash/artifact hash commit（采集前） | seal commit |
| R7 | holdout 与 design 测点两态采集：off/on 交叉（A/B/A 顺序，控制漂移），raw hash 记录 | raw 证据包 |
| R8 | 预测文件（从 off holdout plant + 冻结 coeff 计算）与实测分析；独立 checker 复算 | predicted + checker report |
| R9 | 第二轮 R1–R8（新 session、新 raw、新 design、新 artifact） | round B 证据 |

顺序要求：R3 冻结在 R6 之前；R6 commit 在 R7 首采之前；R7 raw hash 记录在 R8 分析之前；任何顺序违反 → I-04。

---

## 3. 测点、激励、网格与有效集

### 3.1 participants 与槽位（normative）

- 当前 T6 主对象为 **4.1.2**：输出角色 O=7、容器 C=8、participants=[FL,FR,SL,SR,SUB1]（M=5，dense 0..4），scatter=[0,1,2,3,4,5,7]；SUB1=容器/DAC 7，ch6 恒 0；
- 输入角色 I=participants（5 路：G 的列），输出角色 J=participants（5 路：G 的行）；HFL/HFR 不参与低带矩阵（显式 scope 排除，不靠角色名推断）；
- **容器 scatter 与矩阵维度关系（I12）**：G_eff 是 5×5（行/列=participants）；dense 行 0..4 → DAC [0,1,2,3,7]。完整 4.1.2 容器 `scatter=[0,1,2,3,4,5,7]` 描述 **7 个角色**的容器映射（HFL/HFR=容器 4/5 只走 P_post identity/保护，不进 G_eff；容器 ch6 不用）。禁止把 7 槽 scatter 当作 G 的 7 路输出，也禁止把 HFL/HFR 当低频参与；
- 5.3.2 若将来启用：M=8，design ≥8 + holdout ≥3，同一套门与 §8.2.1 观察项；本版不要求 5.3.2 实测（H16 未到 → NOT_VERIFIED）；
- 任何映射以实际 overlay 解析 + 设备宽度校验结果为准（FC-02），不得硬编码本快照。

### 3.2 测点：design 与 holdout（normative）

| 项 | 4.1.2 正式 T6 要求 |
|---|---|
| design 测点 | ≥6 个（label+role=design+weight，weight>0）；不得用于 holdout 评估 |
| holdout 测点 | ≥3 个（role=validation），空间上与 design 分离（不同座位/位置，不得重复测点） |
| 总数 | 推荐 9（6+3）；5+3 只允许 pilot，结果只能 INCOMPLETE（点数不足，I-03），不得称 T6 PASS |
| 标签 | 必须 label/role/weight；坐标/朝向可选；role 必须区分 design/validation |
| 选点 | 在任何 design 拟合前选定并记录；holdout 不得事后替换/增删；测点数量与 label 清单进入冻结条件 hash |

### 3.3 频率网格与判据频带（normative）

- 分析网格：20 Hz–20 kHz，≥1/24 oct 对数网格（报告实际网格数组/hash）；
- **改善观察频带**：63–250 Hz（含端点近似），每 (p,i) 的有效网格点必须覆盖该频带 ≥80%（默认至少 39 个 1/24 oct 点；网格由冻结清单给出）；
- **T20 判据频带**：1/3 oct 带中心 63 Hz 与 125 Hz（必需）；40 Hz 可选报告；
- **guard 频带**：20–63 Hz、250–300 Hz（目标误差退化门）；300–400 Hz（过渡门 + 相位报告）；400 Hz–10 kHz（不变性门）；10–16 kHz（仅报告）；
- **预测一致性频带**：25–300 Hz（1/6 oct 抽样点，用于 §4.9）。

### 3.4 激励与重复（normative）

- 低频专用对数扫频或 IR 激励；文件、电平、fade、前后静音、重复次数进入冻结清单（FC-11）；
- **design/off 每测点 ≥3 次有效重复**（与 B2 T2 对齐：3 次重复算延迟/幅频极差）；on 每测点 ≥2 次；E_noise 用相邻两次 off 计算并报告全部重复对；不足 → 该测点无效/INCOMPLETE；
- 激励不得削波：数字峰值 ≤ -1 dBFS；ADC 采集峰值 ≤ -1 dBFS；
- 重复之间保持相同序号/时间轴 metadata；重复文件独立 hash。

### 3.5 质量掩码与测量有效性（normative）

逐 (p,i) 判定有效性；以下任一失败 → 该 (p,i) 无效（可重测一次；重测后仍失败 → 排除并计入 N/A/覆盖不足）：

| 检查 | 阈值（工程起始门） | 依据/说明 |
|---|---|---|
| 重复性-延迟 | **3 次重复**延迟极差 ≤0.3 sample | B2 T2 现行口径（§4）；T6 实测 |
| 重复性-幅频 | **3 次重复**、1/12 oct 平滑后 100–10k 任一点极差 ≤0.4 dB；T6 另报 63–250 Hz | B2 T2 现行口径 |
| 极性一致性 | 3 次重复极性判断一致；与物理接线/校准声明一致 | 极性未知 → 无效（不得猜） |
| 时钟漂移 | 按 B2 §0.2：报告 `clock_ppm`+`clock_ppm_uncertainty_ppm`；只有 \|ppm\|>2σ 才算漂移/允许校正；100 ppm 仅 sanity；补偿后 10 kHz 残余相位 ≤15° 且跨度/σ_ppm 达标；N=1 不可估计 | 与 B2 一致，不得另立 \|ppm\|≤100 有效性门 |
| SNR | **双门**：B2 全带 200 Hz–8 kHz ≥25 dB（带限用自身通带）**且** T6-M 低带 63–250 Hz 每 (p,i) ≥25 dB | 与 B2 §0.5 一致；报告 `snr_band_hz/snr_db/snr_lowband_db` |
| 重复次数 | design/off ≥3、on ≥2；不足 → 无效/INCOMPLETE | FC-15/§3.4；与 B2 T2 对齐 |
| 相干性 | 若用扫频，magnitude-squared coherence ≥0.9 的点 ≥80% | 不足 → 无效 |
| 时间轴/D 完整性 | 测得公共延迟与声明 D0 差 ≤±0.5 sample；render/capture 映射存在且自洽 | 超差 → 无效/重测 |
| 电平不变性 | 400–2000 Hz 采集电平 off vs on 差 ≤0.2 dB（同时刻/同增益） | 用于证明 FC-07；超差 → 检查增益/重测 |
| 排除比例 | 每 (p,i) 被 mask 排除的网格点 ≤20% | 超过 → 该 (p,i) 无效 |

### 3.6 有效集最低要求（normative）

- holdout 候选集 = holdout 测点数 × 5 inputs（4.1.2 默认 3×5=15）；**有效 (p,i) ≥12（80%）** 且至少 2 个 holdout 测点各 ≥4 个有效输入、每个 input 至少 2 个有效测点；否则 → I-07；
- 每轮独立计算有效集；两轮 N/A/无效集合按 (p,i) 对比，差异 >20% → I-07；
- design 测点有效集仅用于设计/报告，不参与 T6 改善观察（§8.2.1）。

### 3.7 权重与目标声明（normative）

- 频率权重默认等权（w_f=1）；若声明其它权重，必须冻结且 off/on 相同；
- 输入/位置权重（若用）必须冻结、>0、归一；不得按结果调权（调权 = FAIL-07）；
- **目标矩阵声明**必须由设计 artifact 在 holdout 采集前冻结：`T_{j,i}(f)`（M×M 复矩阵，或声明 per-position）与公共 D0；目标不得由同轮基线/on 实测反推得到（禁止循环目标）；若目标 ≈ 基线（E_off→0），按 §4.4 判 N/A，不能借近零目标伪造改善。

### 3.8 声学输出身份检查（T6-AID，normative；修 I05）

T1 partial 时，参与输出的"物理输出归属"不能只用 T4 pair 外推（4.1.2 只有 FL:SUB1/FR:SUB1 两条）。T6-M 必须执行 **acoustic output identity（T6-AID）**：

1. 在预声明声学参考位置（可复用某测点）**逐输出**播放去相关/带限噪声或低频扫频（其余输出静音），UMIK 采集并记录 file sha256；
2. 判定：目标物理扬声器（由用户标注/照片/位置记录确定）能量显著最大且直达峰在预声明窗口内；同时报告该输出的 slot→role→DAC 反查（overlay 解析）与照片/位置证据；
3. 覆盖：FL/FR/SL/SR/SUB1 每个参与输出都必须有 T4 逐通道 FR **或**本检查证据；缺 → **I-08**；身份错配 → **F-10**；
4. 报告字段：`identity_check{status, outputs[]{slot,role,dac_derived,physical_speaker,method,file_sha256}, notes}`；NC-02/NC-08 的"映射/身份检查"即指本 gate（§7.2）。

---

## 4. 指标定义（normative）

### 4.1 复频响重建与相位/D 处理

1. 从 raw（扫频 deconvolution 或直接 IR）重建每 (p,i,j) 复频响 `X_{p,i,j}(f)`；off/on/两轮使用同一分析脚本（sha256 冻结）；
2. 漂移校正方法预声明并一致应用（FC-06）；
3. **只扣除声明公共 D0**：以相位基准对齐后，从所有响应减去 D0 对应的线性相位；不按测点/通道单独找峰对齐、不使用"最佳对齐"搜索；残余群延迟必须报告；
4. 极性只依据声明校准/接线字段（已知极性），不得在 T6 分析中再次乘极性（避免双反相）；未知极性 → 该点无效；
5. 测量对象是端到端复响应（含 P_post、保护、DAC、房间、话筒）；target 必须在同一端到端平面声明；若设计使用 H_raw/H_eff 平面，artifact 必须给出与端到端平面一致的映射并冻结（hash）；
6. 平滑：I 用未平滑复响应（除声明外）；guard 的 1/3 oct 用声明平滑；T20 用 1/3 oct 带能量。

### 4.2 目标矩阵与公共 D

`D_{p,i,j}(f) = T_{j,i}(f) · exp(-j·2π·f·D0/fs)`（p 无关目标），或冻结清单声明的 per-p 形式。

要求：
- T 的幅度/相位单位、归一化锚点、D0、fs 进入 artifact；
- baseline 与 on 使用同一 D 与同一 target；
- 目标不得等于同轮 baseline 测量（E_off 近零 → §4.4 N/A，无法证明改善）。

### 4.3 误差与改善 I（定义）

对每个 (p,i)：

~~~text
F_eff(p,i) = 63–250 Hz 内通过 §3.5 质量掩码的网格点（与至少 1 个有效输出 j）
E_X(p,i)  = Σ_{j∈J} Σ_{f∈F_eff(p,i)} |X_{p,i,j}(f) − D_{p,i,j}(f)|²      (X = off, on)
E_noise(p,i) = 0.5 · Σ_{j∈J} Σ_{f∈F_eff} |X^a_{p,i,j}(f) − X^b_{p,i,j}(f)|²   (两次 off 重复，a/b)
I_raw(p,i)  = 10·log10( E_off(p,i) / E_on(p,i) )
I_use(p,i)  = 10·log10( E_off(p,i) / max(E_on(p,i), E_noise(p,i)) )   ← §8.2.1 工程观察只用 I_use（不选 I_raw；不参与 PASS/FAIL）
~~~

- `I_use` 是保守估计：当 on 误差低于测量噪声底时，不得把噪声当改善；
- 若 E_on 或 E_noise 为 0（理论边界）→ 记录实际浮点值并报告 `numeric_floor: true`；I 报告上限截断 60 dB，不参与超额加分；
- 报告必须同时给出 I_raw 与 I_use、E_off/E_on/E_noise、最差点、绝对残差与不确定度；predicted vs measured 幅相/T20 差见 §4.9；
- **预声明工程观察目标（不参与 PASS/FAIL，§8.2.1）**：所有有效 holdout (p,i) 的 I_use ≥3.0 dB；最差 I_use ≥3.0 dB；每个 input i 的 min over p ≥3.0 dB（三个条件同时成立；不允许用汇总值遮盖不达标点）。**未达目标不产生任何 FAIL/INCOMPLETE 码，不改变 PASS 判定**；观察值必须如实报告。阈值数值不变，只改判定口径。

### 4.4 近零基线 → N/A 规则（normative）

- 若 `E_off(p,i) ≤ 4 · E_noise(p,i)`（基线误差距测量噪声 <6 dB）→ 该 (p,i) 的 I 记为 **N/A**：报告 E_off/E_noise，不计算无限改善，不计入 PASS；
- N/A 点仍必须列入报告（`near_zero` 列表），不得删除；N/A 比例 >20% 或有效集低于 §3.6 → I-07；
- 若全部候选 (p,i) 均 N/A → 近零基线无法证明改善 → **INCOMPLETE**（I-07），不得 PASS；
- 近零对照（NC-09）用于证明该规则可被触发且不被误判为改善。

### 4.5 T20 与"有效 T20 窗"（normative）

**定义**：对每个 (p,i)，取 1/3 oct 带（63 Hz、125 Hz）滤波后的脉冲响应 h(t)，按 Schroeder 反向能量积分：

~~~text
L(t) = 10·log10( ∫_t^∞ h²(τ)dτ / ∫_0^∞ h²(τ)dτ )   [dB]
在 L(t) 的 [-5, -25] dB 区间做线性回归 L(t)=a+b·t（拟合区间即"T20 有效窗"）
T20 = 3 · ( t(L=-25) − t(L=-5) )   [s]    （即 60 dB 外推衰减时间）
~~~

**有效窗判据（全部满足才算有效；否则该估计 N/A）**：

| 编号 | 判据 | 说明 |
|---|---|---|
| V1 | **噪声底余量**：定义 `L_noise` = 采集尾部（IR 结束前 5% 或声明窗）RMS 相对 0 dB 能量；要求 `L(t=-25) − L_noise ≥ 10 dB`（不再另设"噪声底 ≤ -35 dB"的独立门；若 `L_noise > -35 dB` 则窗口无效） | "窗不够"的核心：-25 dB 必须高于噪声底 10 dB；边界统一为 L_noise 余量 |
| V2 | **采集时长**：≥ `1.2 × t(-25)` 且 IR 尾部达到噪声底（`L(t) ≤ L_noise + 5 dB`）；若 `L(-35)` 可测（`L(-35) ≥ L_noise + 5 dB`）再附加要求 ≥ `1.2 × t(-35)`；`t(-35)` 不可测时以噪声底停止为准 | 防尾部截断；不要求不存在的 t(-35) |
| V3 | 拟合 R² ≥ 0.99，回归残差 RMS ≤1.0 dB | 防非线性/多模态斜率 |
| V4 | 斜率 b < 0，T20 ∈ [0.05, 2.0] s | 物理合理性 |
| V5 | 带内 SNR ≥25 dB，且无削波/非有限样本 | 测量有效性 |
| V6 | 带内无失锁/丢块/xrun 影响 | FC-15 |

**聚合（先冻结，不得测后改）**：

1. 每个 (p,i,band) 得到有效 T20 估计；
2. 每个测点 p 的 T20_p = 有效输入的加权中位数（默认等权）；
3. 每个 band 的主统计量 = valid holdout 测点 T20_p 的中位数；
4. 覆盖率：每个 band 至少 3 个有效测点、≥2/3 的 (p,i) 有有效估计；不足 → 该 band N/A → I-06。

**预声明工程观察目标（不参与 PASS/FAIL，§8.2.1）**：对 63 Hz 与 125 Hz 两个 band 分别：
- `median(T20_on) ≤ 0.85 × median(T20_off)`（即有效 T20 下降 ≥15%），且
- 每个有效测点 p：`T20_on(p) ≤ 1.05 × T20_off(p)`（不得有测点恶化 >5%）。

**未达目标不产生任何 FAIL/INCOMPLETE 码，不改变 PASS；T20 观察值必须逐测点/band 如实报告。**

### 4.6 模态改善观察（说明）

- T6 的模态**观察** = §4.5 的 T20 观察值（§8.2.1，不参与 PASS/FAIL）+ 报告瀑布图/EDT（报告项，不额外设门）；
- 不承诺消除全部驻波；不得用单一频点/单一测点推断全部模态改善；
- 基线 T20、on T20、下降百分比、有效窗计数、不确定性必须逐测点/频带报告。

### 4.7 guard / 带外指标（normative）

| 编号 | 频带 | 判据 | 类型 |
|---|---|---|---|
| G-1 | 20–63 Hz | 对每个有效 holdout (p,i)：子带目标误差退化 ≤1.5 dB，即 10·log10(E_on_sub/E_off_sub) ≤ +1.5 dB；有效覆盖 ≥50% 网格点 | 门（FAIL-01） |
| G-2 | 250–300 Hz | 同 G-1 | 门（FAIL-01） |
| G-3 | 300–400 Hz | 每个有效 1/3 oct 点：|Δ合成增益| ≤2.0 dB，且目标误差退化 ≤2.0 dB；报告相位差 | 门（FAIL-01） |
| G-4 | 400 Hz–10 kHz | 每个有效 1/3 oct 点相对基线：|Δ幅度| ≤1.5 dB 且 |Δ相位| ≤20°（同 D 对齐）；有效覆盖 ≥80% | 门（FAIL-01） |
| G-5 | 10–16 kHz | 报告 | 报告 |

### 4.8 保护指标（normative）

| 编号 | 判据 | 类型 |
|---|---|---|
| PR-1 | 参考节目 PR-A（5 馈送不相关）与 PR-B（全同相最坏）经 on 图渲染：每输出数字峰值 ≤ -1 dBFS；limiter 触发率 <0.1%；无 NaN/Inf/削波计数 | 门（FAIL-02） |
| PR-2 | 全部声学测量文件 ADC 峰值 ≤ -1 dBFS，无削波；host 计数 xrun/deadline/clip 记录在案 | 门（FAIL-02） |
| PR-3 | artifact 声明每修正路径增益帽（默认软帽 ≤-6 dB，允许显式放宽至 ≤+6 dB）；由系数文件实算的每路径最大增益 ≤ 声明帽 +2 dB | 门（FAIL-02） |
| PR-4 | 静态衰减/headroom/限幅器统计报告（含峰值、触发计数、总衰减） | 报告 |

### 4.9 预测-实测一致性（honesty gate，normative）

- predicted I（来自冻结 artifact 的预测文件，hash 绑定；用 off holdout plant + 冻结 coeff 计算）与实测 I_use 逐 (p,i) 比较：`ΔI(p,i) = I_pred(p,i) − I_use(p,i)`；
- 门（只防过度宣称）：mean(ΔI) ≤ +2.0 dB 且 max(ΔI) ≤ +6.0 dB；实际优于预测不罚；
- 违反 → FAIL-03；predicted 缺失 → I-09；预测使用的 coeff hash 与 apply 的 hash 不一致 → FAIL-08。
- **predicted vs measured 幅相/T20 差（WBS T68 落地，修 I10）**：checker 必须在 holdout 上比较预测复响应（off plant+冻结 coeff 模型）与实测 on 复响应：25–300 Hz、1/6 oct：`median|Δmag| ≤2.0 dB`、`max|Δmag| ≤6.0 dB`、`median|Δphase| ≤20°`（加权 \|H\|）；并比较 `T20_pred` vs `T20_meas`：每个 band `|ΔT20|/T20 ≤15%`（报告逐点）；超门 → FAIL-03（预测与实测不一致），缺预测模型 → I-09。报告字段 `predicted_vs_measured`。

### 4.10 不确定度与统计报告（normative）

报告必须包含：I_use 的中位数/最小值/最大值/四分位；每 input 的 min/median；N/A 计数与原因；off/on 的 E 分布；两轮逐 (p,i) 的 ΔI；T20 每个测点/band 的估计与有效窗参数（-5/-25 dB 时刻、R²、噪声底余量）；guard/保护/预测一致性统计与不确定性说明。

---

## 5. 独立 holdout 与密封（normative）

### 5.1 定义与禁止用途

holdout 是**从未参与调参**的测点集合。禁止用于：λ/CV、白名单/稀疏选择、Δ/D 选择、target/curve 设计、gain/limiter 调整、analysis 参数选择、人工挑选测点；任何一项违反 → FAIL-05。

### 5.2 选点与声明

- 选点方法（如按座位/声学中心随机或分层选点）在 design 前声明并记录；
- holdout label/role=validation/weight 必须进入冻结清单；
- holdout 数量与空间分布满足 §3.2；不得事后替换/剔除（只能按预声明质量掩码无效）。

### 5.3 密封协议（hash commitment）

1. 设计 artifact 冻结时记录 hash（coeff/target/design manifest）；
2. holdout 首次采集**之前**提交 seal：holdout 测点清单 hash + 分析脚本 hash + checker hash + artifact hash + predicted 计算脚本 hash；
3. 采集后立即记录每个 raw 文件 sha256/bytes；
4. 设计 manifest 的 `tuning_inputs_sha256` 仅允许 design raw/配置文件；出现 holdout raw hash → FAIL-05；
5. 分析由 checker 执行；producer 不得在 verdict 前读取 holdout 分析结果并回改系数；artifact hash 在 apply 后不得变化（除按版本重启 job，且 holdout 重测）；
6. **跨轮预注册**：Round B 的设计参数与 artifact 必须在 Round A holdout 首次分析/unseal 前预注册并 hash（§5.6）；破坏则 FAIL-05。

### 5.4 评估与过拟合判据

- **改善观察只在 holdout 上报告**（design 点结果报告但不参与观察结论；观察值不参与 PASS/FAIL，§8.2.1）；
- 过拟合/泄漏告警门（证据独立性，不是改善幅度门）：holdout 的最差 I_use 比 design 最差 I_use 低 >6 dB → FAIL-05（overfit）；
- holdout 点数/有效集不满足 → I-03/I-07；不得用 design 点替补。

### 5.5 第二轮 holdout

每轮使用**新一轮采集的 holdout raw**（同测点、同条件、新 session）；不得复用第一轮 raw；轮间 holdout 位置保持冻结。

### 5.6 跨轮独立性与预注册（normative；修 I04）

两轮同位置复测 + 顺序执行时，Round A 的 holdout 结果/残差一旦 unseal 即可被人工用于调 Round B 的 target/D0/PEQ/gain/positions/analysis 参数——这会破坏“独立复现”。T6-M 必须：

1. **预注册先于 unseal**：Round B 的 artifact（coeff manifest）/target/D0/positions/quality mask/analysis 参数集，必须在 **Round A holdout 首次分析 unseal 之前**写入 `cross_round_prereg` 并提交 sha256（建议 A/B 双盲预注册：A、B 参数在任一 holdout unseal 前同时 hash）；
2. **禁止使用前轮 holdout**：Round B 设计说明必须声明未使用任何前轮 holdout 的 I/T20/残差/人工听感结果调参；`tuning_inputs_sha256` 不得出现前轮 holdout raw hash；
3. **checker 审时间线**：验证 `seal.round_A_holdout_unseal_utc > cross_round_prereg.round_B_committed_utc`，并记录两轮 artifact/tuning 输入时间线；
4. **违反** → **FAIL-05**（跨轮污染），不得把第二轮当作独立复现；NC-07 注入扩展为“含跨轮 holdout 调参/预注册缺失”；
5. 报告字段：`cross_round_prereg{sha256, committed_utc, round_a_unseal_utc, rounds:{A,B}, declared_no_prior_holdout_use:true}`。

---

## 6. 两轮独立重复复现（normative）

### 6.1 轮次定义与独立性

- Round A、Round B 各为一次**完整 job**：design raw → design/求解 → artifact 冻结 → apply → T3-equivalent → off/on + holdout 复测 → checker；
- 每轮独立：新 session_id；设备关闭/重新打开；话筒重新摆位并记录；新 design raw；新 artifact（系数可不同）；新 holdout raw；
- 两轮共用：room/profile/layout/participants/levels/positions/冻结清单/checker/quality mask；**两轮设计参数/artifact 必须按 §5.6 跨轮预注册**（B 在 A holdout unseal 前 hash），且 B 不得使用 A holdout 结果调参；
- 不得把两轮 raw 合并后只报总体（每轮必须独立满足 §8.2）；
- 一轮 PASS、一轮 FAIL → FAIL（F-06）；一轮无效 → INCOMPLETE（I-05）；两轮都无效 → NOT_RUN。

### 6.2 每轮证据（必须齐全）

session manifest、冻结清单 hash、design raw hash、design artifact（coeff/manifest/target/predicted 输入）、apply ack/actual version、T3-equivalent、holdout seal、**cross_round_prereg（§5.6，含 committed/unseal 时间线）**、off/on raw+hash、predicted、checker 复算报告、verdict 行。

### 6.3 复现判据

1. Round A 与 Round B **各自**满足 §8.2 全部 PASS 条件（每轮独立判）；
2. 跨轮一致性：对两轮均有效的 (p,i)，`|I_use,A − I_use,B|` 的中位数 ≤2.0 dB 且最大值 ≤6.0 dB（跨轮可复现性门，属证据完整性；不是改善幅度门）；超出 → FAIL-06；
3. 方向一致：两轮同一有效 (p,i) 的 I_use 符号一致（不得一轮改善一轮恶化）；该条只作跨轮可复现性证据，不设改善幅度门；
4. N/A 集合两轮一致（差异 >20% → I-07）；
5. T20 两项观察值两轮并列报告；跨轮 T20 下降率差异 ≤10 个百分点为稳定性参考报告项（不设 FAIL 码；T20 观察不参与 PASS/FAIL，§8.2.1）；
6. **跨轮预注册与独立性**：满足 §5.6（B 预注册早于 A holdout unseal、B 未使用 A holdout 调参、时间线可证）；违反 → FAIL-06（独立性不可证）。

### 6.4 统计与报告

- 逐轮报告 + 合并描述（不合并判据）；报告两轮 I/T20 的 spread（median/max |ΔI|、T20 下降率范围）；
- 若附加第 3 轮，仅作报告，不改变判定；不得只挑最好一轮签字（发现 → FAIL-08）；
- 置信区间/不确定度不得用未声明的统计方法。

### 6.5 无效轮与重测规则

- 因设备/环境/时间轴/质量掩码不满足导致某轮无效 → 该轮按 I-02/I-05 处理，需全新 session 重跑整轮（不得只补部分测点）；
- 重跑必须保留旧轮原始文件与失败原因（不得覆盖/删除）；
- 看到失败结果后修改阈值/掩码/测点 → FAIL-07/FAIL-08。

---

## 7. 近零与负对照（N/A 与 NC-01..NC-10）

### 7.1 近零基线（N/A）

见 §4.4：E_off ≤ 4·E_noise → I=N/A；N/A 不得计为通过；N/A 比例超限或全部 N/A → INCOMPLETE；报告必须单列 `near_zero` 列表与原因（近零对照 NC-09 验证规则生效）。任务口径中的"近零 N-A"即本节的 N/A（不可判，不等于 pass；也不等于 0 dB 改善）。

### 7.2 负对照清单（必须在同一 pipeline/checker 上执行）

| ID | 注入/情景 | 预期检出（任一对应检查失败即算检出） |
|---|---|---|
| NC-01 | 用 G=I（identity，同 D0）充当"已应用系数" | checker 必须检出并如实报告 I_use≈0±0.5 dB（低于 3.0 dB 工程观察目标，§8.2.1）与 `observation_met:false`；不得把该观察写成达标改善；必须被检出（观察口径，不产生改善 FAIL 码） |
| NC-02 | 对调两个 participant 的 src/dst 索引（或 manifest 映射错位） | **§3.8 T6-AID 身份 gate 失败（slot→role→DAC→物理输出错配/F-10）**，或受影响 (p,i) 的 I_use 观察值明显下降；必须被检出 |
| NC-03 | 单条非对角路径极性反相 | 受影响 (p,i) 的 I_use 观察值下降，或预测一致性超门；必须被检出 |
| NC-04 | 截断系数尾部 25%（去尾） | T20 观察值低于目标（§8.2.1）或预测一致性/时域尾检查失败；必须被检出 |
| NC-05 | 声明/对齐公共 D 错 1 ms（24 samples@48k）后照常分析 | 相位/D 完整性检查失败（残差相位超噪声/超出声明），或 I 观察值异常；必须被检出 |
| NC-06 | on 响应事后 +1 dB 归一/增益（未声明） | 电平不变性、保护/增益或 honesty 检查失败；必须被检出 |
| NC-07 | 用 holdout raw 参与调参/替换 holdout 文件（seal 不匹配），或 **Round B 使用 Round A holdout 结果调参/缺跨轮预注册** | 单轮：seal/调参输入检查失败；跨轮：§5.6 预注册/时间线检查失败 → FAIL-05；必须被检出 |
| NC-08 | 采集 slot/通道映射整体错 1 位（时间轴/路由错） | **§3.8 T6-AID 身份 gate**、timebase/电平完整性检查失败；必须被检出 |
| NC-09 | 近零基线对照：**flat/无误差 plant 或 E_off≤4E_noise 的合成数据**（baseline 与 on 相同） | 满足 `E_off≤4E_noise` → 输出 I=N/A（非无限改善、非 PASS）；若 `E_off>4E_noise` 而 I_use≈0 → 必须按实测报告 I_use≈0 观察值（`observation_met:false`），不得 N/A、不得宣称达标改善；两种分支都必须被检出（观察口径，不产生改善 FAIL 码） |
| NC-10 | 只在汇总平均上造假：注入单点失败但总体平均仍达标 | 逐 (p,i)/最差观察值必须暴露注入单点，汇总不得遮盖；必须被检出 |

### 7.3 执行与通过判据

1. 负对照必须由独立 checker 执行/复核，raw+注入方法+预期失败项+实际检出项全部留证；
2. **10/10 检出**；
3. 好配置（正常通过候选）连续 3 次复跑，**0 误报**（不得把好配置判为任一 NC 失败）；
4. 任一 NC 漏检或误报 → FAIL-04（T6-M 不得 PASS）；
5. NC-09 判定必须覆盖两个分支（N/A 分支与实测 I_use≈0 分支）；只测其一不算检出（修 I07）；
6. NC 报告必须含 `false_positive`（好配置 3 次复跑是否误报）与 `reruns`（复跑次数/结果）字段（修 I09）。

---

## 8. 判定状态、PASS/FAIL/INCOMPLETE 规则

### 8.1 状态枚举

| 状态 | 含义 |
|---|---|
| PASS | 全部前置证据有效，两轮各自满足 §8.2 全部条件（证据完整性 + holdout 隔离 + guard/保护门），负对照 10/10、误报 0，报告完整；§8.2.1 工程观察项不参与判定 |
| FAIL | 存在明确门失败（§8.3 任一 F 码），即使其它项 INCOMPLETE 也判 FAIL |
| INCOMPLETE | 无明确门失败，但存在缺失/无效/不足证据（§8.4 任一 I 码），不能签字 |
| NOT_RUN | 未执行 T6 测量（P1 前/无硬件/无系数/无授权窗口） |

### 8.2 PASS 条件（全部必须成立）

| ID | 条件 |
|---|---|
| P-01 | 前置证据有效：T0–T4-equivalent 与 T1 partial 声明完整；**同轮 B2 T6-cal 独立 PASS（§5.2，非伪 holdout）**；T5-window xrun/deadline/clip = 0 且真实（§1.2/§1.5） |
| P-02 | 冻结条件清单完整、hash 一致、`frozen_at_utc` 早于 holdout 首采（FC-01..FC-16），**FC-05/06/09/10/14/15 的 §2.1 schema 字段齐全** |
| P-03 | 测点与标签满足 §3.2/§3.6 最低要求（design ≥6、holdout ≥3、有效集达标） |
| P-04 | holdout 密封与未调参证据通过（§5.3/§5.4），无 holdout 污染；**跨轮预注册 §5.6 满足**（B 预注册早于 A holdout unseal、B 未使用 A holdout 结果） |
| P-05 | Round A 与 Round B 均为完整独立 job（§6.1/§6.2） |
| P-06 | 每轮 guard/带外门通过（G-1..G-4，§4.7） |
| P-07 | 每轮保护门通过（PR-1..PR-3，§4.8） |
| P-08 | 每轮预测-实测 honesty gate 通过（§4.9，含 over-claim 与 predicted vs measured 幅相/T20 差） |
| P-09 | 负对照 10/10 检出、0 误报；NC-09 两分支均检出；报告含 `false_positive/reruns`（§7.3） |
| P-10 | 跨轮复现性满足 §6.3（中位 |ΔI| ≤2 dB、max ≤6 dB、方向一致、N/A 一致、§5.6 时间线可证；跨轮可复现性属证据完整性，不是改善幅度门） |
| P-11 | 报告完整：schema 必填（含 `t6_kind/t6_cal/identity_check/predicted_vs_measured/cross_round_prereg/engineering_observations`）、raw→analysis→report hash 链完整、claims/limits/untested 齐全、checker 独立且可复现（§9） |

> **判定边界（Cursor v0.9 终审 F-19，用户已裁决）**：本表只含**证据完整性、holdout 隔离与 guard/保护门**；I_use 改善值与 T20 下降值是 §8.2.1 的**预声明工程观察项**，明确**不参与本表、不得决定 PASS/FAIL**。

### 8.2.1 预声明工程观察项（不参与 PASS/FAIL）

> 用户裁决（2026-11-17，Cursor v0.9 终审 F-19）：**I_use ≥3.0 dB 与 T20 下降 ≥15% 只是工程观察，不得决定验收 PASS/FAIL**。本节两项只用于工程评估与报告：**不参与 §8.2 PASS 判定，不产生任何 §8.3 FAIL 码或 §8.4 INCOMPLETE 码，也不得在 verdict 中据此降级或升级**。阈值数值保持 v1 预声明值不变（3.0 dB / 15% / 5%），只改判定口径。

| ID | 预声明观察目标（阈值不变） | 计算/报告要求 | 判定影响 |
|---|---|---|---|
| O-01 | 有效 holdout (p,i) 的 I_use ≥3.0 dB；最差 ≥3.0 dB；每 input min ≥3.0 dB（§4.3） | 逐 (p,i)/最差/每输入与 N/A 全量报告（§4.10）；两轮并列 | **无：不参与 PASS/FAIL**；仅观察；未达不改变 verdict |
| O-02 | 每轮 T20 63 Hz 与 125 Hz 中位数下降 ≥15%、无测点恶化 >5%（§4.5） | 逐测点/band 报告有效窗参数与观察值；两轮并列 | **无：不参与 PASS/FAIL**；仅观察；未达不改变 verdict |

1. **报告字段**：每轮 `metrics.engineering_observations` 必须给出 `predeclared_targets`（i_use_min_db/t20_reduction_pct/t20_max_point_worsening_pct）、`observed`（min_improve_db/band_63_reduction_pct/band_125_reduction_pct/max_point_worsening_pct）、`observation_met`（是否达到观察目标）与 `affects_verdict: false`；字段缺失按 I-10（报告 schema 不完整）处理，**观察值不达标本身不是失败**；
2. **不得反向使用**：观察达标也不得代替任何证据/隔离/保护门；观察未达不得写 FAIL/INCOMPLETE，也不得据此拒绝签字；verdict 只按 §8.2/§8.3/§8.4；
3. **两轮观察并列**：跨轮差异只作稳定性说明（§6.3 第 5 条），不设 FAIL 码；
4. **负对照**仍需检出观察异常（§7.2），但检出标准是"报告如实呈现观察值/覆盖门"，不依赖 §8.2.1 观察值是否达标；
5. **checker** 必须输出观察值与 `affects_verdict: false`，不得用观察值计算 PASS/FAIL。

### 8.3 FAIL 码（明确门失败；优先级高于 INCOMPLETE）

| 码 | 触发条件 |
|---|---|
> 判定边界（F-19）：I_use/T20 **观察值不达标不产生任何 F 码**；下表不含改善幅度门（观察项见 §8.2.1，只报告、不判定）。原 v1.1 的 F-01/F-02（改善门）已删除，F-03..F-12 依次前移为 F-01..F-10。

| 码 | 触发条件 |
|---|---|
| F-01 | guard/带外门失败（G-1..G-4） |
| F-02 | 保护门失败（PR-1..PR-3，含削波/NaN/limiter 超限/增益帽超限） |
| F-03 | 预测-实测一致性门失败（over-claim mean ΔI >+2 dB 或 max >+6 dB；或 predicted vs measured 幅相/T20 差超门，§4.9） |
| F-04 | 负对照漏检或误报（<10/10 或好配置误报 >0） |
| F-05 | holdout 污染/密封失败/调参输入包含 holdout/**跨轮预注册缺失或 B 使用 A holdout 结果**/过拟合门失败 |
| F-06 | 跨轮不可复现（spread 超门或一轮 PASS 一轮 FAIL 或方向不一致） |
| F-07 | 未声明的事后平移/归一/电平/参数调整，或冻结后改阈值/掩码/测点 |
| F-08 | 报告/证据与 raw 不一致（篡改、删点、汇总遮盖、预测冒充实测） |
| F-09 | 前置证据自身明确 FAIL（T0–T4-equivalent 或 T1 partial 声明中的失败项），且未被修复/重测 |
| F-10 | 声学输出归属确认错误（声明为某角色的输出实际是另一物理输出） |

### 8.4 INCOMPLETE 码（缺失/无效，不能签字）

| 码 | 触发条件 | 补救 |
|---|---|---|
| I-01 | 前置证据缺失/NOT_RUN（T0–T4-equivalent 或 T1/T2/T3/T4 证据） | 补齐并重跑受影响轮 |
| I-02 | 冻结条件不完整或违反后重测不充分 | 重建冻结清单，全新轮次 |
| I-03 | 测点/标签数量不足（design/holdout 低于 §3.2，或 5+3 pilot） | 按正式点数重测 |
| I-04 | holdout 密封不可证（顺序/时点/hash 缺失） | 重新 seal 并整轮重测 |
| I-05 | 两轮不完整或一轮无效 | 补/重跑整轮 |
| I-06 | T20 有效窗不足（某必需 band 有效测点 <3 或覆盖率 <2/3） | 改善 SNR/时长重测；不得改观察目标 |
| I-07 | 有效集不足（N/A/无效超限或两轮 N/A 差异 >20%） | 检查测量/环境后重测 |
| I-08 | T1 partial 未覆盖输出缺声学 ID/替代证据 | 补 T4 逐通道/身份证据 |
| I-09 | 预测/设计模型产物缺失（predicted/target/coeff hash 链断） | 补齐并冻结后重测 holdout |
| I-10 | raw 文件/hash 链/报告 schema 不完整 | 补齐证据包 |
| I-11 | checker 非独立或不可复现（调用 producer 聚合/随机/版本漂移） | 换独立 checker 重算 |
| I-12 | guard 频段有效覆盖不足（未达 §4.7 覆盖率） | 重测/声明范围并降级 |

### 8.5 优先级与不可回溯

- 判定优先级：FAIL > INCOMPLETE > NOT_RUN > PASS（即任一 F → FAIL；无 F 有 I → INCOMPLETE；全 P → PASS）；**§8.2.1 工程观察项不进入本优先级、不改变 PASS/FAIL**；
- N/A 是**指标级**状态，不是 T6 状态；N/A 不得累计成 PASS；
- 阈值/字段/公式一旦用于正式测量即冻结；发现不可达/错误必须**先升版并重测**，不得测后修改（F-07/F-08）；
- 本规范**不提供**降级 PASS 档：范围限制只写进 `verdict.limits`，不改变 PASS 所需硬门（T1 partial 不豁免 §1.3）。所谓"有条件通过/降级签收"在本规范下按 §8.3/§8.4 记为 FAIL 或 INCOMPLETE，不得写 PASS。

### 8.6 重测与修复

- FAIL 后修复必须产生新 artifact hash 与新轮次（A'/B'），旧失败证据保留；
- 不得选择性重测失败点后拼成 PASS（整轮重跑）；
- INCOMPLETE 补齐后按新轮次重测；不得复用旧 raw 补点。

---

## 9. 报告与证据格式

### 9.1 机器可读 schema（khaen-mimo-t6-report/v1）必填字段

| 字段 | 类型 | 必填 | 约束/说明 |
|---|---|---|---|
| schema | string | 是 | 必须 = `khaen-mimo-t6-report/v1` |
| report_id | string | 是 | 唯一；示例报告必须含 `example:true` |
| example | bool | 否 | true 表示示例，禁止作为验收报告 |
| spec | object | 是 | path/version/sha256（本规范） |
| authority | object | 是 | wbs_sha256/decision_sha256 及版本 |
| created_utc | string | 是 | ISO8601 |
| scope | object | 是 | layout/participants/dense_to_dac/fs_hz/gate_bands_hz/rounds |
| **t6_kind** | string | 是 | 必须 = `MIMO-T6`；区分 B2 普通校准 `T6-cal`（I01） |
| **t6_cal** | object | 是 | 同轮 B2 T6-cal 结果：`status`/`n_points`/`positions`/`design_points`/`residual_mean_db`/`residual_max_db`/`passed`/`report_sha256` |
| prerequisites | object | 是 | t0_artifact_validation/t1_partial/t2_repeatability/t3_post_apply/t4_pair_or_single/t5_window_xrun（各含 status 与 evidence hash） |
| environment | object | 是 | 设备、channels、chunksize、mic、orientation、温度 |
| frozen_conditions | object | 是 | manifest_path/manifest_sha256/frozen_id/common_d_samples/phase_ref/**timeline/clock/temperature/seal/cuemix 字段（引用 §2.1；修 I03）** |
| stimulus | array | 是 | role/path/sha256/bytes/fs_hz/level_dbfs/seconds |
| rounds | array | 是 | 长度 ≥2；每轮 id/session/design 引用/metrics/verdict |
| negative_controls | array | 是 | 长度 =10；id/detected/expected_check/detail/**false_positive/reruns/injection_sha256** |
| **identity_check** | object | 是 | §3.8 T6-AID：status/outputs[]{slot,role,dac_derived,physical_speaker,method,file_sha256}/notes |
| verdict | object | 是 | status ∈ {PASS,FAIL,INCOMPLETE,NOT_RUN}/reasons/claims/limits/untested |
| evidence_files | array | 是 | path/sha256/bytes（raw→analysis→report 链） |

每轮 `rounds[i].metrics` 至少包含：

| 字段 | 说明 |
|---|---|
| holdout | effective/na/min_improve_db/median/worst/per_input_min_db/design_vs_holdout_gap_db |
| t20 | band_63/band_125（valid/median_off_s/median_on_s/reduction_pct/valid_positions）/window_summary |
| guard | G-1..G-4 各自 status + 实测统计 |
| protection | PR-1..PR-4 各自 status + peak/limiter counts |
| prediction | mean_overclaim_db/max_overclaim_db/per_point（I_pred vs I_use） |
| **engineering_observations** | §8.2.1 预声明工程观察：predeclared_targets（i_use_min_db/t20_reduction_pct/t20_max_point_worsening_pct）/observed（min_improve_db/band_63_reduction_pct/band_125_reduction_pct/max_point_worsening_pct）/observation_met/**affects_verdict:false**（不参与 PASS/FAIL） |
| **predicted_vs_measured** | 25–300 Hz：median/max |Δmag| dB、median |Δphase|°（加权）、逐点表；T20：pred_off/pred_on/meas_off/meas_on/|ΔT20|% 每 band（I10） |
| **measurement_validity** | 每文件 xrun/deadline/lost_lock/dropped/capture_peak/snr_db/snr_lowband_db/repeats/valid（FC-15/I03） |
| near_zero | (p,i) 列表 + E_off/E_noise + reason |
| reproducibility_inputs | 两轮对比：median_abs_delta_db/max_abs_delta_db/na_mismatch |

每轮 `rounds[i].design` 至少包含：`coeff_manifest_sha256`/`target_sha256`/`tuning_inputs_sha256`/`holdout_seal{committed_utc,raw_sha256}`/**`cross_round_prereg{sha256,committed_utc,round_a_unseal_utc,declared_no_prior_holdout_use:true}`（§5.6）**/`t6_cal_report_sha256`。每个测量文件必须带 `timeline`/`clock`/`temperature`/`measurement_validity` 元数据（可引用 raw sidecar，但其 sha256 必须入 `evidence_files` 链）。

### 9.2 示例 JSON（**illustrative only**；example:true）

~~~json t6-report
{
  "schema": "khaen-mimo-t6-report/v1",
  "report_id": "EXAMPLE-T6-DO-NOT-USE-20261116-A",
  "example": true,
  "spec": {"path": "rt-stack/spec/mimo-t6.md", "version": "khaen-mimo-t6/v1", "sha256": "0000000000000000000000000000000000000000000000000000000000000000"},
  "t6_kind": "MIMO-T6",
  "t6_cal": {"status": "pass", "n_points": 3, "positions": ["P1", "P2", "P3"], "design_points": ["D1", "D2", "D3", "D4", "D5", "D6"], "residual_mean_db": 1.1, "residual_max_db": 2.6, "passed": true, "report_sha256": "0000000000000000000000000000000000000000000000000000000000000000"},
  "authority": {"wbs_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "decision_sha256": "0000000000000000000000000000000000000000000000000000000000000000"},
  "created_utc": "2026-11-16T16:00:00Z",
  "scope": {"layout": "4.1.2", "participants": ["FL", "FR", "SL", "SR", "SUB1"], "dense_to_dac": [0, 1, 2, 3, 7], "fs_hz": 48000, "gate_bands_hz": {"improve": [63, 250], "t20": [63, 125]}, "rounds": ["A", "B"]},
  "prerequisites": {
    "t0_artifact_validation": {"status": "pass", "evidence_sha256": ["0000000000000000000000000000000000000000000000000000000000000000"]},
    "t1_partial": {"status": "partial", "loopback_map_1based": [{"play": 9, "capture": 7}], "covered_outputs_1based": [9], "uncovered_outputs_1based": [1,2,3,4,5,6,7,8,10], "cuemix_routing": {"in7_to_out7": "on", "in8_to_out8": "on", "other_input_to_output": "none", "out7_out8_downstream": "disconnected"}, "final_insert_direct_routes": "disabled", "mitigation": ["file_backend", "t4_acoustic", "acoustic_output_id"], "claim": "partial: single-path electrical loopback; other outputs via file backend + T4 acoustic + T6 output identity"},
    "t2_repeatability": {"status": "pass"},
    "t3_post_apply": {"status": "pass"},
    "t4_pair_or_single": {"status": "pass", "pairs": ["FL:SUB1:80", "FR:SUB1:80"]},
    "t5_window_xrun": {"status": "pass", "xrun": 0, "deadline_miss": 0}
  },
  "environment": {"playback": "hw:motu,0", "capture": "hw:umik1,0", "channels": 22, "chunksize": 128, "temperature_c": 22.0, "mic": {"model": "UMIK-1", "serial": "EXAMPLE", "cal_file_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "orientation": "90deg"}},
  "frozen_conditions": {"manifest_path": "example/frozen.json", "manifest_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "frozen_id": "EXAMPLE-FROZEN", "common_d_samples": 120, "phase_ref": "electrical_ref_ch0", "timeline": {"render_frame_index": 0, "slot": 0, "capture_sample_index": 0, "session_id": "EXAMPLE-SESSION-A", "epoch": 1, "graph_id": "EXAMPLE-GRAPH", "timeline_method": "ppm-resample", "sidecar_sha256": "0000000000000000000000000000000000000000000000000000000000000000"}, "clock": {"clock_ppm": 3.2, "clock_ppm_uncertainty_ppm": 1.1, "drift_significant": false, "correction_method": "none", "residual_phase_10k_deg": 0.0, "residual_span_s": 10.4, "sigma_ppm": 0.4}, "temperature": {"baseline_c": 22.0, "on_c": 22.1, "rounds_c": [{"round": "A", "design_c": 22.0, "on_c": 22.1}], "delta_max_k": 0.1}, "seal": {"seal_id": "EXAMPLE-SEAL", "seal_committed_utc": "2026-11-16T15:00:00Z", "points_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "analysis_script_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "checker_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "artifact_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "predicted_script_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "cross_round_prereg_sha256": "0000000000000000000000000000000000000000000000000000000000000000"}, "cuemix": {"snapshot_text_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "snapshot_image_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "routing": {"in7_to_out7": "on", "in8_to_out8": "on", "other_input_to_output": "none"}, "final_insert_direct_routes": "disabled", "out7_out8_downstream": "disconnected"}},
  "stimulus": [{"role": "lowband_sweep", "path": "example/sweep.wav", "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "bytes": 0, "fs_hz": 48000, "level_dbfs": -30, "seconds": 30}],
  "rounds": [
    {"id": "A", "session": "EXAMPLE-SESSION-A", "design": {"coeff_manifest_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "target_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "tuning_inputs_sha256": ["0000000000000000000000000000000000000000000000000000000000000000"], "cross_round_prereg": {"sha256": "0000000000000000000000000000000000000000000000000000000000000000", "committed_utc": "2026-11-16T13:00:00Z", "round_a_unseal_utc": "2026-11-16T15:10:00Z", "declared_no_prior_holdout_use": true}, "t6_cal_report_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "holdout_seal": {"committed_utc": "2026-11-16T15:00:00Z", "raw_sha256": ["0000000000000000000000000000000000000000000000000000000000000000"]}}, "metrics": {"holdout": {"effective": 15, "na": 0, "min_improve_db": 4.2, "median_improve_db": 9.1, "per_input_min_db": {"FL": 4.2, "FR": 4.4, "SL": 4.9, "SR": 5.1, "SUB1": 6.0}, "design_vs_holdout_gap_db": 1.2}, "t20": {"band_63": {"valid": true, "median_off_s": 0.42, "median_on_s": 0.31, "reduction_pct": 26.2, "valid_positions": 3}, "band_125": {"valid": true, "median_off_s": 0.35, "median_on_s": 0.29, "reduction_pct": 17.1, "valid_positions": 3}, "window_summary": "V1-V6 ok"}, "guard": {"G1": "pass", "G2": "pass", "G3": "pass", "G4": "pass"}, "protection": {"PR1": "pass", "PR2": "pass", "PR3": "pass", "peak_dbfs": -1.2, "limiter_ratio": 0.0002}, "prediction": {"mean_overclaim_db": 0.6, "max_overclaim_db": 1.3}, "engineering_observations": {"predeclared_targets": {"i_use_min_db": 3.0, "t20_reduction_pct": 15.0, "t20_max_point_worsening_pct": 5.0}, "observed": {"min_improve_db": 4.2, "band_63_reduction_pct": 26.2, "band_125_reduction_pct": 17.1, "max_point_worsening_pct": 2.0}, "observation_met": true, "affects_verdict": false}, "predicted_vs_measured": {"median_abs_mag_db": 1.1, "max_abs_mag_db": 2.4, "median_abs_phase_deg": 9.0, "t20_delta_pct": 4.2}, "measurement_validity": {"files_valid": 12, "xrun": 0, "deadline_miss": 0, "capture_peak_dbfs": -6.0, "snr_lowband_db": 31.0}, "near_zero": [], "reproducibility_inputs": {"median_abs_delta_db": 0.8, "max_abs_delta_db": 2.1, "na_mismatch": 0}}, "verdict": "PASS"},
    {"id": "B", "session": "EXAMPLE-SESSION-B", "design": {"coeff_manifest_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "target_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "tuning_inputs_sha256": ["0000000000000000000000000000000000000000000000000000000000000000"], "cross_round_prereg": {"sha256": "0000000000000000000000000000000000000000000000000000000000000000", "committed_utc": "2026-11-16T13:00:00Z", "round_a_unseal_utc": "2026-11-16T15:10:00Z", "declared_no_prior_holdout_use": true}, "t6_cal_report_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "holdout_seal": {"committed_utc": "2026-11-16T15:40:00Z", "raw_sha256": ["0000000000000000000000000000000000000000000000000000000000000000"]}}, "metrics": {"holdout": {"effective": 15, "na": 0, "min_improve_db": 3.8, "median_improve_db": 8.7, "per_input_min_db": {"FL": 3.8, "FR": 4.0, "SL": 4.6, "SR": 4.8, "SUB1": 5.5}, "design_vs_holdout_gap_db": 1.5}, "t20": {"band_63": {"valid": true, "median_off_s": 0.41, "median_on_s": 0.30, "reduction_pct": 26.8, "valid_positions": 3}, "band_125": {"valid": true, "median_off_s": 0.36, "median_on_s": 0.30, "reduction_pct": 16.7, "valid_positions": 3}, "window_summary": "V1-V6 ok"}, "guard": {"G1": "pass", "G2": "pass", "G3": "pass", "G4": "pass"}, "protection": {"PR1": "pass", "PR2": "pass", "PR3": "pass", "peak_dbfs": -1.1, "limiter_ratio": 0.0003}, "prediction": {"mean_overclaim_db": 0.7, "max_overclaim_db": 1.6}, "engineering_observations": {"predeclared_targets": {"i_use_min_db": 3.0, "t20_reduction_pct": 15.0, "t20_max_point_worsening_pct": 5.0}, "observed": {"min_improve_db": 3.8, "band_63_reduction_pct": 26.8, "band_125_reduction_pct": 16.7, "max_point_worsening_pct": 2.0}, "observation_met": true, "affects_verdict": false}, "predicted_vs_measured": {"median_abs_mag_db": 1.2, "max_abs_mag_db": 2.6, "median_abs_phase_deg": 10.0, "t20_delta_pct": 4.8}, "measurement_validity": {"files_valid": 12, "xrun": 0, "deadline_miss": 0, "capture_peak_dbfs": -6.0, "snr_lowband_db": 30.0}, "near_zero": [], "reproducibility_inputs": {"median_abs_delta_db": 0.9, "max_abs_delta_db": 2.4, "na_mismatch": 0}}, "verdict": "PASS"}
  ],
  "identity_check": {"status": "pass", "outputs": [{"slot": 0, "role": "FL", "dac_derived": 0, "physical_speaker": "FL", "method": "per-output excitation + UMIK energy/onset", "file_sha256": "0000000000000000000000000000000000000000000000000000000000000000"}], "notes": "all five participant outputs evidenced (example truncated)"},
  "negative_controls": [
    {"id": "NC-01", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "improve_below_3db", "detail": "identity candidate not PASS"},
    {"id": "NC-02", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "mapping_mismatch", "detail": "swapped src/dst detected"},
    {"id": "NC-03", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "improve_drop", "detail": "inverted path detected"},
    {"id": "NC-04", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "t20_fail", "detail": "truncated tail detected"},
    {"id": "NC-05", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "phase_residual", "detail": "undeclared D shift detected"},
    {"id": "NC-06", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "level_invariance", "detail": "undeclared gain detected"},
    {"id": "NC-07", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "holdout_seal", "detail": "holdout contamination detected"},
    {"id": "NC-08", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "slot_mapping", "detail": "slot offset detected"},
    {"id": "NC-09", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "near_zero_na", "detail": "near-zero baseline reported N/A not PASS"},
    {"id": "NC-10", "detected": true, "false_positive": false, "reruns": 3, "injection_sha256": "0000000000000000000000000000000000000000000000000000000000000000", "expected_check": "per_point_gate", "detail": "single bad point not hidden by average"}
  ],
  "verdict": {"status": "PASS", "reasons": [], "claims": ["EXAMPLE ONLY", "T6 gates met in both rounds under frozen conditions"], "limits": ["T1 partial: single-loopback electrical mapping; other outputs via T4 acoustic"], "untested": ["5.3.2", "AES67/Dante endpoints"]},
  "evidence_files": [{"path": "example/raw/example.raw", "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "bytes": 0}]
}
~~~

示例校验：必须能被 §9.7 的 checker 当作结构样例解析；示例中的零 hash/EXAMPLE 标记表示**不是真实证据**。

### 9.3 人读报告必须包含

1. 范围与 claim/limits/untested 声明（含 T1 partial 状态、5.3.2/AES67/Dante 未测项）；
2. 冻结条件清单摘要（FC-01..FC-16）与 hash；
3. 每轮：design artifact hash、apply actual version/ack、design/holdout 有效集与 I 表（逐 (p,i)）、E_off/E_on/E_noise；
4. T20 每测点/band 表（有效窗参数、R²、噪声底余量、下降率）；另列 §8.2.1 工程观察项（I/T20 观察值、observation_met、`affects_verdict:false`），明确不参与 PASS/FAIL；
5. guard/保护/预测一致性表，**predicted vs measured 幅相/T20 差（§4.9）**；
6. T1 partial 下 **§3.8 T6-AID 声学身份检查**逐输出表（slot→role→DAC→物理输出）；
7. 负对照 10 项结果、`false_positive/reruns` 与 NC-09 两分支；
8. 两轮复现对比、§5.6 跨轮预注册时间线与不确定度；
9. **T6-cal（B2 普通 holdout）独立报告摘要**（n_points/positions/residual/passed/报告 hash）与 T6-M 的关系声明；
10. 判定（PASS/FAIL/INCOMPLETE）与失败码/缺失码、原始证据路径与 sha256、复现命令。

### 9.4 证据包与 hash 链

- 树：`raw/`（不可改）→ `analysis/`（脚本+中间产物）→ `report/`（json+md）→ `manifest.sha256`；
- 每个 raw 文件必须有 sha256/bytes；analysis 脚本/checker 有版本与 sha256；报告引用每个测量文件；
- 失败/无效轮次文件保留，不得删除或覆盖；
- hash 链必须可由 checker 从 raw 重算到报告数字（抽查至少 I 与 T20 逐点）。

### 9.5 T1 partial、claims 与 limits（报告字段）

- `prerequisites.t1_partial` 按 §1.3 字段记录；
- `verdict.claims` 只允许写实测支持的范围（如"4.1.2 五参与者低带 MIMO 在 9 个测点、两轮条件下 pass"）；必须写 `t6_kind=MIMO-T6` 且 T6-cal 独立 PASS；
- `verdict.limits` 必须写：T1 partial 未覆盖输出/身份检查方式、5.3.2/AoIP 未测、measurement band 限制、任何 N/A 与 guard 覆盖不足；
- `verdict.untested` 必须列未执行项（不得空白）；
- 缺失 T1 partial 声明或声学归属 → I-08。

### 9.6 诚实性规则

1. 无"0 tests PASS"、无只编译 PASS、无 prediction 冒充实测、无 S0–S3/旧 tier 代替 T6；
2. 不删点、不改门、不汇总遮盖；任何 (p,i)/band/guard **门**失败都必须在 verdict 中显式出现；§8.2.1 工程观察值（含未达目标）必须如实报告，但不写成门失败、不改变 verdict；
3. 报告数字必须能由 raw 复算；不能复算的数字 → FAIL-08；
4. 缺证据 = INCOMPLETE，不得"待补"签 PASS。

### 9.7 独立 checker 要求

- 只读 raw + 冻结 artifact/target/frozen manifest，不调用 producer 的聚合/报告函数；
- 自行实现 §4 公式（或使用独立验证过的实现），版本/hash 记录；
- 确定性：同输入同输出；无随机、无时间依赖、无网络；
- 输出：逐 (p,i) I_raw/I_use/E、T20 表、guard/保护、**predicted vs measured 幅相/T20 差**、**§3.8 身份检查**、**§5.6 跨轮预注册时间线**、**§8.2.1 工程观察值（含 `affects_verdict:false`）**、负对照结果、verdict 与理由；
- 报告与 producer 值不一致时以 checker 为准并记 FAIL-08；
- **工程观察值只报告、不参与 PASS/FAIL 判定（§8.2.1）**；不得用观察值计算 FAIL/INCOMPLETE；
- checker 本身必须通过"坏输入"测试（NC 集）与"好输入"复跑（0 误报）。

---

## 10. 人工检查表（T6-C01..T6-C60，逐条可判）

判据类型：**机** = 可由报告/raw/脚本机器判；**人** = 需人工核对记录；**D** = 文档评审现在可判；**I** = 实现后（工具/流程可用）可判；**R** = 需真实测量。状态列：本文档自身只能判 D；I/R 为未来门。

### 10.1 A. 权威、独立性与前置（T6-C01..T6-C08）

| ID | 检查项 | 通过判据 | 类型/状态 | 条款 |
|---|---|---|---|---|
| T6-C01 | 权威链/命名/版本冻结 | 报告引用 WBS/DEC hash 有效；报告 `t6_kind=MIMO-T6` 且 `t6_cal` 独立记录；本规范 version/hash 与报告一致；冲突按 §0.3 裁决 | 机+D | §0.3/§0.4/§1.1 |
| T6-C02 | 旧契约未被 T73 修改 | T73 写域 = 本 spec + `evidence/T73/`；旧契约无 T73 写入记录。若并发任务修改，`evidence/T73/CONCURRENT-CHANGES.md` 记录 before/after + 归属；T6 引用以 T73 快照为准 | 机/D | §0.4 |
| T6-C03 | 范围与非目标明确 | 4.1.2 主范围、5.3.2/AoIP 未测声明、无上混/下混、无性能门混入；**T6-cal 仍独立必须、T6-M 不替代** | 人/D | §0.2/§0.4 |
| T6-C04 | T0–T4/T6-cal/T1partial 引用可回溯 | 每个前置证据有来源路径/章节/报告 hash；**同轮 T6-cal 报告（n_points/positions/design/residual/passed）存在**；缺任一 → I-01 | 机/I | §1.2/§1.5 |
| T6-C05 | T6 独立 schema | 报告使用 `khaen-mimo-t6-report/v1`，无旧 `khaen-calibration-report/v1` 字段冒充 | 机/I | §0.4/§9.1 |
| T6-C06 | T1 partial 字段齐全 | loopback_map/covered/uncovered/cuemix_routing/final_insert/mitigation/claim 全存在（若 partial） | 机/I | §1.3 |
| T6-C07 | T1 partial 不豁免声学覆盖 | 每个参与输出有 T4 FR 或 **§3.8 T6-AID 声学身份检查**（逐输出激励+UMIK 能量/onset+slot→role→DAC 反查）证据；缺 → I-08，错配 → F-10 | 机/R | §1.3/§3.8 |
| T6-C08 | 预测/实测分离 | predicted 文件 hash 绑定；报告 I 为实测 checker 值；预测冒充实测 → FAIL-08 | 机/I | §1.4/§9.6 |

### 10.2 B. 冻结测量条件（T6-C09..T6-C24）

| ID | 检查项 | 通过判据 | 类型/状态 | 条款 |
|---|---|---|---|---|
| T6-C09 | 冻结清单存在且 hash 一致 | `khaen-mimo-t6-frozen/v1` 必填字段齐全，**FC-05/06/09/10/14/15 字段（timeline/clock/cuemix/temperature/seal/measurement_validity）逐个存在**；frozen sha256 与报告一致 | 机/I | §2.1 |
| T6-C10 | 房间/摆位/温度记录 | FC-01/FC-10 记录齐全；两轮 |ΔT| ≤2 K；无移动未记录 | 人/R | FC-01/FC-10 |
| T6-C11 | profile/布局/映射 hash | FC-02 overlay 解析 hash + 设备宽度校验 + dense_to_dac/scatter 一致 | 机/R | FC-02 |
| T6-C12 | baseline/on 仅 g 差异 | FC-03 diff_summary 只含 g；graph_id/plan hash 记录；无未声明差异；**FC-16 冻结后未改图/参数** | 机/I | FC-03/FC-16 |
| T6-C13 | 公共 D 预声明且一致 | FC-04 D0 在 off/on/两轮相同；只扣 D0；无事后平移（否则 FAIL-07） | 机/R | FC-04 |
| T6-C14 | 相位基准公共 | FC-04 phase_ref 声明并在所有文件一致；跨通道相对相位可重建 | 机/R | FC-04 |
| T6-C15 | render frame/slot 时间轴 | FC-05 + §2.1 `timeline`：每个文件 render_frame_index/slot/capture_sample_index/session/epoch/graph_id/timeline_method 齐全并被 raw sidecar hash 链覆盖 | 机/I | FC-05/§2.1 |
| T6-C16 | 时钟漂移（B2 对齐） | FC-06：报告 `clock_ppm`+`clock_ppm_uncertainty_ppm`；只有 \|ppm\|>2σ 才算漂移/允许校正；100 ppm 仅 sanity；补偿后 10 kHz 残余相位 ≤15° 且跨度/σ_ppm 达标；N=1 不得估计 | 机/R | FC-06/§3.5 |
| T6-C17 | 电平/增益冻结 | FC-07 数字电平/播放/ADC/mic/main trim 一致且记录；kMainTrim 已记录/补偿 | 机/R | FC-07 |
| T6-C18 | 中频电平不变性 | 400–2000 Hz off vs on 采集电平差 ≤0.2 dB | 机/R | FC-07/§3.5 |
| T6-C19 | 话筒/cal/朝向 | FC-08 序列号+cal sha256+90° 朝向+位置记录；未应用 cal 不得声称绝对目标 | 人/R | FC-08 |
| T6-C20 | 设备独占与 CueMix | FC-09 + §2.1 `cuemix`：唯一 owner；snapshot_text_sha256/snapshot_image_sha256/routing/final_insert_direct_routes/out7_out8_downstream 齐全 | 机+人/R | FC-09/§2.1 |
| T6-C21 | 激励与重复（FC-15） | FC-11 激励文件 hash/电平一致；**design/off ≥3、on ≥2 次有效重复**；`measurement_validity` 每文件 xrun/deadline/lost_lock/dropped/peak/SNR 齐全 | 机/R | FC-11/FC-15/§3.4 |
| T6-C22 | 保护/限幅冻结 | FC-12 limiter/保护/参考节目 hash 冻结；两态一致 | 机/R | FC-12 |
| T6-C23 | 软件/artifact 版本 | FC-13 host/daemon/checker/分析脚本版本+sha；coeff/target/tuning hash 齐全 | 机/I | FC-13 |
| T6-C24 | 冻结时点/密封（FC-14/FC-16） | frozen_at_utc < holdout 首采；seal（测点/分析/checker/artifact/predicted/cross-round）在采集前 commit；冻结后无改参数（FC-16） | 机/I | FC-14/FC-16/§2.1/§5.3 |

### 10.3 C. 测点与独立 holdout（T6-C25..T6-C36）

| ID | 检查项 | 通过判据 | 类型/状态 | 条款 |
|---|---|---|---|---|
| T6-C25 | 测点数量 | 4.1.2 design ≥6、holdout ≥3；5+3 仅 pilot（I-03） | 机/R | §3.2 |
| T6-C26 | 标签/role/weight | 所有测点 label/role/weight 必填、weight>0；role 区分 design/validation | 机/I | §3.2 |
| T6-C27 | holdout 选点先于设计 | 选点记录时间早于 design 拟合；无事后替换/剔除 | 机+人/I | §5.2 |
| T6-C28 | holdout 密封 + 跨轮预注册 | seal 含测点/分析脚本/checker/artifact/predicted hash 且早于采集；**§5.6 `cross_round_prereg` 存在且 B 预注册早于 A holdout unseal** | 机/I | §5.3/§5.6 |
| T6-C29 | 调参输入不含 holdout | design manifest `tuning_inputs_sha256` 无 holdout raw hash | 机/I | §5.3 |
| T6-C30 | holdout 未用于调参 | 无 λ/白名单/Δ/target/gain 由 holdout 决定的记录；违反 → FAIL-05 | 机+人/I | §5.1 |
| T6-C31 | holdout 独立采集/跨轮独立性 | 每轮 holdout raw 为新 session、新采集；不复用旧 raw；B 设计未使用 A holdout 结果（§5.6 声明 + 时间线） | 机+R | §5.5/§5.6/§6.1 |
| T6-C32 | 空间分离 | holdout 与 design 测点位置不同（记录/坐标或标记）；不得重复测点 | 人/R | §3.2/§5.2 |
| T6-C33 | 有效集最低数量 | holdout 有效 (p,i) ≥12/15；≥2 测点各 ≥4 有效输入；每 input ≥2 测点 | 机/R | §3.6 |
| T6-C34 | 质量掩码 + FC-15 执行 | §3.5 阈值（3 次重复/双 SNR 门/相干/时间轴/D/排除比例）实际执行；`measurement_validity` 逐文件记录 | 机/R | §3.5/FC-15 |
| T6-C35 | 近零判定用噪声底 | E_off ≤4·E_noise → N/A；报告 E_off/E_noise | 机/R | §4.4 |
| T6-C36 | N/A 比例与不可 PASS | N/A ≤20%；全 N/A → INCOMPLETE；N/A 不作 PASS | 机/I | §4.4/§8.5 |

### 10.4 D. 指标与门（含 §8.2.1 工程观察，T6-C37..T6-C50）

| ID | 检查项 | 通过判据 | 类型/状态 | 条款 |
|---|---|---|---|---|
| T6-C37 | 目标声明冻结且非循环 | target T/D0 在 holdout 前冻结；target 非由同轮基线/on 反推 | 机/I | §3.7/§4.2 |
| T6-C38 | I 公式/权重/频带 | I_raw/I_use 按 §4.3；频带 63–250、权重冻结、网格 ≥1/24 oct | 机/I | §4.3 |
| T6-C39 | 逐点改善观察 | 所有有效 holdout (p,i) 的 I_use 观察值与 `observation_met`（≥3.0 dB 为预声明工程观察目标；**不参与 PASS/FAIL**） | 机/R | §4.3/§8.2.1/O-01 |
| T6-C40 | 最差点观察 | min I_use 观察值（无汇总遮盖；**不参与 PASS/FAIL**） | 机/R | §8.2.1/O-01 |
| T6-C41 | 每输入观察 | 每个 input 的 min over p I_use 观察值（**不参与 PASS/FAIL**） | 机/R | §8.2.1/O-01 |
| T6-C42 | 跨轮复现门/独立性 | 两轮各自 PASS；|ΔI| 中位 ≤2 dB、max ≤6 dB；方向一致；N/A 一致；§5.6 预注册/时间线可证 | 机/R | §6.3/§5.6/F-06 |
| T6-C43 | T20 定义与公式 | Schroeder + [-5,-25] 拟合 + T20=3×；报告 -5/-25 时刻/R²/斜率 | 机/I | §4.5 |
| T6-C44 | T20 有效窗（V1/V2 边界） | V1= L(-25)−L_noise≥10 dB；V2=采集 ≥1.2×t(-25)+尾部达噪声底（t(-35) 可测时附加）；V3–V6；窗不够 → N/A；band 覆盖不足 → I-06 | 机/R | §4.5 |
| T6-C45 | T20 63 Hz 观察 | 中位数下降观察值与 `observation_met`（≥15% 为预声明工程观察目标）；无测点恶化 >5% 观察；**不参与 PASS/FAIL** | 机/R | §4.5/§8.2.1/O-02 |
| T6-C46 | T20 125 Hz 观察 | 同 T6-C45（**不参与 PASS/FAIL**） | 机/R | §4.5/§8.2.1/O-02 |
| T6-C47 | guard 低/高频带门 | G-1/G-2 误差退化 ≤1.5 dB；覆盖率达标 | 机/R | §4.7/F-01 |
| T6-C48 | 过渡与带外不变性 | G-3 300–400 Hz ≤2 dB；G-4 400 Hz–10 kHz ≤1.5 dB/20°；覆盖 ≥80% | 机/R | §4.7/F-01 |
| T6-C49 | 保护/限幅/增益帽 | PR-1..PR-3 通过；峰值 ≤-1 dBFS；limiter <0.1%；增益帽+2 dB 内 | 机/R | §4.8/F-02 |
| T6-C50 | 预测一致性/预测差 | over-claim：mean ≤+2 dB、max ≤+6 dB；**predicted vs measured 幅相：median |Δmag|≤2 dB、max≤6 dB、median |Δphase|≤20°；T20 差 ≤15%**；predicted hash 绑定 | 机/R | §4.9/F-03 |

### 10.5 E. 负对照与近零（T6-C51..T6-C55）

| ID | 检查项 | 通过判据 | 类型/状态 | 条款 |
|---|---|---|---|---|
| T6-C51 | NC 列表完整可执行 | NC-01..NC-10 注入方法/预期失败项/证据路径明确 | 人/D | §7.2 |
| T6-C52 | NC 证据保存 | 每项 raw/注入脚本/checker 结果 hash 齐全 | 机/I | §7.3 |
| T6-C53 | 10/10 检出 | 全部负对照被检出（含 NC-10 汇总遮盖） | 机/R | §7.3/F-04 |
| T6-C54 | 0 误报/字段 | 好配置连续 3 次复跑均 PASS；报告 `negative_controls[].false_positive/reruns` 齐全且为 false/≥3；NC-09 两分支均测 | 机/R | §7.3/F-04 |
| T6-C55 | NC 失败即 FAIL | 任一漏检/误报 → FAIL-04，不得 PASS | 机/I | §7.3/§8.3 |

### 10.6 F. 判定、报告与证据（T6-C56..T6-C60）

| ID | 检查项 | 通过判据 | 类型/状态 | 条款 |
|---|---|---|---|---|
| T6-C56 | PASS 条件完整 | P-01..P-11 全部有证据（含 T6-cal、跨轮预注册、predicted vs measured）；§8.2.1 观察项不参与；缺一 → 非 PASS | 机/I | §8.2/§1.5 |
| T6-C57 | FAIL/INCOMPLETE 码与优先级 | F-01..F-10、I-01..I-12 可判（F-05 含跨轮污染；I-08 含 T1 partial 身份；改善观察不产生 F 码）；FAIL > INCOMPLETE > NOT_RUN | 机/I | §8.3–§8.5 |
| T6-C58 | report schema 可解析 | 报告通过 `khaen-mimo-t6-report/v1` 结构校验；示例标注 example:true | 机/I | §9.1/§9.2 |
| T6-C59 | raw→analysis→report hash 链 | 每个 raw/analysis/report 有 sha256；抽查可复算 I/T20 | 机/I | §9.4/§9.6 |
| T6-C60 | claims/limits/untested 与 checker 独立 | 范围声明诚实（含 T6-cal/MIMO-T6 双证据、T1 partial）；checker 非 producer 聚合；不确定度/两轮 spread/predicted_vs_measured 已报告 | 人+机/I | §9.3/§9.5/§9.7 |

---

## 11. 追溯、manifest 与复核

### 11.1 WBS T73 DoD → 本文件章节/检查表

| WBS T73 DoD 要素 | 本规范落点 | 检查表 |
|---|---|---|
| 引用 T0–T4/T1partial 证据口径 | §1.2、§1.3 | T6-C04、C06、C07 |
| 冻结测量条件（同一电平） | FC-07、§3.5 电平不变性 | T6-C17、C18 |
| 公共 D/相位基准 | FC-04、§4.1、§4.2 | T6-C13、C14 |
| render frame/slot | FC-05 | T6-C15 |
| 独立 holdout 点 | §5（选点/密封/过拟合） | T6-C25..T6-C34 |
| T20 有效窗 | §4.5（V1–V6/聚合/观察） | T6-C43..T6-C46 |
| 近零 N/A | §4.4、§7.1 | T6-C35、T6-C36 |
| 负对照 | §7.2/§7.3 | T6-C51..T6-C55 |
| 至少两轮独立重复复现 | §6 | T6-C42、T6-C56 |
| 模态改善观察（非 PASS 门） | §4.5、§4.6、§8.2.1 | T6-C45、T6-C46 |
| 失败/INCOMPLETE 规则 | §8 | T6-C56、T6-C57 |
| **I/T20 工程观察移出 PASS/FAIL（Cursor v0.9 终审 F-19）** | §8.2.1、§8.3 判定边界 | T6-C39..C41、C45、C46 |
| 附 manifest（输入 hash/复核命令/未测项） | `evidence/T73/manifest.json`、§11.2–§11.4 | T6-C02、T6-C59 |
| 人工检查表可判 | §10（60 条） | 全部 |
| **T6 命名/关系（评审 I01）** | §0.4、§1.1、§1.2、§9.1 `t6_kind/t6_cal` | T6-C01、T6-C04、T6-C56 |
| **跨轮 holdout 预注册（I04）** | §5.6、§6 | T6-C28、T6-C31、T6-C42 |
| **声学身份检查（I05）** | §3.8 | T6-C07、T6-C20 |
| **FC-05/06/09/10/14/15 schema 闭包（I03）** | §2.1、§9.1 | T6-C09、T6-C15、T6-C16、T6-C20、T6-C21、T6-C24、T6-C34 |

### 11.2 输入 hash（只读核对；task-145 冻结刷新 2026-11-17 13:3x；T73 读取时旧值见 manifest `sha256_at_start`）

| 输入 | sha256（T73 读取时） |
|---|---|
| scratch/lead/planner/KHAEN-RT-STACK-WBS.md | `ffe5cde9e27470f0c2db9c4242383478422f553defba9370c8d268d0643204b9` |
| scratch/lead/planner/DSP-HOST-DECISION.md | `ab1056477d8e1c01dbe61df6f2d15c4bb0cca6b05f9177ef51559df3e2d25ded` |
| rt-stack/reference/SPEC.md | `f7f1a5337216138b1cffbf10f4a8652bb173f49108256d62790e0050e1b6962f`（22,843 B；task-145） |
| rt-stack/reference/HAND-CALC.md | `36ca46eb597f462a16ba0d43fb2b8d8477e8fe7c5b31e5293f00e684f0403ce0` |
| rt-stack/spec/compat-matrix.md | `1a46b4f86af9f38ebcd62a2f61a2929cdaf88e88cbdfbcc1a737f76e5ee0a1c1`（99,248 B；task-145 冻结；原 T73 快照 3c3f3adc→4ba2d005，后续 task-38 修至 1a46b4f8） |
| calibration-spec/ACCEPTANCE.md | `c83671a3e0f36ea6b945497744e916f2b7d1e4c2b53055a44c2afb8a744d6ffb`（30,222 B；task-145） |
| calibration-spec/CONTRACT.md（参考只读） | `bec362671d645c27731eec2254b5aad1e3526e788fafbca1a0e5eb12b3dd77af`（25,438 B；task-145） |
| rt-stack/reference/evidence/T03/manifest.json（T03 证据链） | `5f5fda7b77d26134370ba27d2e40de9da544b43085783cb0adda4515ed9465dc`（18,371 B；task-145） |

注：`calibration-spec/CONTRACT.md`、`ACCEPTANCE.md` 两行为 T73 读取时快照（13569/17376 B）；**task-145 冻结刷新后当前值**见上表与 manifest（`sha256_at_start` 保留 T73 读取时旧值）。T73 执行期间并发 B2 任务（task-33 spec-checker）修订了旧文件；T73 未写该目录。B2 最终观测值：ACCEPTANCE `cbca5543ef7bd36f70161d7e71bb1b266361e9a81af8acff89267a8433381698`（28744 B，含 T6-cal §5.2）、CONTRACT `1664028e34823a08500c46416941aae584ba8ac8edc2cf0dbf99e205bd02726b`（22329 B）；T05 由 task-38 修至 `4ba2d005...`。全部 before/after 与归属见 `evidence/T73/CONCURRENT-CHANGES.md`；manifest 把这些项标 `volatile` 并归因，不再以 T73 快照断言当前文件。

### 11.3 复核命令（在 `projects/khaen-audio-dsp` 下运行）

~~~powershell
cd C:\WorkSpace\Claw\projects\khaen-audio-dsp
# 1) 规范结构/ID/示例 JSON/checklist 自检
python rt-stack\evidence\T73\check_spec.py --spec rt-stack\spec\mimo-t6.md
# 2) manifest 全部输入/输出 hash 复核（含旧契约只读断言）
python rt-stack\evidence\T73\check_spec.py --manifest rt-stack\evidence\T73\manifest.json
# 3) 单文件 hash 抽查
Get-FileHash rt-stack\spec\mimo-t6.md,calibration-spec\ACCEPTANCE.md,calibration-spec\CONTRACT.md -Algorithm SHA256
~~~

### 11.4 未测项（本任务明确未做，禁止当作已验证）

1. 未实现 T6 测量/校验工具；本规范阈值未在真实 IR/房间数据上 pilot；
2. 未接板、未扫频、未 UMIK 采集、未应用系数、未做 T3/T4/T5 复测；
3. 两轮复现、holdout 密封流程、负对照 10 项、T20 有效窗判定均**未执行**；
4. T1 partial 证据引用自旧 `ACCEPTANCE.md`/评审记录，**未在板端重新核验**；
5. 新栈 host/daemon/calibration job（T60–T69）尚未实现，T6 无工具可跑；
6. 报告 schema 只做本文档内结构示例与静态自检，未用正式 JSON Schema 校验器验证；
7. 5.3.2（M=8）只给扩展边界，未给完整测点/门；AES67/Dante 端点未测；
8. 本任务不写代码、不测量、不改旧契约、不 git commit；
9. 工程观察目标/门阈值可达性（I/T20 观察目标 3 dB / 15%；guard 1.5 dB / 有效集 80%）未用 pilot 数据校准；正式测量前如需调整必须按 §11.5 升版；
10. 旧 `CONTRACT.md`/`ACCEPTANCE.md` 在 T73 执行期间被**并发 B2 任务（task-33 spec-checker）修订**；T73 未写入该目录，变化 before/after 与归属见 `evidence/T73/CONCURRENT-CHANGES.md`，不得把并发变化算作 T73 修改，也不以旧文件当前内容代替 T73 快照引用。
11. T73 评审（`scratch/lead/rt-reviews/T73-review.md`）的 I01–I12 修复为**规格/证据层修订**，未重新测量、未验证阈值可达性；详见 `evidence/T73/REVIEW-FIXES.md`。
12. Cursor v0.9 终审 F-19 只改判定口径（I/T20 观察值移出 PASS/FAIL，阈值数值不变），未新增测量/工具/板端证据；v1.2 结构与示例同步更新，见 §8.2.1 与 `evidence/T73/REVIEW-FIXES.md`。

### 11.5 版本与变更规则

- 本文件 schema 固定 `khaen-mimo-t6/v1`；阈值/公式/字段/判定状态改变（含 §8.2.1 观察目标数值变更）→ 升 `v2` 并保留 v1；
- 不得在正式测量后放宽阈值/删除点/改质量掩码；变更必须在新版中预声明并**重测全部轮次**；
- 旧契约（calibration-spec/**）的修改属其他 owner 的 B2 批次，本文件不跟踪其变更；引用以 READONLY 快照为准（T73 期间已观察到 B2 并发修订，见 `evidence/T73/CONCURRENT-CHANGES.md`）；
- 复核：`evidence/T73/manifest.json` 记录输入 hash、输出 hash、复核命令、checklist 计数与未测项；
- 未测/未实现项不得在报告中写 PASS；违反按 §9.6/§8.3 处理。

---

## 12. 变更记录

| 日期 | 版本 | 变更 |
|---|---|---|
| 2026-11-16 | v1（首版） | 独立 T6 校准实际验收规格：FC-01..16、I/T20/guard/保护/预测门、独立 holdout、两轮复现、N/A 近零与 NC-01..10、PASS/FAIL/INCOMPLETE 规则、报告 schema、60 条人工检查表、T73 manifest 与未测项 |
| 2026-11-17 | **v1.1（评审修复）** | 按 `scratch/lead/rt-reviews/T73-review.md`（NEEDS-FIX 12 条）修：I01 MIMO-T6/T6-cal 命名与双证据关系；I02 时钟/SNR/T2 对齐 B2 现行（2σ uncertainty、双 SNR 门、≥3 重复）；I03 冻结/报告 schema 补 FC-05/06/09/10/14/15 字段；I04 §5.6 跨轮预注册与防泄漏；I05 §3.8 声学输出身份 T6-AID；I06 T20 V1/V2 边界；I07 NC-09 两分支；I08 T5 窗口统一为 0；I09 identity/NC 字段/checklist 映射；I10 predicted vs measured 幅相/T20 差；I11 manifest/T05 归属与 CONCURRENT 更新；I12 scatter 语义 |
| 2026-11-17 | **v1.2（Cursor v0.9 终审 F-19）** | 按用户裁决把 **I_use ≥3.0 dB 与 T20 下降 ≥15% 移出 PASS/FAIL**：删除 P-06/P-07 与 F-01/F-02（旧 P-08 至 P-13 依次改为 P-06 至 P-11；旧 F-03 至 F-12 依次改为 F-01 至 F-10），新增 §8.2.1 预声明工程观察项（O-01/O-02，不参与 PASS/FAIL）；PASS 收窄为证据完整性 + holdout 隔离 + guard/保护门；§4.3/§4.5/§6.3/§7.2/§9.1/§9.2/§9.3/§9.6/§9.7/§10/§11 同步改观察口径；示例 JSON 增 `engineering_observations`（`affects_verdict:false`）；阈值数值未变 |
