# SPEC — KHAEN f64 核心卷积参考规范（khaen-reference/v1，P0）

2026-11-16 · rt-reference（WBS **T03** 行 / team task-23）。写域：`projects/khaen-audio-dsp/rt-stack/reference/`。
状态：**P0 规范 + 手算向量 + 夹具**，独立于 T06 Cargo workspace、未来 `rt-check`、任何 Rust 实现；不碰板端/ALSA/服务。
权威计划：[KHAEN-RT-STACK-WBS.md](../scratch/lead/planner/KHAEN-RT-STACK-WBS.md#L38)（T03 行）、
[DSP-HOST-DECISION.md](../scratch/lead/planner/DSP-HOST-DECISION.md#L26)（统一模型 §1）、
[DSP-HOST-CPU-BUDGET.md](../scratch/lead/planner/DSP-HOST-CPU-BUDGET.md#L80)（F_ji 公式 §5）。输入 hash 见 `evidence/T03/manifest.json`。

规范用词：**必须**（normative）、**应当**（recommended）、**可以**（optional）。

---

## 1. 范围与非目标

### 1.1 本规范定义什么

新自研 RT 栈中**核心线性卷积图**的 f64 参考语义，供 direct（T07）、FFT/partitioned（T08）、
fused compile（T12）、file runner（T14）、独立 oracle/dual-graph（T15/T27/T58）对拍：

1. 输入选择/零填（`A_map`）、输入侧 FIR（`A_i`）、低带 M×M 模态矩阵（`G = δ_D I + g` 嵌入）、
   输出对角 FIR（`P_post`）、容器 scatter 的数学定义；
2. 长度/支撑/尾/flush/D 计入口径，factorized 与 fused 的等价条件；
3. f64 比较规则（exact / tolerance）与逐样本 oracle 夹具格式；
4. 手算 impulse/卷积向量（见 [HAND-CALC.md](HAND-CALC.md)）。

### 1.2 本规范不定义什么

- 不定义 core API/生命周期/epoch/worker-ack（T02）、FIR 设计器七家族（T04）、REST/WS/YAML 兼容（T05）；
- 不定义测量/拟合/solver/正则/holdout（T48–T54、T60–T69）、H_raw 与 H_eff=H_raw·P 的标定侧关系（仅 §2.7 注记）；
- 不定义 ALSA/格式转换/限幅器动态/音量 ramp/保护 fallback 图（T17/T21/T22/T23）；
- 不实现上混/下混/自动造声道；stereo_strict 与多声道 profile 是**各自独立**的图（WBS §3）；
- 不定义 96k/多速率/ASRC；本规范数学与 fs 无关（`fs_hz` 只作元数据，参考实现不做重采样）。
- 本目录不是 Cargo 包，不创建 `Cargo.toml`/`Cargo.lock`（T06 integrator 独占）。

---

## 2. 模型定义（normative）

### 2.1 记号

- 离散样本下标 `n ∈ ℤ`；序列因果：`x[n] = 0`（`n < 0`）。
- 线性卷积 `(a*b)[n] = Σ_k a[k]·b[n−k]`；`δ_k` 表示第 k 个 tap 为 1 的单位冲激；
  `z^{-1}` 为 1 样本延迟。FIR 系数表 `[c_0,…,c_{L−1}]` 表示 `Σ_k c_k z^{-k}`。
- 数值域：**normative 参考为 IEEE-754 binary64（f64）**；参考的数学结果是精确实数卷积，
  f64 只负责在夹具允许的误差内表示它（§4）。不做 dB/归一化：所有增益是线性系数；
  “反相” = 系数乘 −1。dB↔线性换算属设计器（T04/T11），不在 core。
- 参考实现**不使用**隐式 dither、clip、denormal flush、DC 去除或任何自适应归一化。

### 2.2 通道与选择 `A_map`

- `N` 个源通道（`1 ≤ N ≤ 32`）；`O` 个输出角色（`1 ≤ O ≤ 22`）；`C` 个容器通道
  （`C ≥ O`，当前 MOTU 4.1.2 用 8ch 容器/7 角色，22ch 扩展同理）。
- `A_map` 是 `O×N` 矩阵，元素 ∈ {0,1}，语义**只选择/零填**：
  - 每行至多一个 1（一行不得把多个源相加 ⇒ 无混音）；
  - 每个源至多被一行选中（不得复制/分发到多角色 ⇒ 无上混/下混）；
  - 允许整行为 0（该角色无节目输入，输出必须逐样本 0）；允许源未用。
- 角色域信号：`u_j = u_{σ(j)}`（`σ(j)` 为第 j 行选中的源），无源则 `u_j ≡ 0`。

### 2.3 输入侧 FIR `A_i`（作用在列）

每个源 i 有因果 FIR `a_i`（长度 `L_Ai ≥ 1`，缺省 `δ_0 = [1]`），先于 `A_map` 作用于源：

`u_i[n] = Σ_k a_i[k]·x_i[n−k]`

覆盖输入侧整数延迟、极性、固定 gain（例如 `[0,-1]` = 1 样本延迟且反相）。
CPU 预算的通用式给出融合路由：`F_ji = p_j * G_ji * A_i`（§2.7）。

### 2.4 低带矩阵 `G = δ_D I + g`（含 D 与 offdiag）

- `D ≥ 0` 为整数**公共 baseline 延迟**（样本，预声明）。**注（P3-3）**：图内 `D` 只取整数；T04 允许的半整数/分数设计 D 由 T12 编译为 `P/A` 系数或 plan 层 `delay_q16`（T02 core-v1 v1.1 §11.10/§12.3），不在本参考的 `graph.D` 内表达；本参考只验证整数 D 图。完整矩阵为

  `G_full[j][k] = (j == k ? z^{-D} : 0) + (j,k 均属于 participants ? g[m(j)][m(k)] : 0)`

  其中 `participants` 是角色的有序子集（M 个，`M ≤ 8`；当前 4.1.2 取 FL/FR/SL/SR/SUB1 共 5 个），
  `m(·)` 为角色→dense 低带索引；`g` 为 `M×M` 因果 FIR 矩阵，行 = 输出角色、列 = 输入角色。
- 对角项 `G_jj = z^{-D} + g_jj` 含 baseline；**非对角项只有 `g_ji`**（offdiag 即模态/串扰修正）。
  非参与角色行只有 `z^{-D}` 对角项（例如 4.1.2 的 HFL/HFR），不得按角色名附加任何滤波。
- `D` 必须计入 `G` 的支撑（见 §3.1）；不得把 `G` 实现成“无延迟的 g 再额外加 D”两次。
- `g` 允许对角/非对角、任意整数/实数系数；本规范不约束其物理来源（模态 residual、设计器产物或测试值）。

### 2.5 输出对角 FIR `P_post`（作用在行）

每个输出角色 j 有因果 FIR `p_j`（长度 `L_Pj ≥ 1`）。它承担目标/分频/phase/整数延迟/极性/最终保护的**系数级**表达：

`y_j = p_j * v_j,  v_j = Σ_{k=0}^{O−1} G_full[j][k] * u_k`

- `P` 是**行对角**（每行独立 FIR），不是可交换矩阵；`F = P·G ≠ G·P`（除非所有 p_j 相同）。
- 整数输出延迟 = `p_j` 中相应 tap 的移位（如 1 样本延迟 = `[0,1]`）；极性 = 系数符号；固定 gain = 乘常数。
- 分频/target 的相位是**系数本身**；本规范不把“linear phase/min phase”解释成额外延迟，群延迟由系数决定。

### 2.6 容器 scatter

角色 j 的输出写到容器通道 `scatter[j]`（`scatter` 必须是单射，`0 ≤ scatter[j] < C`）：

`z[scatter[j]][n] = y_j[n]`，未被任何角色使用的容器通道 `z[c][n] ≡ +0.0`（逐样本严格零）。

例：4.1.2 的 `scatter = [0,1,2,3,4,5,7]`（角色 6 = SUB1 → DAC ch7；容器 ch6 恒 0）。

### 2.7 组合式与等价式

- 组合（factorized）：`y_j = p_j * (Σ_k G_full[j][k] * u_k)`。
- 融合（fused）：`F[j][k] = p_j * G_full[j][k]`（线性卷积、zero-padded），`y_j = Σ_k F[j][k] * u_k`。
- 两者在**线性卷积**意义下逐样本等价；FFT 实现必须 zero-pad 到至少该路 `L_x_eff + L_F_route − 1`，
  不得用短 FFT 做圆周卷积（陷阱见 HAND-CALC H11 / fixture C04 `traps`）。
- 行/列方向固定：`F_ji = p_j * G_ji * A_i`——`p_j` 作用输出行 j，`A_i` 作用输入列 i；**不得交换**（陷阱 C14）。
- 注记（标定侧，非本规范 runtime）：测量/设计中的 `H_eff = H_raw·P_post`，G 只做 modal residual，curve 不双叠；
  runtime 参考只计算 `P·G·A·x`，不计算 H_raw。

### 2.8 identity baseline

- `g ≡ 0` ⇒ `G_full = z^{-D} I`。
- 若再有 `a_i = δ_0`、`p_j = δ_0`，则 `z[scatter[j]] = x_{σ(j)}` 延迟 D（或 0，若该行无源）。
- `G = I`（D=0）时参考输出与输入逐样本相同；“与 frozen profile 只允许预声明 D 差异”属 E2E 验收口径
  （`docs/E2E-ACCEPTANCE.md`），手算/夹具见 C01/C02/C13。

---

## 3. 长度、支撑、尾与流式语义（normative）

### 3.1 声明支撑长度

对每个图声明（夹具中 `supports`，运行时由 profile/编译期给出）：

| 符号 | 定义 | 约束 |
|---|---|---|
| `L_A` | `max_i len(a_i)` | ≥1 |
| `L_G` | `max(D+1, 1 + max{ g 非零 tap 下标 })` | **必须 ≥ D+1**（D 计入支撑） |
| `L_P` | `max_j len(p_j)` | ≥1 |
| `L_F` | `L_A + L_G + L_P − 2` | 单路总 tap（含 D） |
| `L_out` | `L_in + L_F − 1` | 离线输出（含全部尾） |

- 某条实际路径支撑可以比声明值短；输出仍必须补零到 `L_out`。**长度由声明支撑决定**，不由“最后一个非零系数”决定。
- `L_F − 1` 即尾长；流式 flush 的零个数 = `L_F − 1`（§3.3）。
- 运行时上限（WBS 末段/§4，本参考只记录不可静默截断）：常规 profile 每路有效支撑 ≤ **4096** taps；
  **8192/16384** 为显式 extended profile，须通过 RAM/最坏 deadline/延迟/热切峰值准入；**>16384 仅设计工具离线**。
  融合长度 `L_G+L_P−1` 超上限时保留 factorized 图并重新准入，或报 INFEASIBLE/offline，不得截尾。
- 参考性群延迟（对称 linear phase，informative）：4096 tap@48k = 42.65625 ms；8192 = 85.322916… ms；16384 = 170.65625 ms。
- **口径交叉引用（P3-2）**：本规范的 `L_F` 是**完整路径长度**（含输入侧 `A_i`）；T02 core-v1 的 `FUSED_LEN_MAX=L_G+L_P−1` 与 T04 的 `L_F=L_G+L_P−1` 只是 G/P 融合部分，两者相差 `L_A−1`。下游（T08/T12/T14）报告长度/flush 时必须注明采用哪个 L_F；T12 若 `A_i≠δ` 需按本规范补 `L_A`。

### 3.2 离线（offline）语义

- 初始状态全零；输入为有限长 `L_in` 的 N 通道块；输出为长度 `L_out = L_in + L_F − 1` 的 C 通道数组。
- 每一路输出是**完整线性卷积**（含尾），不截断、不周期化、不归一化。
- 数学结果 = 精确实数；夹具按 §4 声明 expected 的表示方式（精确可表示或单次正确舍入）。

### 3.3 流式（streaming）语义与 block-size 不变性

对块大小 B（`B ≥ 1` 整数；**参考/测试候选含 64/128/256**；runtime 生产准入见 T02 core-v1 Q-11/§3.2：只 `{64,128}`，256 仅参考/设计测试口径）：

1. 在输入尾部补 `flush_zeros = L_F − 1` 个零，再补零到 B 的整数倍；
2. 按 B 样本块处理，每块输出 B 个样本（块对齐，无额外算法延迟；任何延迟必须已编码在 D/P 中）；
3. 拼接后截断到 `L_out`；
4. 结果必须与 §3.2 离线结果一致（exact 夹具逐位一致；tolerance 夹具按 §4 容差）。

即：对同一图/输入，任意 B 的结果序列相同（block-size invariance）。状态必须保持因果历史，块边界不得丢样/重算。
流式实现不得用“块内 FFT 但跨块不保留历史”的近似；partitioned/FLUSH 语义见 T08（本参考的 flush 长度即其验收基准）。

### 3.4 频谱实现要求

- 频域相乘前必须 zero-pad 到 `L_x_eff + L_F_route − 1`（等价保证线性卷积）；
- 短 FFT 圆周卷积、P/G 顺序颠倒、A 放输出侧都是**规范禁止**的实现错误，夹具以 `traps` 固化错误值供反断言。

### 3.5 精度与确定性

- 参考值定义在实数域；f64 实现只允许 §4 的误差。
- 同构建/同输入/同累加顺序应可复现；跨 x86/aarch64/NEON/FMA 用容差而非 bit 一致（DSP-HOST-DECISION §5）。
- exact 夹具的所有系数/输入使每步乘积与部分和都是 binary64 精确可表示（整数或有界二进制分式），
  因此**任何求和顺序**都应得到逐位相同结果——这使 direct/FFT/多线程 reduce 都可被强判据检查。
- 不允许输出 NaN/Inf；core 不做饱和（饱和/clip/nonfinite 统计在 T22 格式层）。

---

## 4. 比较规则（normative）

### 4.1 两种模式

| 模式 | expected 的定义 | 实现通过条件 |
|---|---|---|
| `exact` | 精确实数结果，且每个值在 binary64 中精确可表示 | 每个样本**数值相等**（+0.0 与 −0.0 视为相等）；任何 NaN/Inf 失败 |
| `tolerance` | 对解析后的 f64 输入做精确实数卷积，再**一次正确舍入**到 f64 | 每个样本 `abs(y_impl − y_ref) ≤ atol + rtol·max(1, max_abs_y_ref)`；`norm = max_abs` |

- tolerance 的 `max_abs_y_ref` 为该用例 expected 全通道最大绝对值；`rtol` 逐夹具声明（当前 C11 = 1e-12，`atol`=0）。
  依据：binary64 机器 epsilon = 2^-53 ≈ 1.11e-16；小图 MAC 数 ≤ ~10^3，理论舍入误差远小于 1e-12，留 10^4 安全余量。
- 声明的容差**不得事后放宽**；需要更松判据必须新建夹具并说明。

### 4.2 f32 参考指引（informative，不属本规范硬门）

对 f32 runtime 与 f64 参考的比较，沿用 `docs/E2E-ACCEPTANCE.md` 的工程默认（待执行计划冻结，不是已测结果）：
相对 L2 ≤ 1e-5，最大绝对误差 ≤ 1e-5·max(1, peak_ref)。f32 具体门由 T07/T15 各自任务定义并另存证据。

### 4.3 负向要求

- 必须能拒绝/失败于：未 zero-pad 的圆周混叠（C04 trap）、`G·P` 列缩放（C06 trap）、
  输入侧 A 错误放到输出侧（C14 trap）、非因果长度（`L_G < D+1`）、输出长度不按公式。
- 测试必须实际执行非零样本断言；只加载 fixture 不算通过（WBS 末段）。

---

## 5. 布局、角色与 stereo_strict（normative 索引 + informative 说明）

### 5.1 4.1.2（当前验收布局）

唯一 role→DAC 来源是板端 overlay `audio/overlay/etc/khaen-audio/layout-presets.yml`（本项目内只读副本；
运行时必须解析 + 绑定 hash，不得硬编码快照——T57 负责完整解析）：

| role index | role id | 容器/DAC | 低带 participant（dense） |
|---|---|---|---|
| 0 | FL | 0 | 0 |
| 1 | FR | 1 | 1 |
| 2 | SL | 2 | 2 |
| 3 | SR | 3 | 3 |
| 4 | HFL | 4 | —（全频，无 HP/LP/18ms 默认） |
| 5 | HFR | 5 | — |
| 6 | SUB1 | 7 | 4 |
| — | （容器 ch6 未用） | 6 | — |

⇒ 4.1.2：O=7、C=8、participants=[0,1,2,3,6]（M=5）、`scatter=[0,1,2,3,4,5,7]`。

### 5.2 stereo_strict（WBS §3、DSP-HOST-DECISION §1）

- `a_map` 只有 FL/FR 两行有源；SL/SR/HFL/HFR/SUB1 行全 0，输出必须逐样本严格 0
  （`modal_support_to_other_outputs=false`）；不得“顺手”复制 L/R。
- stereo_strict 是**独立求解/验证的受限 profile**：不能把一般 5×5 G 在 runtime 简单 mask 后仍称同一预测；
  参考只对“已生成的受限图”计算输出。
- 纯 FL/FR 节目 + 低频跨箱支援若将来启用，必须另求解/应用/holdout，不是本默认算法。

### 5.3 高度角色

默认**全频、无 HP/LP/人工 18ms**；任何滤波只来自显式配置/校准系数（“引擎不得按角色名硬编码带限”）。
参考实现不对 HFL/HFR 做任何特殊推断；4.1.2 中它们只是 G 的非参与角色（只有 `z^{-D}` 直通 + 自己的 `p_j`）。

### 5.4 其它布局

5.3.2/7.1.4 等角色表由 layout-presets 提供；本参考的图模型与布局无关，夹具可选覆盖（当前覆盖 4.1.2 与合成 2×2/5×5）。
layout hash 与 role 解析的权威实现/验收属 T57，本文件不复制其表。

---

## 6. 夹具格式 khaen-reference-fixture/v1

### 6.1 文件与绑定

```
reference/
├── fixtures/index.json          # schema khaen-reference-fixtures-index/v1 + 每个 case 的 sha256/size
├── fixtures/cases/<ID>.json     # 一个 case 一个文件（ID 与文件名一致）
├── SPEC.md / HAND-CALC.md
├── tools/gen_fixtures.py        # 确定性生成器（--write / --check）
├── tools/verify_reference.py    # 独立验证器（--report 输出 JSON 证据）
├── tools/ref_f64.py             # 独立 binary64 参考实现（offline/streaming CLI）
└── evidence/T03/                # manifest/verify-report/logs/env（本任务证据）
```

- fixture 的 `sha256/bytes` 记录在 `index.json`；验证器逐字节核对（输入 hash 绑定）。
- 机器可读手算块（HAND-CALC.md 内 `json handcalc/handcheck/handtrap` fenced blocks）是规范审阅入口。

### 6.2 顶层字段

| 字段 | 类型 | 必填 | 语义 |
|---|---|---|---|
| schema | string | 是 | 必须 = `khaen-reference-fixture/v1` |
| id | string | 是 | 唯一，与文件名（去 .json）一致 |
| title / notes | string | 是/可 | 人读说明 |
| tags | string[] | 是 | 覆盖矩阵标签（见 6.5） |
| fs_hz | int | 是 | 采样率元数据（数学与 fs 无关） |
| generated_by | string | 是 | 生成器路径（审计用） |
| graph | object | 是 | 见 6.3 |
| supports | object | 是 | `{l_a,l_g,l_p,l_f,l_out}`，必须与 §3.1 公式一致 |
| input | number[][] | 是 | N 通道、等长 `L_in` |
| expected | number[][] | 是 | C 容器通道 × `L_out`（含未用通道的严格 0） |
| expected_len | int | 是 | = `L_out` |
| comparison | object | 是 | `{"mode":"exact"}` 或 `{"mode":"tolerance","atol":…,"rtol":…,"norm":"max_abs"}` |
| streaming | object | 是 | `{block_sizes:[…],flush_zeros:L_F−1,contract:"…"}` |
| traps | object[] | 可 | 错误实现的反例值（`wrong_expected` 或 `wrong_outputs`） |

### 6.3 graph 字段

| 字段 | 语义 |
|---|---|
| n_src / n_out / container_channels | N / O / C |
| d_samples | D（整数 ≥0） |
| a_map | O×N，0/1，行/列各至多一个 1（§2.2） |
| a_fir | N 个非空 FIR（每源，缺省 `[1]`） |
| participants | 角色索引数组（有序，对应 g 的 dense 索引） |
| g | M×M FIR 矩阵；`[]` 表示零 FIR |
| p | O 个非空 FIR（每角色） |
| scatter | O 个容器索引，单射 |

### 6.4 校验规则（验证器必须执行）

1. schema 精确；未知字段拒绝；id 与文件名一致；tags 在允许集合内。
2. 尺寸/索引/类型约束（§2.2、§2.6、6.3）；所有数值有限。
3. `supports` 必须等于 §3.1 公式值：`l_g = max(D+1, 1+max_g_tap)`（**D 计入**）、
   `l_f = l_a+l_g+l_p−2`、`l_out = L_in+l_f−1`；expected 维度 = C×l_out。
4. exact 模式：expected 必须与精确有理卷积逐值相等；tolerance 模式：expected 必须 = 精确值的单次正确舍入。
5. 流式：每个声明 B 的结果与离线结果按比较模式一致；`flush_zeros = l_f−1`。
6. trap 的 wrong 值必须与线性正确结果**不同**（否则测试无效）。
7. 标签语义：**语义强制标签**（identity/offdiag/tail/D/strict/scatter/column_fir/five_by_five/dense/tolerance/streaming/zerofill/multichannel/embed/row_column/input_delay/polarity/baseline 中除描述性者外）必须被验证器断言；**描述性标签** `baseline`（identity 基线族）与 `random_exact`（合成确定性 exact 用例）只作索引，不单独断言语义（§6.5）。
8. 手算块必须能被解析，且与 fixture 的 input/expected/supports/trap 逐值一致；offdiag/tail/D 必须被手算覆盖。

### 6.5 标签与 DoD 覆盖

> 标签分两类：**语义强制**（下表全部，验证器逐条断言）与**描述性**（`baseline`、`random_exact`：只作索引/归类，不单独断言）。README/REVIEW-CHECKLIST 的“每例 tag 语义”指前者；负向校验见 `tools/verify_reference.py --negative-validation`。

| 标签 | 机器检查的语义 | 用例 |
|---|---|---|
| identity | g=0、a/p=δ，输出 = 选中输入延迟 D | C01/C02/C13 |
| offdiag | 存在非对角非零 g 项 | C03/C06/C07/C09/C10/C11/C12/C14 |
| tail | 存在 n ≥ L_in 的非零输出（真尾） | C02/C04/C05/C07/C09/C10/C12 |
| D | D > 0 且 D 在 L_G 内 | C02/C05/C07/C09/C10/C11/C12/C13 |
| strict | 存在未映射角色行，其输出严格 0 | C08 |
| scatter | 非平凡 scatter / 有未用容器通道且严格 0 | C08/C09/C13 |
| column_fir | 输入侧 FIR ≠ δ | C14 |
| five_by_five | M=5 且嵌入更宽输出集 | C09 |
| dense | 全部 M×M g cell 非零 | C12 |
| tolerance | comparison.mode=tolerance | C11 |
| streaming | 含流式 block_sizes | C10（其他用例也声明流式自检） |
| zerofill | `a_map` 有全零行，且该行/未用容器输出严格 0 | C08（C09/C13 亦满足零填） |
| multichannel | C>2 且至少 2 个非零输出 | C08/C09/C13 |
| embed | participants 数 < n_out（低带矩阵嵌入更宽输出集） | C09（C12 为 M=O，无 embed 标签） |
| row_column | 存在非标量/不同 `p_j` 与非对角 g，P 只能作用行（P·G≠G·P） | C06 |
| baseline | **描述性**：identity 基线族索引，不单独断言 | C01 |
| random_exact | **描述性**：确定性合成 exact 用例索引，不单独断言 | C12 |

---

## 7. 工具与复现命令

纯 Python 3.12+ 标准库（无 numpy/pip 依赖），在 `reference/` 下运行：

```powershell
python tools/gen_fixtures.py --check      # 磁盘 fixture 与确定性生成结果逐字节一致
python tools/verify_reference.py --report evidence/T03/verify-report.json
python tools/verify_reference.py --negative-validation --report evidence/T03/verify-negative-report.json
python tools/ref_f64.py --case fixtures/cases/C04_tail_conv_1x1.json --mode offline
python tools/ref_f64.py --case fixtures/cases/C10_streaming_128.json --mode streaming --block 64
python tools/make_evidence.py             # 生成 evidence/T03/manifest.json + 日志（输入/输出 hash、命令、exit code）
```

- `verify_reference.py` 退出码 0 = 全部检查通过；非 0 = 有失败（stderr 打 FAIL 行）。
- 当前实测（2026-11-16）：14 fixtures、108 checks、0 failures；手算块 15 个（handcalc 11 / handcheck 1 / handtrap 3），
  覆盖 11 个用例；原始输出见 `evidence/T03/verify-report.json` 与 `evidence/T03/manifest.json`。
- 工具与夹具故意保持两套精确实现：生成器用多项式乘、验证器用逐样本直接卷积，f64 参考是第三套；三者互证。

---

## 8. 未来消费者契约

| 任务 | 必须如何使用本参考 |
|---|---|
| T07 direct | 对 exact 夹具逐样本 bit 一致；对 C11 按声明容差；必须实际跑所有 case，不许 0 tests |
| T08 FFT/partitioned | zero-pad 到线性卷积长度；圆周混叠必须被 C04 trap 抓到；flush/tail 按 §3.3 |
| T12 fusion | factorized/fused 等价按 C07/C09；长度公式 §3.1；不得为省算假合并 |
| T14 file runner | 输出长度/尾/identity 保护按 §3；元数据另加但不改核心语义 |
| T15/T58/T27 | 作为独立 oracle/dual-graph 的 f64 参考之一；graph 差异白名单必须显式，不能把错误默认藏进 oracle |
| T56 资源计划 | L_A/L_G/L_P/L_F 的声明值进入 RAM/时限预算；>上限不得静默截断 |

实现方不得修改 fixture expected 来迁就实现；实现不匹配时先按 §3/§4 定位语义错误，并把证据写到各自 `evidence/<Txx>/`。

---

## 9. 版本与变更

- schema `khaen-reference-fixture/v1` 与本文档规则固定；新增夹具可以（id 唯一），
  改变语义/容差判据/字段含义必须升版本（v2）并保留 v1 夹具；
- 不得事后放宽既有容差或修改 expected；发现规范错误须记录 issue 与新旧差异，由评审确认后升版；
- 所有输入文件 hash 与生成/验证命令记录在 `evidence/T03/manifest.json`，可重放 `gen --check` + `verify` 复核。
