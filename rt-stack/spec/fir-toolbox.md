# TSPI RT-first DSP · FIR 工具箱与目标导入规范（tspi-fir-toolbox v1.2-draft）

> 状态：**草案 v1.2-draft，2026-11-17（v1.1 按独立评审 T04-review 修复 12/12；v1.2 含 Lead 裁决 errata A（fs/2 边界、HP 穿越判据、block 64/128 措辞）与 errata B（T39 crossover：fc≤0.2fs 生产硬上限/0.2–0.225 实验门、LR 0°/BW 180° 家族极性、AnalyticPhase 规范构造）；复审 PENDING；未实现/未跑代码/未板测）**（WBS T04 / 角色 fir-spec-author；任务 task-34/task-66/task-139）。
> 写域（task-34 授权）：rt-stack/spec/fir-toolbox.md + rt-stack/evidence/T04/；不建 Cargo 根、不改任何现生产代码/文档/契约。
> 消费方（future）：T10/T11/T33/T34/T35/T36/T37/T38/T39/T40/T41（设计器）、T12（融合/编译）、T13（校准适配）、T23（图节点）、T55（报告/独立 oracle）、T56（资源计划）、T60–T66（校准/daemon）。
> 规范关键词：**必须/MUST**、**应/SHOULD**、**可/MAY**、**禁止/MUST NOT**。本规范中的 schema 名（tspi-fir/v1、tspi-target-draft/v1、tspi-modal-peq/v1）在实现任务完成前**不存在**，本文只定义语义。

## 0. 定位、范围与优先级

### 0.1 本文解决什么

1. 七类 FIR 预设家族（linear、min_fast、min_slow、apodizing、brickwall、corrected/lowdelay、nos）的**参数、参考构造、通带/阻带/相位/支撑/延迟/振铃判据与拒收边界**，使 T10/T33–T38 可按同一口径实现、T55 可独立复算、T12/T23 可消费。
2. **10 段模态 PEQ（允许 IIR）**的数据模型、RBJ 系数语义（含 shelf S/Q 语义）、系数导出与“原图比对”验收、IIR 不硬截 FIR 的尾长判据，供 T41（及 T64 的 PEQ fit）实现。
3. **目标导入**（tspi-target-draft/v1：CSV + sidecar 或单 JSON）的字段、单位、相位/展开/归一化/插值/端点/拒收语义，供 T11 实现、T13 适配、T37/T55 消费。
4. **运行边界**：每路径支撑上限、融合长度、延迟预算、P/G 行列方向、zero-padding 线性卷积、精度与确定性，供 T12/T56/T23/T55 使用。
5. 相关工具（输出分频 LR/BW、phase/delay/polarity）的**最小契约**，供 T39/T40 引用；细节以其任务卡 DoD 为准。

### 0.2 非目标（本文不做）

- 不实现任何 Rust/C/Python 代码，不建 rt-stack Cargo 根（T06 integrator 独占）。
- 不修改 calibration-spec/CONTRACT.md v1/v1.1 的任何已发布字段与语义；不修改 channelmap v2 schema；v2.1 per-route FIR 仅对齐其草案。
- 不定义 REST/WS/Web、不定义 MIMO solver、不定义校准 daemon 流程（分别 T42/T24/T70、T50–T54、T60–T66）。
- 不做板端/音频/CueMix/ALSA 操作，不跑 benchmark/cyclictest/扫频，不启动/停止任何服务。
- 不复刻任何专有 DAC 的重建滤波器；NOS 只表示本软件旁路，不表示 DAC 内部重建行为。
- 不实现上混/下混/自动内容模式（WBS 已撤销）；本文的 FIR/PEQ 只做选择、校正、保护。

### 0.3 文档优先级与兼容

1. 用户最新要求 > DSP-HOST-DECISION / WBS（下文简称“主文/WBS”）> calibration-spec CONTRACT 稳定字段语义 > 本规范 > 旧设计假设。
2. 字段/系数产物**不改** CONTRACT v1/v1.1；校准文件 → 新栈只是“生产者/消费者”关系：CONTRACT 的 PEQ 字段语义（peaking q、shelf q=RBJ slope S）在本规范 §5 原样沿用。
3. 与 channelmap v2：既有 `output.roles[].fir{path,samplerate,channel}` 行为不变；v2.1 per-route FIR 草案（web/docs/API.md §10）由本规范 §2.6 给出字段来源与估算公式。
4. 若某实现任务的 DoD 与本文冲突，先停下并请 Lead 裁决；禁止实现方私自改判据放行。

### 0.4 与上游文档的对照（任务期间上游有更新的记录）

T04 开工时与收尾时读取的上游文件哈希不同（Lead 在 2026-11-16 23:32–23:36 之间有更新）：WBS 由 149 行增至 166 行、主文由 157 行增至 169 行。已逐条比对：**T04 行 DoD、写域、§3 FIR 工具箱/融合/预算边界、target 导入草案字段、P/G 行列语义均未改变**，只是 P1 门禁/校准任务编号扩展。

评审与修复（2026-11-16/17）：独立评审 rt-compat（scratch/lead/rt-reviews/T04-review.md，sha256 500af634…8185）结论 **NEEDS-FIX（4 P1 / 7 P2 / 1 P3）**；作者按 task-34 逐条修复 12/12（0 DECLINED），新增可复算脚本 rt-stack/evidence/T04/recompute-vectors.mjs 与结果 vector-results.json，逐条关闭记录见 §12.7。修复期间上游 CONTRACT/ACCEPTANCE/API/MIMO-COEFF-PLAN 亦被他人更新（哈希见 §12.2）；已复核本规范引用的关键语义（CONTRACT shelf producer/consumer 约定、API v2.1 per-route 字段草案）未变。

## 1. 通用定义与测量方法

### 1.1 采样、块与时间

- fs：运行采样率，本文默认 48000，允许 96000；所有 Hz 值以运行 fs 为准。
- 块长 B：宿主 block，**生产候选仅 64/128**（T02 core-v1 v1.1.1 §3.2 与 N8；256 只是参考/测试口径，**不是 runtime/生产候选**）；128@48k 的实际周期 2.6667 ms，64@48k 为 1.3333 ms。
- 样本↔时间：t_ms = n / fs * 1000；1 sample @48k = 0.0208333 ms，@96k = 0.0104167 ms。
- 延迟预算：用户最大附加延迟 15 / 25 / 40 ms（整链，含 block、SRC、I/O，不只 FIR D）。本文所有“超出预算”的判定必须用实际 profile 的声明值，不能用假定值。

### 1.2 响应、幅度、相位、群延迟（规范性测量程序）

冲激响应 h[n]（n=0..L−1，因果），复频响：

    H(f) = sum_{n=0}^{L-1} h[n] * exp(-j*2*pi*f*n/fs)
    mag_db(f) = 20*log10(|H(f)|)，|H| 下限截到 1e-12（−240 dB）避免 log0
    phase_deg(f) = unwrap(atan2(Im,Re)) 的连续分支；相位差比较用 wrap 到 (-180,180]
    GD(f) = -(1/(2*pi)) * d(phase_rad)/df，单位 samples；实现可用中心差分，步长 <= max(0.1 Hz, f*1e-3)

测量网格（T55 报告必须落盘网格定义与点数）：

- 幅度/相位：对数频率网格 20 Hz–min(20000, 0.499*fs)，点数 N >= max(8192, 8*L)，即细于最大可分辨纹波；端点外不测。
- 过渡带判定：在 [min(f_p,f_s)-0.2*fs … max(f_p,f_s)+0.2*fs] 上用同样的 N 点（线性可）搜索 −A_s 穿越频点；**LP 取首次（从通带侧进入阻带）穿越，HP 取最后一次穿越（衰减保持到 f_s 的最后一个离开点）**，见 §3.1 判据 5。
- 群延迟：与幅度同一网格，但仅在声明频带内报告；线性相位类用 (L-1)/2 对比。
- 极值/纹波：在声明频带内取 max/min；**禁止**只取少数倍频点后宣称通过。
- 20 Hz 与 20 kHz 端点必须单独列入 `response.endpoints`（§2.4 的 Endpoint 联合类型；若 20 kHz 不可测则写 `{status: not_applicable_at_fs, reason}`，不得省略或写成裸字符串）。

### 1.3 支撑、有效支撑与延迟

- 声明支撑 L：系数文件长度（taps），必须等于 `len(h)`。
- 有效支撑：`first = min{n : |h[n]| > eps*max|h|}`，`last = max{n : |h[n]| > eps*max|h|}`，eps=1e-6；报 `effective_taps = last-first+1`。
- 延迟 D：设计声明的体延迟（samples）。线性相位类必须 D=(L-1)/2（可为半整数，L 偶数时）；最小相位/任意相位/修正类 D 由设计声明并报告，可为 0。
- delay_ms = D/fs*1000（报 6 位小数）；“onset latency” = first/fs*1000 单列，不得把两者混写。
- 禁止：把“支撑长度 L”说成“固有群延迟”；把最小相位说成“零延迟”；把半整数 D 隐式四舍五入后不报告。

### 1.4 振铃（前/后）与排除窗

为避免不同实现口径不同，规定统一口径：

- 主峰位置 p = argmax_n |h[n]|；排除窗 W = ceil(0.000333 * fs) samples（48k=16、96k=32），即主峰两侧约 0.33 ms 不计入振铃。
- pre_ring_db = 20*log10( max_{n < p-W} |h[n]| / max|h| )；post_ring_db 同理 n > p+W。
- pre_ring_energy_db = 10*log10( sum_{n<p-W} h[n]^2 / sum_n h[n]^2 )；post_ring_energy_db 同理。
- 若 p-W < 0 或 p+W >= L，则该项写 `n/a` 并说明（支撑过短）。
- 振铃必须在**零填充线性卷积**的系数上测（不是循环卷积）。

### 1.5 幅度基准（归一化）与 0 dBFS

- 每个设计产物必须有 `reference: {hz, gain_db}`：hz=0 表示 DC，或用 h 求和精确计算；hz>0 用 §1.2 的 H(f) 计算。
- 必须满足 |measured_gain_db − gain_db| <= 0.01 dB；否则该设计判 FAIL（E_FIR_REF）。
- 本文所有 dB 是相对幅度 dB（20log10），不是 dBFS；数字满幅/余量/保护由运行器负责（WBS/RT 规范），FIR 设计只报告 max|sum|h|| 与峰值余量建议。
- 禁止：设计器在系数里偷偷加整体增益（例如为“更好看”的 0 dB 曲线）而不声明 reference。

### 1.6 精度与确定性

- 设计计算必须 f64（或更高）；冲激响应落盘默认 IEEE float32（WAV mono），也可在 JSON manifest 内存 f64 全精度数组（`coefficients.embed_f64: true`），但运行器若用 f32 必须报告量化误差复算。
- 与独立 float64 参考（T03 reference）比较的工程容差（沿用 项目 E2E-ACCEPTANCE 的工程默认，冻结前可改版）：相对 L2 <= 1e-5，最大绝对误差 <= 1e-5*max(1, peak_ref)；跨 x86/aarch64/NEON/FMA 不要求 bit 一致。
- 确定性：同一输入、同一 tool 版本，两次运行 `coefficients.file` 与 manifest 的 sha256 必须逐字节一致；系数文件内禁止时间戳/随机数/未排序 map。
- `created_unix` 只写 manifest，不进系数文件，不参与系数哈希。

### 1.7 七类家族标识

| family | 中文 | IIR/FIR | 相位类 | 主要用途 |
|---|---|---|---|---|
| linear | 线性相位 | FIR | 常数 D | 目标/分频/保护基线、可预测延迟 |
| min_fast | 最小相位快滚降 | FIR | 最小相位、频变 GD | 低延迟、近 Nyquist 锐截止 |
| min_slow | 最小相位慢滚降 | FIR | 最小相位、频变 GD | 低延迟、Nyquist 附近平缓衰减 |
| apodizing | Apodizing | FIR | 线性相位/低前振铃 | 绕开砖墙前振铃（不仿专有 DAC） |
| brickwall | 有限砖墙近似 | FIR | 常数 D | 尽量锐的过渡 + 高阻带，接受长振铃 |
| corrected | 修正/低延迟 | FIR | 显式幅相目标、可频变 GD | 复目标/房间修正/低延迟折中 |
| nos | 直通/旁路 | 直通 | 无 | 显式 bypass，保护仍在图外执行 |

## 2. 通用 FIR 系数产物与 manifest（tspi-fir/v1）

### 2.1 系数文件

- 每路径/每输出角色一个单声道 WAV：`WAVE_FORMAT_IEEE_FLOAT`（f32），采样率 = fs，长度 = taps；**不做任何归一化**（整体增益由 reference 声明并在图内保证）。
- 也允许原始 f32le（`.raw`，manifest 写 `format: "raw_f32le"`）；WAV 优先。
- 长度上限：设计 16384 taps；运行准入见 §7.2（常规 4096）。禁止暗中截断/补零改变长度后仍写原声明。
- 文件名建议 `<role>__<family>__<fs>__<taps>.wav`（不强制）；manifest 记录相对路径 + sha256。

### 2.2 manifest JSON（tspi-fir/v1）

以下字段为规范性；未知字段：`x_` 前缀允许（warning），其它未知字段拒绝（E_FIR_SCHEMA）。

    schema: "tspi-fir/v1"                     # 必填常量
    tool: {name, version}                     # 必填
    created_unix: u64                         # 可选（不参与系数哈希）
    family: linear|min_fast|min_slow|apodizing|brickwall|corrected|lowdelay|nos
    fs: u32                                   # 48000/96000
    taps: u32                                 # = len(h)，1..16384
    delay_samples: number                     # 可为半整数（仅 linear 偶数长）
    delay_ms: number                          # = delay_samples/fs*1000
    design: { ... }                           # 家族相关，见 §3.1..§3.7
    source: {target_path? target_sha256?, measurement_sha256?, profile_sha256?, calibration_sha256?}
    reference: {hz: number, gain_db: number, measured_gain_db: number}
    response: {
      passband: {lo_hz, hi_hz, ripple_db, min_db, max_db},
      stopband: {lo_hz?, hi_hz?, attenuation_db, max_db?},
      transition: {edge_hz_at_attenuation, width_hz},
      group_delay: {class: constant|frequency_varying, min_samples, max_samples, mean_samples, at_1khz_samples},
      ringing: {exclusion_samples, pre_db, post_db, pre_energy_db, post_energy_db},
      endpoints: {f20: Endpoint, f20k: Endpoint}     # Endpoint = {status:"measured", freq_hz, mag_db, phase_deg} | {status:"not_applicable_at_fs", reason}
    },
    verification: {method, grid_points, ref_impl, rel_l2, max_abs_err, passed: bool},
    coefficients: {file, format: "wav_f32_mono"|"raw_f32le", bytes, sha256, embed_f64?},
    limits: {profile: regular|extended|offline_only, route_taps_cap, ram_bytes_f32, latency_budget_ms?, fits_budget: bool},
    warnings: [string]

校验规则（必须）：

1. taps 与文件实际样本数相等；不相等 E_FIR_TAPS。
2. delay_samples 与家族规则一致（linear=D=(L-1)/2；min_*=声明值；nos=0；corrected=声明 D）E_FIR_DELAY。
3. 全部 h 有限（无 NaN/Inf），max|h| > 0（nos 除外）E_FIR_NUMERIC。
4. max|H(reference.hz)| 与 reference.gain_db 差 <= 0.01 dB E_FIR_REF。
5. response 内所有 dB 数值由 §1.2 程序重算，容差 0.05 dB / 0.1 sample（报告一致性）。
6. sha256 与系数文件一致 E_FIR_HASH。
7. 若 `fits_budget=false`，禁止运行器加载到该 profile（可作为离线设计产物保留）。

### 2.5 E_FIR_* 失败码集中表（T10/T33–T41/T55 共用）

| 码 | 条件 |
|---|---|
| E_FIR_SCHEMA | manifest schema 缺失/不等于 tspi-fir/v1，或未知非 x_ 字段 |
| E_FIR_TAPS | taps 与文件样本数不符，或越界（nos 例外） |
| E_FIR_DELAY | delay_samples 与家族规则不符或 delay_ms 不一致 |
| E_FIR_NUMERIC | h 含 NaN/Inf 或 max|h|=0（nos 例外） |
| E_FIR_REF | reference 归一化不满足 ±0.01 dB |
| E_FIR_HASH | coefficients.sha256 与实际文件不符 |
| E_FIR_BAND | 频带/参数域非法（f_p/f_s 顺序、越界、过渡 <=0）；**crossover 参数域唯一码**（fs/fc/taps/attenuation/线性相位方法奇数 taps，见本表下注；F-22） |
| E_FIR_HP_NYQ | HP 要求在 fs/2 附近通带但结构不满足（见 §3.1 判据 9） |
| E_FIR_PHASE_CLASS | linear 对称/常数 GD 不满足 |
| E_FIR_MINPHASE | min_* 未通过最小相位验证（§3.2 判据 2） |
| E_FIR_FAMILY | 家族自洽/互斥检查失败（§3.8） |
| E_FIR_EQUIV_CLAIM | corrected 冒充 linear/min 等价 |
| E_FIR_NOS | nos 参数非恒等 |
| E_FIR_BUDGET | 支撑/可行性/延迟/融合长度超预算（INFEASIBLE，不静默截断） |
| E_FIR_ENDPOINT | 端点缺失/status 非法/不可测写错 |
| E_FIR_NONCAUSAL | 目标相位在声明 D 下不可因果 |
| E_FIR_XOVER_TYPE | crossover 未知 type（LR/BW 白名单外） |
| E_FIR_XOVER_FC_LIMIT | fc > 0.2fs 且未显式 experimental（默认生产拒绝） |
| E_FIR_XOVER_FC_INFEASIBLE | fc > 0.225fs（Nyquist fade 护栏外）→ INFEASIBLE |
| E_FIR_XOVER_SUM | 合并平坦/幅度近似失败 → INFEASIBLE（BW 偶数阶按解析和注定失败） |
| E_FIR_XOVER_PHASE | 相位@fc/相位近似失败 → INFEASIBLE（BW 偶数阶不走此码：相位项只报告、标 NOT_APPLICABLE，拒收只走 E_FIR_XOVER_SUM） |
| E_FIR_XOVER_LAYOUT | crossover 输出赋值布局/拓扑非法：output_count=0；output_index 越界；同一输出重复赋值（一条输出多个 branch）；分支未显式（§4.1“无默认角色 HP”）。最小触发：`plan_outputs(&design, 8, &[OutputAssignment::lowpass(8)])` → E_FIR_XOVER_LAYOUT |
| E_FIR_APO_FP | apodizing f_p > 0.85*fs/2 |
| E_TGT_* | 目标导入失败码见 §6.9（由 T11 产生，设计器透传） |

**错误码唯一性（终审 errata C，F-22）**：crossover 参数域（fs/fc/taps/attenuation/线性相位方法奇数 taps）与其它 FIR 家族共用唯一码 `E_FIR_BAND`，不再有独立 crossover 参数域码。历史 errata B 稿曾列 `E_FIR_XOVER_BAND`，**该名已撤销（alias → `E_FIR_BAND`）**：实现（`crossover.rs`）与测试（`tests/T39`）只输出 `E_FIR_BAND`，检索到 `E_FIR_XOVER_BAND` 一律视为历史文本。`E_FIR_XOVER_LAYOUT` 为独立布局码（不属参数域），已正式登记于本表。

### 2.3 与 channelmap v2 / v2.1 的映射

既有 v2（不变）：`output.roles[].fir = {path, samplerate, channel?}`；`path` 指向 §2.1 WAV，`samplerate` 必须 = fs（不匹配则现有校验失败，本规范不放宽）。

v2.1 per-route FIR 草案（web/docs/API.md §10，未实现）由本规范提供字段来源：

    sources[].fir = { path: coefficients.file, samplerate: fs, taps: taps,
                      band_limit: [design.band_lo_hz, design.band_hi_hz],  # 设计/限带声明，非运行期滤波
                      sparse: bool }
    roles[].limiter = {threshold_db, release_ms}   # 由保护/limiter 规范定义，本规范不定义阈值
    roles[].max_gain_db
    print.estimated = { paths, taps_max, gflops_direct, gflops_fft, group_delay_ms }

估算公式（可复算，T55/T56 报告用；B = host block，K = 2B，F = B+1，Q = ceil(taps/B)，r = fs/B，P = 非零路径数，N_in/N_out = 输入/输出变换数）：

    paths        = P
    taps_max     = max(taps over paths)
    gflops_direct = 2 * sum_paths(taps_i) * fs / 1e9
    gflops_fft    = [ 8 * P * Q * F * r + (N_in + N_out) * 2.5 * K * log2(K) * r ] / 1e9
    group_delay_ms = delay_samples / fs * 1000     # 最小相位另报 gd_max；不把 D 冒充唯一延迟

注：8 FLOP/复数 MAC、2.5*K*log2(K) FLOP/实 FFT 是 CPU-BUDGET 的同一模型；N_in/N_out 按共享变换实现（25 对独立 Conv 为 2P，共享为 N+M）。

### 2.4 端点报告与 20 Hz / 20 kHz

- 每个设计产物必须报 `response.endpoints.f20` 与 `f20k`，二者都是 **Endpoint 联合类型**（字段定义见 §2.2）：可测时写 `{status:"measured", freq_hz, mag_db, phase_deg}`；20 kHz 在 fs=44.1k 时不可达（0.499*fs=22000 < 20000 可测；若 fs 更低）时写 `{status:"not_applicable_at_fs", reason}`，不得用裸字符串。
- 20 kHz 在 fs=48k 时是 0.4167*fs（可测）；“20/20k 端点不漏”检查以本字段为准，禁止用 30 Hz–16 kHz 的报告冒充 20 Hz–20 kHz。
- 校验：缺 f20/f20k 或 status 非法 → E_FIR_ENDPOINT。

## 3. 七类预设家族规范

### 3.0 公共字段与公共判据

所有家族（nos 除外）共享：`taps`、`passband_hz`（[lo,hi]）、`ripple_db`、`reference`、`window/method`；验收程序必须按 §1.2 独立复算。共同判据：finite、支撑=taps、参考归一化、20/20k 端点、确定性、预算判定。

### 3.1 linear（线性相位）

**参数（design）：** `taps`, `mode: lowpass|highpass`, `passband_hz: [lo,hi]`, `stopband_hz: [lo,hi]`, `ripple_db`（默认 0.5）, `attenuation_db`（默认 80）, `window: {type: kaiser|blackmanharris|least_squares, beta?}`, `symmetry: symmetric|antisymmetric`（默认 symmetric）。**约束（评审 I09）**：`antisymmetric` 仅允许 `mode=highpass`（Type III/IV 的 H(0)=0，LP 必须 DC 通带，故 antisymmetric LP 不可实现；出现即 E_FIR_BAND）。

**参考构造（RECOMMENDED，非唯一）：** 理想 LP/HP 幅度 + 窗函数法（Kaiser beta 按 A_s 选）或最小二乘/Remez；L 偶数或奇数都可。

**规范性判据（全部必须）：**

1. **对称性**：|h[n] − s*h[L-1-n]| <= 1e-7*max|h|，s=+1（symmetric）或 −1（antisymmetric），全部 n；antisymmetric 只允许 highpass（见参数约束）。
2. **相位**：GD(f) = (L-1)/2 samples，|GD(f) − (L-1)/2| <= 0.01 samples，f 覆盖 passband（20 Hz..f_p 或 f_p..0.499fs）。
3. **通带**：passband 内 max−min <= ripple_db + 0.05 dB。
4. **阻带**：
   - LP：在 [f_s, 0.499fs] 内 max|H|dB <= −(attenuation_db − 0.5)（保留 0.5 dB 测量余量，与旧版一致）。
   - HP：阻带为 **[0, f_s]（含 DC）**，整段阻带**纹波峰值**必须 **< −attenuation_db**（不留 0.5 dB 余量；Lead 裁决 A，2026-11-17；镜像读法：HP 通带在 f_s 之上，衰减必须从 DC 一直保持到 f_s）。
5. **过渡（Lead 裁决 A，2026-11-17 修正 HP 写法）**：tol = max(0.05*(f_s−f_p), 20 Hz)（与 §3.5 判据 4 同一口径，供测量网格/舍入容差）。
   - LP：首次达到 −attenuation_db 的频点 <= f_s + tol（衰减在带边前建立）。
   - HP：以 **[0, f_s] 为阻带、[f_p, 0.499fs] 为通带**；**最后一次离开 −attenuation_db 的穿越点必须 >= f_s − tol**（即衰减一直保持到 f_s；旧稿“HP 最后一次离开 <= f_s”与“HP 通带在 f_s 之上”自相矛盾，按裁决改为镜像读法）。
6. **参考**：|H(reference.hz)|dB 与 gain_db 差 <= 0.01 dB。
7. **延迟**：D=(L-1)/2、delay_ms=D/fs*1000 一致；L 偶数时 D 为半整数（.5），不得取整隐藏。
8. **边界**：L ∈ [8,16384]；20 < f_p < f_s <= 0.499fs（LP）或 20 < f_s < f_p <= 0.499fs（HP）；f_s−f_p >= 30 Hz。违反 E_FIR_BAND。
9. **Nyquist 特例（评审 I09 澄清）**：偶数长对称 FIR 必有 H(0.5fs)=0（推导：n 与 L−1−n 成对，L 偶数时 (−1)^n + (−1)^{L−1−n} = (−1)^n(1−1)=0）。因此：
   - 若 HP 参考点取 0.499fs 而非 0.5fs，**不要求 L 奇数**（0.499fs 处仍可满足参考）；
   - 仅当声明 passband 上界包含 0.5fs（或要求 |H(0.5fs)| 与参考差 <= 0.05 dB）时，必须用 L 奇数（Type I）或频谱反转结构；不满足 → E_FIR_HP_NYQ；
   - LP 的 0.5fs 是阻带，无需处理。
10. **预算**：latency = D/fs*1000；若 profile 预算（15/25/40 ms）减去 block/SRC 余量后不足，写 `limits.fits_budget=false` 并报 INFEASIBLE_FOR_PROFILE；**不是**设计失败，禁止截短 taps 冒充通过。

**报告额外字段：** 对称类型；GD min/max；pre/post ring（仅报告，无界）；H(0.5fs)（LP 期望 0 / HP 视长度）。

### 3.2 min_fast（最小相位快滚降）

**参数：** `taps`, `passband_hz`, `stopband_hz`, `ripple_db`（默认 0.5）, `attenuation_db`（>=60）, `prototype: {magnitude: raised_cosine|linear, taps, window: kaiser, beta}`, `factorization: cepstrum|roots`, `mag_floor_db: -120`, `cepstrum_fft: 32768`。

**参考构造（可复现，见 evidence/T04/recompute-vectors.mjs V17）：**
1. 幅度目标：|H|=1 for f<=f_p；f_p..f_s 升余弦到 floor=10^(mag_floor_db/20)；f>=f_s 取 floor。
2. 用 §3.1 的对称合成（N=32768、D=(L−1)/2、Kaiser beta=8.96、DC 归一）得到线性相位原型 h_proto。
3. 同态最小相位谱分解：对 h_proto 补零到 cepstrum_fft=32768，取 |H| 下限 mag_floor_db；实倒谱折叠 c[0], 2c[1..N/2−1], c[N/2]；exp(FFT) 后 IFFT，取前 L 点；**不做峰值归一化**（谱分解保留绝对幅度，除设计显式声明 reference）。
4. 根配对方法等价可接受（要求同样通过判据 2）。

**规范性判据：**

1. **幅度**：同 §3.1 判据 3–6（通带/阻带/过渡/参考），容差同。
2. **最小相位验证（M2，规范性；评审 I08）**：必须用**相位–Hilbert 一致性**检验：对系数 h 补零到 N=65536；在 N 个频点算 H(k) 与 unwrap 相位 φ_meas(k)；对 log|H|（下限 −120 dB）做实倒谱折叠得 φ_min(k)；在 20 Hz–0.45fs 上取 wrap(φ_meas−φ_min) 的最大绝对值，要求 **<= 2.0°**（报告 max 值与方法/网格）。
   - 可选辅助 roots 检验（设计期离线，MAY）：H(z) 全部零点 |z| <= 1−1e-6，报告最大 |z| 与越界数（根求解精度/方法写入报告；不替代 M2）。
   - **不得**把 inverse-causality 当 PASS/FAIL：有限截断 min-phase FIR 的 1/H 尾部会被截断，该指标对合法截断设计也常显著为负（本规范旧稿的 −181.7 dB 只对应其特定构造），仅作诊断（MAY）。
   - 参考构造复算（evidence/T04/recompute-vectors.mjs V17）：L=2048、f_p=16k/f_s=19k 升余弦原型 → **hilbert_consistency=0.0045°**、通带幅度差 2.3e-11 dB，通过；作为可行性示例，T55 必须用自己系数复算。
3. **延迟报告**：必须报 `group_delay` 的 min/max/mean/at_1khz 与 onset；`latency_ms` 用 GD(1 kHz)/fs*1000；另报 `gd_max_ms`。复算一致容差：±0.5 sample / ±5%。**禁止**声称“零延迟/零群延迟”。
4. **家族边界**：f_s/f_p <= 1.5 且 f_s <= 0.45fs；否则 E_FIR_FAMILY（建议 linear/brickwall）。
5. **极点/稳定性**：仅本家族不引入 IIR；FIR 本身稳定；报告是否检测到 NaN/溢出。
6. 参考构造数据（L=2048、f_p=16k/f_s=19k、beta=8.96，evidence 脚本 V17）：hilbert 一致性 0.0045°；GD = 1.5886（50 Hz）、1.5963（1 kHz）、2.7442（10 kHz）、7.5394（15 kHz）、31.3770（18 kHz）samples；仅作可行性示例，T55 必须用自己系数复算。

### 3.3 min_slow（最小相位慢滚降）

**参数：** `taps`, `passband_hz: [20, f_p]`, `nyquist_attenuation_db`（20..60，默认 30）, `ripple_db`（默认 0.5）, `prototype: {family:"smooth", transition:"cosine_to_nyquist"}`, `factorization`, `cepstrum_fft: 32768`。

**参考构造（同 §3.2 的可复现流程）：** 幅度在 [20, f_p] 内 1，从 f_p 到 fs/2 用升余弦平滑衰减到 floor（10^(mag_floor_db/20)）；对称合成（N=32768、Kaiser beta=8.96）后做同态最小相位谱分解（N=32768，floor −120 dB），取前 L 点；f_p <= 0.42*fs。复算见 evidence 脚本 V17。

**规范性判据：**

1. **通带**：20 Hz..f_p 内 ripple <= 声明值 +0.05 dB。
2. **Nyquist 带**：[0.49fs, 0.499fs] 内 max|H|dB <= −(nyquist_attenuation_db − 1)。
3. **单调性**：f > f_p 后 |H|dB 不得上翘超过 0.25 dB（允许极小纹波，但必须报 max 上翘）。
4. **最小相位验证**：同 §3.2 判据 2。
5. **延迟报告**：同 §3.2 判据 3。
6. **与 fast 区分**：若测得的 −60 dB 穿越点 <= 0.45fs，则必须改用 min_fast；min_slow 报告必须证明 M(0.45fs) > −60 dB，否则 E_FIR_FAMILY。
7. 参考构造数据（L=1024，f_p=16k，floor −120 dB、beta=8.96，evidence 脚本 V17）：hilbert 一致性 0.0036°；M(20k)=−6.02 dB、M(22k)=−16.68、M(23.5k)=−40.28；GD = 0.2336（50 Hz）、0.2346（1 kHz）、0.3864（10 kHz）、0.9288（15 kHz）、2.2037（22 kHz）samples；Nyquist 带最大 −40.99 dB。仅作可行性示例。

### 3.4 apodizing

**参数：** `taps`, `passband_hz: [20, f_p]`（f_p <= 0.85*fs/2）, `ripple_db`（默认 0.5）, `nyquist_attenuation_db`（>=30，默认 30）, `apodizing_window: {type: kaiser|hann_squared, beta?}`, `pre_ring_bound_db: -45`。

**参考构造（可复现，evidence/T04/recompute-vectors.mjs V16）：** 幅度目标 |H|=1 for f<=f_p，f_p..fs/2 升余弦 1→floor 10^(−120/20)；用对称合成（N=32768、D=(L−1)/2）后乘 Kaiser apodizing 窗（默认 beta=8.96），DC 归一；线性相位（不是砖墙），不做专有 DAC 复刻。

**规范性判据：**

1. **相位**：同 §3.1 判据 1–2（对称 + 常数 GD=(L-1)/2）。
2. **通带**：ripple <= 声明值 +0.05 dB。
3. **Nyquist 带**：[0.49fs, 0.499fs] max|H|dB <= −(nyquist_attenuation_db − 1)。
4. **前振铃**：pre_ring_db <= pre_ring_bound_db（默认 −45 dB）且 pre_ring_energy_db <= −45 dB（W=ceil(0.000333*fs)）。
5. **后振铃**：必须报告 post_ring_db / post_ring_energy_db（本家族不设上限）。
6. **过渡单调**：f_p..0.499fs 内 |H|dB 不上升超过 0.5 dB。
7. **家族边界**：f_p > 0.85*fs/2 → E_FIR_APO_FP；若测得 0.45fs 处 <= −60 dB（砖墙式）→ 必须改报 brickwall/linear；报告必须含句“仅自研可算窗/目标，不复刻任何专有 DAC”。
8. 参考构造数据（L=1024，f_p=18k，beta=8.96，evidence 脚本 V16）：pre_ring −54.64 dB、post_ring −54.64 dB、pre_energy −55.42 dB、Nyq带最大 −36.01 dB、GD 常数 511.5、通带 ripple <1e-6 dB。边界复算：f_p=20400（0.85*Nyq）→ pre_ring −49.90 dB；f_p=21600（0.90）→ −37.87 dB（故 f_p<=0.85*Nyq 边界）。阈值 −45 dB 为工程默认，依据本参考构造可达 −54.6 dB 且同长度砖墙构造约 −30.3 dB；若 T55 用自己的构造不能达到，先修订本规范，不得静默放宽。

### 3.5 brickwall（有限砖墙近似）

**参数：** `taps`, `passband_hz`, `stopband_hz`, `ripple_db`（默认 0.2）, `attenuation_db`（60..120）, `method: kaiser_windowed_sinc|least_squares|remez`。

**参考构造（可复现，evidence/T04/recompute-vectors.mjs V16）：** Kaiser 窗 sinc：fc=(f_p+f_s)/2，beta 由 A_s 选取（默认 8.96 ≈80 dB），h[n]=2fc/fs·sinc(2fc/fs·(n−D))·w_kaiser[n]、D=(L−1)/2、DC 归一；或等波纹（必须声明 method）。**过渡带必须 > 0**。

**规范性判据：**

1. **相位**：线性相位对称 + 常数 GD = (L-1)/2（同 §3.1 判据 1–2）。
2. **通带**：ripple <= 声明 +0.05 dB。
3. **阻带**：[f_s, 0.499fs] 内 max|H|dB <= −(A_s − 1)。
4. **过渡**：测得 −A_s **首次**穿越点 <= f_s + tol，tol = max(0.05*(f_s−f_p), 20 Hz)（与 §3.1 判据 5 同一口径；brickwall 为 LP 型设计，若声明 HP 型按 §3.1 判据 5 的 HP 镜像规则判定）。
5. **可行性预检**：L_req = ceil(1 + (A_s − 7.95)*fs/(14.357078*(f_s−f_p)))（Kaiser 经验式，14.357078 = 2.285*2*pi，A_s>=40 dB；评审 I11：旧稿写 14.3567 是截断常数）；若 L < L_req 且 method=kaiser_windowed_sinc，必须 E_FIR_BUDGET（拒绝）；若用其它方法，必须在报告声明 method 并仍满足判据 2–4。
6. **振铃报告**：必须报 pre/post ring 与 duration（从主峰衰减到 −80 dB 的样本数）；本家族不设振铃上限（这是与 apodizing 的取舍差异）。
7. **边界**：Δf=f_s−f_p >= 20 Hz；A_s ∈ [60,120]；taps > 16384 → offline_only。
8. 参考构造数据（L=1024，f_p=19k，f_s=20.5k，fc=19750，Kaiser beta 8.96，evidence 脚本 V16）：pre_ring −30.33 dB、post_ring −30.33 dB、pre_energy −24.61 dB；[21 kHz, 24 kHz) 内 max|H| <= −108 dB（该构造的阻带远好于 80 dB 声明，按判据 3 只需 >=A_s）。与 apodizing 的 −54.6 dB 明确区分，故 apodizing 用 −45 dB 门。

### 3.6 corrected / lowdelay（显式幅相目标 + 因果约束）

**参数：** `mode: corrected|lowdelay`, `taps`, `delay_samples: D`（整数 >=0；lowdelay 另 `delay_max_samples`）, `band_hz: [lo,hi]`, `max_mag_error_db`（默认 1.0）, `max_phase_error_deg`（默认 5.0）, `rms_error_db`（默认 0.5）, `weights: uniform|target_sha256`；目标来自 §6（必须带 phase，phase_ref=absolute 或 delay_removed）。

**参考构造：** 频域加权最小二乘/迭代（或时域 Toeplitz 因果解），在声明频带内逼近复目标；D 由设计选，不假设最小相位。

**规范性判据：**

1. **目标有效**：§6 校验通过；phase_ref=none → E_TGT_PHASE_UNSUPPORTED（本家族必须显式相位）。delay_removed 时 `target_delay_samples` **必须等于** D；若设计只能近似（如整数 D 与目标分数延迟），必须报告残差且 |D_used − D_target| <= 0.01 sample（两套规则不得混写）；absolute 时 D 仅报告。
2. **复误差**：band 内 max( |20log10|H| − 20log10|T| | ) <= max_mag_error_db；max |wrap(phase(H)−phase(T))| <= max_phase_error_deg；RMS 复幅度误差 <= rms_error_db。测量网格同 §1.2。
3. **因果/延迟**：D 整数、>=0；若目标在声明 D 下不可因果（例如目标群延迟在某频点为负且无法由 D 吸收）→ E_FIR_NONCAUSAL。
4. **群延迟报告**：class=frequency_varying；报 min/max/mean @1 kHz 与 1/3 oct 表；**禁止**把 corrected 说成线性相位/最小相位等价。
5. **lowdelay**：D <= delay_max_samples；必须报告误差-延迟折中（至少 2 个 D 的扫描结果）；若在 D_max 内达不到 max_mag_error_db/max_phase_error_deg → INFEASIBLE（报告 best achieved，不得静默放行）。
6. **等价性声明**：若输入 target 只有幅度（phase_ref=none）或设计只做幅度修正，报告必须写 `magnitude_only: true` 与 “not equivalent to measured complex matrix”；不得把最小相位设计写成原矩阵等价。
7. **带外**：band 外行为必须报告（不要求满足目标）；不得因带外误差大而裁剪 band 隐藏。
8. **预算**：taps 与 D/fs 计入 §7.2；超预算 → INFEASIBLE_FOR_PROFILE。

### 3.7 nos（直通/旁路）

**参数：** `bypass: true`；`taps` 必须=1。

**判据：** h=[1.0]（|h[0]−1| <= 1e-12）；D=0；`response` 全 0 dB/0°；manifest 写 `filtering: none`；报告写“本软件旁路，不改变/不模拟 MOTU DAC 内部重建滤波”；保护（limiter/clip/ramp）仍在图的其他节点执行，不属于本家族，但必须由图规范保证。

**拒收：** 任何 taps≠1、任何频带/窗参数 → E_FIR_NOS。

### 3.8 家族自洽与互斥检查（T55 必须执行）

| 检查 | 规则 | 失败码 |
|---|---|---|
| linear 对称/常数 GD | §3.1 判据 1–2 | E_FIR_PHASE_CLASS |
| min_* 最小相位 | §3.2 判据 2 | E_FIR_MINPHASE |
| min_fast vs min_slow | fast: f_s/f_p<=1.5 且 f_s<=0.45fs；slow: M(0.45fs)>−60 dB 且 f_p<=0.42fs | E_FIR_FAMILY |
| apodizing vs brickwall | apodizing: f_p<=0.85*Nyq、pre_ring<=−45 dB、无 0.45fs 处 −60 dB 砖墙；brickwall: 必须有非零过渡带与 ≥60 dB 阻带 | E_FIR_FAMILY |
| corrected 不等于 linear/min | 必须带显式相位目标且误差达标 | E_FIR_EQUIV_CLAIM |
| nos 恒等 | h=[1] | E_FIR_NOS |
| 预算 | taps/延迟/内存/融合长度 | E_FIR_BUDGET / INFEASIBLE_FOR_PROFILE |
| crossover（相邻工具，非七家族） | §4.1：家族相对极性/AnalyticPhase 规范构造/fc 档位；BW 偶数阶 0.5 dB 平坦判据必须 INFEASIBLE，且偶数阶相位项只报告（NOT_APPLICABLE，拒收只走 SUM）；参数域错误统一 E_FIR_BAND；输出赋值布局错误 E_FIR_XOVER_LAYOUT | E_FIR_XOVER_TYPE/FC_LIMIT/FC_INFEASIBLE/SUM/PHASE/LAYOUT、E_FIR_BAND / INFEASIBLE |

## 4. 相邻工具最小契约（crossover / phase-delay-polarity，供 T39/T40）

### 4.1 输出 FIR 分频（T39；v1.2-draft errata B，RULED 2026-11-17）

- 类型：LR2/LR4/LR6/LR8、BW1..BW8（阶数指总阶；LRn = 两级 Butterworth n/2 级联，幅度平方）。允许 future 增加但必须显式拒绝未知 type（E_FIR_XOVER_TYPE）。
- **无默认角色 HP/LP**：高度默认全频（无 HP/LP/人工 18 ms），分频必须显式配置来自 profile/校准，不得按 role 名注入；未赋值输出是 bypass/full-range，不是 HP。输出赋值布局/拓扑非法（output_count=0、output_index 越界、同一输出重复赋值）→ `E_FIR_XOVER_LAYOUT`（§2.5 已登记；与参数域码 `E_FIR_BAND` 分离）。
- **规范构造（RULED：AnalyticPhase）**：解析复频响 H(j·f/fc) × 公共体延迟 e^{−j2πfD/fs}（D=(taps−1)/2）× Nyquist 护栏 fade（[0,0.45fs] 幅度=1，[0.45fs,0.5fs] 升余弦→0）→ IFFT → 取 n=0..taps−1 矩形截断。两支必须同 L、同方法、同 D，相对 D 必须为整数 samples 并报告（当前实现恒 0）；报告幅度/相位近似误差、群延迟、前/后振铃、有效支撑。**LinearPhaseMagnitude 仅非规范对照**（用于量化旧 §4.1 字面构造与家族判据的矛盾），不得用于 §4.1 验收；线性相位只约束支路 FIR 自身设计，不约束两支求和（旧稿“按线性相位设计”与“BW 相位非线性”的矛盾由本裁决关闭）。
- **家族相对极性（RULED）**：LR 全阶——应用家族极性后 fc 相对相位 0°（同相和）且 |S| 平坦；LR2/LR6 解析相差 180°，令 HP 反相后回 0°；LR4/LR8 同相。BW 奇数阶（1/3/5/7）——允许/要求一支反相（家族极性关系 180°），fc 相对相位按解析目标（±90°），**不强制 0°、不要求线性相位**；解析和 (1∓s^n)/D 对奇数 n 仍全通，故 |S| 平坦。BW 偶数阶——解析和本身不平坦（fc 处 +3.0103 dB；带内相对 max 偏差约 1.3–3.0 dB，解析可证），§4.1 的 0.5 dB 平坦判据**必须判 INFEASIBLE**（不得放宽/静默接受/冒充已支持）；如需支持须另立裁决与判据。**BW 偶数阶相位项只报告**：`xover_family_polarity` 与 `xover_phase_at_fc` 标 NOT_APPLICABLE（不计 pass、不得以相位门放行或拒收），拒收只走 ⑤ 合并平坦判据 → `E_FIR_XOVER_SUM`（终审 errata C，F-24；奇数阶行为不变）。
- **BW 偶数阶 phase 项语义（终审 errata C，F-24；Lead 收口 2026-11-17）**：`xover_family_polarity` 与 `xover_phase_at_fc` = **NOT_APPLICABLE**；实现以 `pass=false` + `limit=None` + statement 含 `NOT_APPLICABLE` 表达（不引入 NOT_APPLICABLE 状态字段、不改公共 API）。消费者须按“不适用/不计 pass”解释，**不得**读成“已实现但失败”；这两项不得作为拒收依据——拒收只走 ⑤ 合并平坦判据 → `E_FIR_XOVER_SUM`。
- **判据（T14/T15/T55 共用；全部落盘实测值）**：① type 白名单，未知拒绝；② fc 档位见下；③ 两支同 L、同方法、同 D，相对 D 整数 samples；④ 相位@fc：LR（家族极性后）|Δφ| <= 0.5°，BW 奇数阶相对解析目标 ±5°，BW 偶数阶不定义相位验收（相位项标 NOT_APPLICABLE、只报告、不计 pass，拒收只走 ⑤ → E_FIR_XOVER_SUM）；⑤ 合并 S=H_LP+H_HP 在 [fc/2, 2fc] 内 |S|dB 相对 max 偏差 <= 0.5 dB（仅 LR 全阶 + BW 奇数阶）；⑥ 幅度近似带内 <= 0.5 dB、AnalyticPhase 相位近似 <= 5°（实现报告门；LinearPhaseMagnitude 只报告）；⑦ finite；⑧ 极性来源必须 polarity_source = analytic_filter_relation，并测相反极性作诊断，**禁止靠听感/role 名判极性**；⑨ 失败必须在报告 criteria/warnings 与证据中保留原始测量值并报 INFEASIBLE，不静默 PASS。
- **fc 档位（RULED 2026-11-17）**：spec 参数域 fc ∈ [20, 0.45fs] **仅用于 E_FIR_BAND 消息**，不再等于可设计范围。可设计范围：**生产硬上限 fc <= 0.2fs（含 0.2fs）**；0.2fs < fc <= 0.225fs 仅显式 `experimental` flag + warning 可设计，**非生产且不得 accepted**（判据 xover_production_cap 失败）；fc > 0.225fs → INFEASIBLE（E_FIR_XOVER_FC_INFEASIBLE）；默认 experimental=false 时 fc > 0.2fs 直接 E_FIR_XOVER_FC_LIMIT 拒绝。理由：Nyquist fade 自 0.45fs 起，生产带的上界 min(2fc, 0.499fs) <= 0.4fs（全在 fade 外），实验带上界 <= 0.45fs（触界）；> 0.225fs 带内进入 fade 不可验收。96k 按 fs 比例同规则。
- **证据互引（T39，RULED 2026-11-17）**：实现 `crates/designer/src/crossover.rs`（模块文档“Lead 裁决/RULED”、常量 `FC_PRODUCTION_MAX_FRACTION=0.2`、`FC_EXPERIMENTAL_MAX_FRACTION=0.225`）；测试 `crates/designer/tests/T39/crossover_cases.rs`（RULED 档位/家族极性/AnalyticPhase 自洽用例）；运行证据 `rt-stack/evidence/T39/run-tests.log`（15/15 passed）、`metrics.log`（LR4 sum dev 0.0288 dB；BW6 flat_dev 2.8710 dB accepted=false 等）、`evidence/T39/MANIFEST.md` §5–§7；外部评审 `scratch/lead/rt-reviews/FIR-DESIGNER-review.md`（I01“fc>0.2fs 未落地”、I02“§4.1 构造冲突”，由本 errata + T39 修复关闭）。**证据同步提示（不阻塞本 errata）**：T39 MANIFEST §2 记录的源码/测试哈希为修复前（80,146/41,027 B），当前实测已更新（89,661/50,719 B，2026-11-17 13:0x）；`evidence/T39/RULED-T39-2026-11-17.md` 在代码中被引用但尚未落盘 → 由 T39 owner 补齐；T04 以代码/测试/日志为准，不依赖该缺失文件。
### 4.2 phase / delay / polarity（T40）

- 整数延迟：h[n]=delta[n−D]，|D| 整数、>=0；报告 D、delay_ms；解析向量见 §9 V10。
- 分数延迟：窗函数 sinc（Kaiser/Hann）或 Lagrange，必须声明 `fractional_delay_d` 与总 D；判据：在 [20 Hz, 0.9*fs/2] 内 |H|dB 与 0 dB 差 <= 0.1 dB、GD 与声明 D 差 <= 0.02 samples；taps >= 63（作者参考：63-tap Kaiser beta 8.96、d=0.5 时 |H| 偏差 <=0.01 dB（DC −0.00954 dB、20 Hz–20 kHz 最大 0.00977 dB）；**总 GD = 31.5000 ± 0.0001 samples，即 GD−31 = 0.500 ± 0.001**（20 Hz–10 kHz；原稿写 GD−31.5=0.500 差 0.5 sample，已按评审 I01 修正）；构造与复算见 §9 V11 与 evidence/T04/recompute-vectors.mjs）。
- 极性：invert = 系数整体乘 −1（或运行期符号），两者等价；报告 polarity 字段，不得与校准 invert 双乘（校准契约已定义 XOR，见 CONTRACT §3.2）。
- 方向：**输入侧 delay/polarity 作用于列（源 A/G 的输入）**，**输出侧作用于行（P 的输出）**；矩阵非对称，两者不可交换；报告必须写清作用侧。
- 因果：任何负的/提前的延迟必须由 target_delay_samples/D 吸收，否则 E_FIR_NONCAUSAL。

## 5. 10 段模态 PEQ（允许 IIR）规范

### 5.1 作用与链位置

- 模态 PEQ 是 **P_post 的每输出对角校正**（约 10 段），允许 IIR biquad；用于低带模态削峰/宽带对齐。
- 规范链序：输入选择 A_map → G（低带矩阵 FIR）→ P_post（crossover HP/LP → 模态 PEQ → 输出目标 FIR → gain → delay → polarity）→ 保护/limiter → scatter。LTI 节点数学上可交换，但**规范序**用于定义相位/延迟报告、哈希与实现确定性；重排必须证明等价的容差（f64 设计下 <= 0.01 dB/0.1°），否则禁止。
- P/curve 冻结后 G 只承担 modal residual；**禁止**同一峰谷被 baseline PEQ 与 G 各切一次（不双重 tone），见主文 §2.5；curve_id 只做绑定。

### 5.2 数据模型与界限

每输出角色（name/index 与 overlay/layout 解析结果一致）：

    modal_peq: {
      channels: [{
        name: "FL", index: 0,
        bands: [{type: peaking|low_shelf|high_shelf, freq, q, gain_db, b:[3], a:[1,a1,a2]}...]
      }]
    }

- 模态 peaking 段：**每通道 <= 10 段**（本规范“10 段模态 PEQ”的定义）；shelf 可选、**生产端（T41/T64）**约定每通道 low_shelf/high_shelf 各 <= 1 段（不计入 10）；每通道 biquad 总数 <= 32（CONTRACT 硬上限）。
  **生产/消费语义区分（评审 I06）**：每通道 shelf 各 <= 1 是 producer 输出约定；**消费方（T13 读 calibration v1.1、T23 载入）不得假设最多一段**，必须接受任意已知 type 的若干段（总数 <= 32），按 CONTRACT v1.1 §10 追加。
- 字段语义与 CONTRACT v1.1 §3.4/§10 完全一致：peaking 的 q 是 RBJ Q（有限且 >= 0.1）；shelf 的 q 是 RBJ slope **S**（[0.1, 1.0]，不是 Q）；type 缺省 peaking；未知 type 整份拒绝 E_PEQ_TYPE（不降级）。
- freq ∈ [20, min(20000, fs/2−1)] Hz；单段 gain ∈ [−24,+24] dB；trim 另按 CONTRACT。
- **模态段恒只切（评审 I05，对齐 peq-optimizer 需求 5 AC4）**：modal peaking gain **恒 <= 0 dB**，**不允许** `allow_boost` 作用于 modal 段；T64 生产者不得对 modal 用 allow_boost。
- **宽带/非模态正增益**（peq-optimizer 需求 6 AC4）：默认上限 0 dB；显式 `--boost-cap-db` 打开后总量 <= +3 dB（>+3 按 +3 执行并 warning）；非模态总量另受 <= 12 dB 与 >6 dB warning 限制；不得用于填谷。
- 非目标：不把 IIR 强截成 FIR（见 §5.7）；不负责分频（§4.1）；不改变 role gain/delay/polarity。

### 5.3 RBJ 系数公式（规范，A=10^(g/40)，w0=2*pi*f0/fs）

Peaking：alpha = sin(w0)/(2Q)；

    b0 = 1 + alpha*A;      b1 = -2*cos(w0);      b2 = 1 - alpha*A
    a0 = 1 + alpha/A;      a1 = -2*cos(w0);      a2 = 1 - alpha/A      # 导出时全部除以 a0

Low shelf（S 是 slope）：alpha = sin(w0)/2 * sqrt((A + 1/A)*(1/S - 1) + 2)；

    b0 =    A*((A+1) - (A-1)*cos(w0) + 2*sqrt(A)*alpha)
    b1 =  2*A*((A-1) - (A+1)*cos(w0))
    b2 =    A*((A+1) - (A-1)*cos(w0) - 2*sqrt(A)*alpha)
    a0 =      (A+1) + (A-1)*cos(w0) + 2*sqrt(A)*alpha
    a1 =   -2*((A-1) + (A+1)*cos(w0))
    a2 =      (A+1) + (A-1)*cos(w0) - 2*sqrt(A)*alpha

High shelf：

    b0 =    A*((A+1) + (A-1)*cos(w0) + 2*sqrt(A)*alpha)
    b1 = -2*A*((A-1) + (A+1)*cos(w0))
    b2 =    A*((A+1) + (A-1)*cos(w0) - 2*sqrt(A)*alpha)
    a0 =      (A+1) - (A-1)*cos(w0) + 2*sqrt(A)*alpha
    a1 =   2*((A-1) - (A+1)*cos(w0))
    a2 =      (A+1) - (A-1)*cos(w0) - 2*sqrt(A)*alpha

可判不变量（T41 单测必须覆盖）：

1. Peaking 在 f0 处 |H(f0)|dB = gain_db（精确，解析）；gain_db=0 时 b=a（恒等）。
2. Shelf 在 f0 处 |H(f0)|dB = gain_db/2；low_shelf DC=gain_db、Nyquist=0；high_shelf DC=0、Nyquist=gain_db。
3. S=1 时 alpha = sin(w0)/2 * sqrt(2)（即 alpha/sin(w0)=0.70710678）。
4. Peaking 半增益带宽（|H|dB=gain_db/2 的两个频率 f1,f2，f1<f0<f2）满足 f0/(f2−f1) ≈ Q；仅当 f0 <= fs/16 且 |gain_db|<=12 时作为**信息性**检查（容差 ±2%）：解析二分求根 f0=40,g=+12,Q=8 → 8.00004（f1=37.5781、f2=42.5780）；f0=1k,g=+6,Q=1 → 1.00428（f1=618.5798、f2=1614.3144；双线性翘曲导致 +0.43%，故不用于高频规范性判定）。方法：对解析 |H|dB 二分到 1e-4 Hz；复算脚本 evidence/T04/recompute-vectors.mjs V7。

### 5.4 数值/精度

- 设计 f64；系数以 a0=1 归一；导出 JSON 用 f64（十进制至少 17 位有效数字）。
- 运行期允许 f32 系数/状态，但必须复算 f32 量化后的响应：**固定评估网格** 20 Hz–20 kHz、对数 2001 点 f_k=20*1000^(k/2000)（k=0..2000；约 1/200 oct，细于 1/48 oct）上，与 f64 解析响应差 <= 0.1 dB；否则 `requires_f64: true`，运行器不得用 f32 加载（或必须用双精度路径）。作者实测（同一网格）：30 Hz/−12 dB/Q=20 极半径 0.999804，f32 化后最大偏差 0.1965 dB → 必须 requires_f64；40 Hz/−10/Q=16 → 0.0397 dB；25 Hz/−8/Q=12 → 0.1281 dB。网格定义写入 V15 与 evidence 脚本。
- 极点半径：|p| <= 0.999999（越界 E_PEQ_UNSTABLE）；报告 max|p|、max|a1|。
- 状态变量 denormal 处理由运行器负责（RT 规范），设计报告只要求稳定与有限。

### 5.5 导出 schema（tspi-modal-peq/v1）

    schema: "tspi-modal-peq/v1"
    tool: {name, version}
    created_unix: u64
    fs: u32
    source: {measurement_sha256?, curve_id?, target_sha256?, calibration_sha256?}
    channels: [{
      name: "FL", index: 0,
      bands: [{type, freq, q, gain_db, b:[f64,f64,f64], a:[1.0,f64,f64]}],
      predicted: {dc_gain_db, nyquist_gain_db, poles_radius_max, requires_f64: bool}
    }],
    design: {mode: modal_only|modal_plus_wide, allow_boost: bool, band_hz, fit_report_sha256?}
    # allow_boost 仅对 wideband/非模态段有效（<= +3 dB）；对 modal peaking 段无效，modal 恒 <= 0 dB
    warnings: [string]

### 5.6 “原图比对”验收（T41/T64/T55 共用口径）

1. **自洽**：由 band 字段（type/freq/q/gain）解析计算的级联响应，与由导出 b/a 系数计算的响应，在 20 Hz–20 kHz、1/48 oct 网格上差 <= 0.1 dB / 1°。
2. **原图比对（分两级，不得混写）**：与拟合前的原始曲线（measurement/target 的幅度）在同一网格比较：报告每段 f0、peak/dip 修正量、模态带残余。
   - 2a **拟合阶段单点规则**（peq-optimizer 需求 5 AC7）：某单点在模态带内峰值高出目标 > +3 dB，或该点相对修正前变差 > 3 dB → 降增益/跳段。
   - 2b **多测点验收规则**（peq-optimizer 需求 8 AC4）：各测点相对修正前的偏差**恶化不超过 1 dB**（不是 3 dB；原稿混写已在评审 I02 修正）；另 1/3 oct 相对目标 ripple <= 3 dB、模态带内峰 <= 目标 +3 dB、不得新增 > 6 dB 窄带缺口。
   - 2c 两个数字来源不同：3 dB 只用于 2a 的单点拟合判据，1 dB 才是多测点交付验收；T41/T64/T55 报告必须分别列出 2a/2b 结果。
3. **只切**：默认 `allow_boost=false` 时不得出现正增益 peaking；深谷不得填。
4. **合成夹具**（T41 必须）：8 个已知模态 + 倾斜 + 噪声，至少两个 Q>10：检出频率 ±3 Hz、Q ±30%、模态增益 <=0；Q 不被夹到 10；`q_unresolved` 在网格不可分辨时正确。
5. **与原曲线对比不得只在 1/3 oct 平滑后看**：必须另报 1/24 oct 原始分辨率最大偏差。

### 5.7 IIR 不硬截 FIR（尾长判据）

默认导出 IIR，不转 FIR。若某运行 profile 只接受 FIR 而要求把 IIR 转 FIR，必须满足：

- 每个 biquad 的冲激响应数值计算到候选长度 L；级联尾部满足：tail_peak <= −100 dB re 总峰值，tail_energy <= −80 dB re 总能量；
- 如果任一 biquad 的极点半径 r 导致在 L 内不满足，必须拒绝 E_PEQ_FIR_TAIL 并给出所需长度估计，禁止静默截断；
- 长度估计（保守、上限参考）：L ≈ ceil(log(1e-5)/log(r))；例 r=0.999 → 11508 samples（239.75 ms @48k），r=0.9995 → 23021 samples（479.60 ms），远超常规 4096 taps → 结论：此类模态段必须保留 IIR，不能塞进 4096 常规 FIR 预算。
- 截断后必须重算 §5.6 的自洽/原图比对；不得以“听起来没问题”放行。

### 5.8 与 calibration v1.1 的关系

CONTRACT v1/v1.1 字段语义不变；v1.1 文件经 T13 adapter 转成内部 IR（保留 type/freq/q/gain/trim/delay/polarity；未知 type 拒绝）。本规范的 modal PEQ 是新栈 P_post 的直接产物，不经 CamillaDSP；不得双写/双应用同一 PEQ。

## 6. 目标导入（tspi-target-draft/v1）

### 6.1 边界与区分

- 目标是**期望**的复/幅度响应（设计输入）；测量 CSV 是实测（去粗延迟后的相位，不能直接当 target 相位）。**禁止**把测量 CSV 的去粗延迟公式套到 target 上；phase 语义见 §6.6。
- 内部 version adapter：本 schema 不写进 CONTRACT；T11 实现读入，T37/T55 消费。单位不明/未知语义必须拒收，不猜 Hz/kHz、dB/linear、rad/deg。

### 6.2 文件形式

形式 A（CSV + sidecar）：

    target.csv              # 表头必须精确：freq_hz,mag_db  或  freq_hz,mag_db,phase_deg
    target.csv.json         # sidecar：同目录、同名 + ".json"（也接受显式路径）

CSV 规则：逗号分隔、无注释行（# 行拒绝）、允许指数记法、空白 trim；>= 2 个点；<= 20000 个点；字段数必须与表头一致。

形式 B（单 JSON）：一个对象含全部元数据 + `points: [{freq_hz, mag_db, phase_deg?}]`；字段同 §6.3；不允许 CSV + JSON points 混用。

### 6.3 sidecar / 单 JSON 字段（规范性）

    schema: "tspi-target-draft/v1"            # 必填
    frequency_unit: "Hz"                     # 必填且只允许 Hz
    magnitude_unit: "dB"                     # 必填且只允许 dB
    phase_unit: "deg"                        # 有 phase 时必填；无 phase 时必须缺省/拒绝
    runtime_rate: 48000                        # 必填，必须 == 目标 profile fs，否则 E_TGT_RATE
    normalization: none|ref_1k|peak           # 必填，见 §6.5
    reference_gain_db: number                 # 可选，默认 0.0，[-24,+24]
    phase_ref: none|absolute|delay_removed    # 有 phase 时必填；见 §6.6
    unwrap_policy: unwrapped|auto             # 有 phase 时必填；见 §6.6
    target_delay_samples: number              # phase_ref=delay_removed 时必填，整数 0..200*fs/1000；否则必须缺省
    interpolation: "log_frequency_linear"     # 必填且只允许该值
    edge_policy: hold|reject                  # 必填，见 §6.7
    source_hash: sha256_hex                   # CSV 形式必填（= CSV 文件字节 sha256）；JSON 形式可选（= points 规范序列化 sha256，见下）
    source_file_sha256: sha256_hex            # 可选；原始文件字节哈希，跨 Rust/JS 实现追溯用
    note: string                              # 可选，<=500 字

JSON 形式：`source_hash` 若给出，必须是 points 数组的**规范序列化** sha256，算法固定为：

1. 按数组顺序；每个点只序列化 freq_hz/mag_db/phase_deg 三个键（无 phase 时省略 phase 键，不写 null）；
2. 键顺序固定为 freq_hz, mag_db, phase_deg；无空格/无换行；UTF-8；
3. 数字用最短往返十进制（round-trip shortest）：Rust `serde_json`/`ryu` 与 ECMAScript `Number::toString` 对 finite double 的输出一致；整数不带 `.0`（除非该字段被声明为浮点且值需区分时以原始 token 为准）；
4. 输出为 `[{...},{...}]`，末尾无换行；sha256 取其 UTF-8 字节；
5. 实现语言无法保证最短往返时，必须另写 `source_file_sha256`（原文件字节哈希）并在报告声明 `source_hash_algo: none`；不得伪造规范哈希。

未知字段：顶层 `x_` 前缀 warning、其它未知字段 E_TGT_FIELD_UNKNOWN；points 内元素只允许 freq_hz/mag_db/phase_deg（`x_` 前缀 warning，其它 E_TGT_POINT_FIELD）；sidecar 携带 `points`、或 CSV 与 JSON 同时给 points/表 → E_TGT_SIDECAR_MIX。

必填字段缺失（schema/frequency_unit/magnitude_unit/runtime_rate/normalization/interpolation/edge_policy；有 phase 时 phase_unit/phase_ref/unwrap_policy；delay_removed 时 target_delay_samples；CSV 形式 source_hash）→ E_TGT_MISSING_FIELD。

### 6.4 频点数值约束（全部必须，任一违反即拒收）

1. freq 有限、> 0、严格递增、无重复；**freq <= fs/2**。但 **带 phase 的目标点必须 < fs/2**（等于 fs/2 拒收：Nyquist 相位退化，码 E_TGT_PHASE_REAL）；magnitude-only 目标允许 freq = fs/2（Lead 裁决 2026-11-17，见 evidence/T04/RULING-fs2-2026-11-17.md）。
2. mag_db 有限；phase 有限；NaN/Inf → E_TGT_NAN。
3. 每点必须有幅度；phase 列必须全有或全无（混合缺失 E_TGT_PHASE_MIXED）。
4. 点数 >= 2；<= 20000。
5. 网格分辨率：报告最小/最大频点间隔与相邻 octave 间隔；建议 20–200 Hz 不低于 1/48 oct、1–20 kHz 不低于 1/12 oct；不满足 → warning（不拒收）。

### 6.5 normalization 与 reference_gain_db

- none：按给定 mag_db 使用（producer 已锚定）；不额外平移。
- ref_1k：整体平移使 1 kHz 处（按插值）为 0 dB。
- peak：整体平移使 max(mag) = 0 dB。
- reference_gain_db 在 normalization **之后**整体相加，不改变形状；报告必须分别记录 `normalization`、`reference_gain_db`、`applied_offset_db`。
- 提升检查：`max_boost_db = max(mag_norm + ref) − (mag_norm(ref_1k)+ref)`（与 ref 无关）；>6 dB warning；>12 dB 且未显式 allow_boost → E_TGT_BOOST；硬上限 +18 dB。禁止静默抬低频。

### 6.6 相位语义（与测量去延迟公式严格区分）

符号约定：H(f)=|H|exp(j*phi_rad)，phase_deg = phi_rad*180/pi；与 §1.2 一致。

- `phase_ref: none`：只有幅度目标；设计方必须显式声明选用的相位家族（linear/min_fast/...），**不得**把缺 phase 当零相位；报告写 `phase_target: none` + `phase_family_used`。
- `phase_ref: absolute`：给出的是期望的**总相位**（已数圈展开，deg）。设计器必须用总相位（含 D 造成的 −360*f*D/fs）逼近；D 由设计报告；不允许再额外隐式叠加/加大延迟（原稿“加速延迟”为笔误，已按评审 I11 修正）。
- `phase_ref: delay_removed`：给出的是**去粗延迟 φ_ex**；目标总相位 φ_tot(f) = φ_ex(f) − 360*f*D_target/fs，D_target=target_delay_samples（必填，整数）。设计必须用 φ_tot；报告 D_used 与残差（|D_used − D_target| <= 0.01 sample 记录）。
- `unwrap_policy: unwrapped`：要求相邻点 |Δφ| <= 180°，否则 E_TGT_PHASE_JUMP；`auto`：按最小跳变 +/−360*k 展开并报告 wrap 次数；两种都必须报告 max|Δφ|。
- **带 phase 的目标点禁止 freq = fs/2**（Lead 裁决 2026-11-17：Nyquist 处相位退化，直接拒收 E_TGT_PHASE_REAL；magnitude-only 不受此限）。**E_TGT_PHASE_REAL 责任层拆分（终审 errata C，F-23）**：(a) freq = fs/2 子条件由静态导入层 T11 判定（`targets.rs` stage 19；测试 `tests/T11/target_import_cases.rs::t11_targets_phase_fs2_ruling`）；(b) “其余频点相位无法由实系数因果 FIR 表示/自相矛盾”**静态导入层不可判**（任一有限点集在 f<fs/2 的任意相位可由足够长度实系数 FIR 表示；可判性依赖 taps/D/预算，属设计期），由设计层 T37 corrected（E_FIR_NONCAUSAL / 相位误差判据 INCOMPLETE）与报告层 T55（`corrected_phase_target`/`corrected_complex_error` 判据，联合 E_TGT_BUDGET）承担；测试见 `tests/T37/corrected_cases.rs`（`t37_non_causal_target_rejected`、`t37_min_phase_substitution_fails_phase_where_corrected_passes`）与 `tests/T55/report_cases.rs`（`t55_report_corrected_complex_target_equivalence_and_delay`）。

### 6.7 插值与端点

- 插值：按 log10(f) 线性插值 dB 与展开后的 phase（单位 deg）：

      t = (log10(f) − log10(f1)) / (log10(f2) − log10(f1))
      mag = m1 + t*(m2 − m1)；phase = p1 + t*(p2 − p1)

- 端点：`edge_policy=hold`：网格外取端点值（报告覆盖范围与 hold 的频段）；`reject`：若设计需要覆盖 20 Hz 或 20 kHz 而网格未包含，拒收 E_TGT_EDGE。两种都必须报告 20 Hz/20 kHz 处的取值来源。

### 6.8 内置目标

- flat：mag_db(f)=0 对所有 f；默认网格 20 Hz–20000 Hz、480 点几何（f_k = 20*(1000)^(k/479)，k=0..479；公比 1.014525688，约 1/48.07 oct）；reference_gain_db=0。
- tilt10：mag_db(f) = −10*log10(f/20)/log10(20000/20) = −(10/3)*log10(f/20)，20<=f<=20000；端点外按 edge_policy。参考值：20 Hz 0 dB；100 Hz −2.329900；1 kHz −5.663233；10 kHz −8.996567；20 kHz −10.000000 dB。

### 6.9 拒收码（E_TGT_*，T11 必须全部实现并输出 kind=error）

**完整表 28 码**（评审 I03 已补 E_TGT_MISSING_FIELD / E_TGT_CSV_FORMAT / E_TGT_SIDECAR_MIX / E_TGT_POINT_FIELD / E_TGT_PHASE_UNSUPPORTED 共 5 个；原表 23 个）：

| # | 码 | 条件 |
|---|---|---|
| 1 | E_TGT_SCHEMA | schema 缺失/不等于 tspi-target-draft/v1 |
| 2 | E_TGT_FIELD_UNKNOWN | 顶层未知非 x_ 字段 |
| 3 | E_TGT_MISSING_FIELD | 必填字段缺失（schema/单位/runtime_rate/normalization/interpolation/edge_policy；有 phase 时 phase_unit/phase_ref/unwrap_policy；delay_removed 时 target_delay_samples；CSV 形式 source_hash） |
| 4 | E_TGT_UNIT | frequency/magnitude/phase 单位缺失或非 Hz/dB/deg |
| 5 | E_TGT_RATE | runtime_rate 非法或 != profile fs |
| 6 | E_TGT_POINTS | <2 或 >20000 点 |
| 7 | E_TGT_CSV_FORMAT | CSV 表头不是 freq_hz,mag_db 或 freq_hz,mag_db,phase_deg；字段数与表头不符；含注释行（# 行）；数字 token 非法/空字段 |
| 8 | E_TGT_SIDECAR_MIX | sidecar 携带 points、CSV 与 JSON 同时给 points/表、或同时提供两种形式 |
| 9 | E_TGT_POINT_FIELD | points 元素出现 freq_hz/mag_db/phase_deg 之外的键（x_ 前缀 warning 除外） |
| 10 | E_TGT_FREQ_INVALID | freq <=0 或非有限 |
| 11 | E_TGT_FREQ_ORDER | 非严格递增/重复 |
| 12 | E_TGT_FREQ_RANGE | freq > fs/2（magnitude-only 允许 = fs/2；带 phase 的 = fs/2 走 E_TGT_PHASE_REAL） |
| 13 | E_TGT_NAN | mag/phase NaN/Inf |
| 14 | E_TGT_MAG_MISSING | 某点缺 mag |
| 15 | E_TGT_PHASE_MIXED | 部分点有 phase 部分没有 |
| 16 | E_TGT_PHASE_UNIT | 无 phase 却给 phase_unit，或有 phase 却没给 |
| 17 | E_TGT_PHASE_REF | 有 phase 时 phase_ref 缺失/非法；delay_removed 缺 target_delay_samples |
| 18 | E_TGT_PHASE_JUMP | unwrapped 模式下相邻 |Δφ|>180° |
| 19 | E_TGT_PHASE_REAL | (a) 带 phase 目标含 freq = fs/2（Nyquist 相位退化）——**静态导入层 T11 判定**（唯一静态可判子条件；`targets.rs` stage 19；测试 t11_targets_phase_fs2_ruling）；(b) 相位无法由实系数因果 FIR 表示/自相矛盾——**静态层不可判**（见 §6.6/§6.9.1 注），由设计层 T37 + 报告层 T55 设计期判定并报 INCOMPLETE（联合 E_TGT_BUDGET） |
| 20 | E_TGT_PHASE_UNSUPPORTED | 所选家族不接受 phase（linear/min_*/apodizing/brickwall 收到 phase_ref=absolute/delay_removed 且未显式允许忽略） |
| 21 | E_TGT_DELAY | target_delay_samples 非整数/越界/与 D_used 不一致 |
| 22 | E_TGT_INTERP | interpolation 非 log_frequency_linear |
| 23 | E_TGT_EDGE | edge_policy 非法或 reject 项无法覆盖 20/20k |
| 24 | E_TGT_NORM | normalization 非法 |
| 25 | E_TGT_GAIN_RANGE | reference_gain_db 超出 [-24,+24] |
| 26 | E_TGT_BOOST | 超过 +12 dB 提升且未 allow_boost（硬上限 +18） |
| 27 | E_TGT_HASH | CSV 形式缺 source_hash 或与实际不符；JSON 给 source_hash 但无法按规范序列化复算 |
| 28 | E_TGT_BUDGET | 目标网格/相位要求超出 taps/延迟/预算（与设计方联合判定，INCOMPLETE 而非静默截断） |

### 6.9.1 非法输入 → 唯一码映射矩阵（评审 I03）

多错并存时按本表/码表从上到下取**第一个**命中码（每类输入只能映射一个码）；T11 必须实现该优先级并写单测。

| 非法输入类别 | 唯一码 |
|---|---|
| schema 缺失/未知 | E_TGT_SCHEMA |
| 必填元数据缺失 | E_TGT_MISSING_FIELD |
| 顶层未知字段 | E_TGT_FIELD_UNKNOWN |
| 单位缺失/非约定单位 | E_TGT_UNIT |
| runtime_rate 缺失/不等于 fs | E_TGT_RATE |
| CSV 表头/字段数/注释行/数字格式 | E_TGT_CSV_FORMAT |
| sidecar 带 points / CSV+JSON 混用 | E_TGT_SIDECAR_MIX |
| points 内未知字段 | E_TGT_POINT_FIELD |
| 点数越界 | E_TGT_POINTS |
| freq 非正/非有限 | E_TGT_FREQ_INVALID |
| freq 非严格递增/重复 | E_TGT_FREQ_ORDER |
| freq > fs/2（magnitude-only；带 phase 的 = fs/2 见下） | E_TGT_FREQ_RANGE |
| mag/phase NaN/Inf | E_TGT_NAN |
| 缺 mag | E_TGT_MAG_MISSING |
| phase 部分缺失 | E_TGT_PHASE_MIXED |
| phase 单位缺失/多余 | E_TGT_PHASE_UNIT |
| phase_ref/target_delay_samples 语义缺失 | E_TGT_PHASE_REF |
| phase 跳变越界且 auto 不收敛 | E_TGT_PHASE_JUMP |
| 带 phase 目标含 freq = fs/2（Nyquist 退化；静态层 T11 判定的唯一可判子条件） | E_TGT_PHASE_REAL |
| 家族不接受 phase | E_TGT_PHASE_UNSUPPORTED |
| target_delay_samples 非法/与 D_used 不一致 | E_TGT_DELAY |
| interpolation 值非法 | E_TGT_INTERP |
| edge_policy 非法/覆盖不足 | E_TGT_EDGE |
| normalization 非法 | E_TGT_NORM |
| reference_gain_db 越界 | E_TGT_GAIN_RANGE |
| 提升超限 | E_TGT_BOOST |
| source_hash 缺失/不符/无法复算 | E_TGT_HASH |
| 超 taps/延迟/预算 | E_TGT_BUDGET（与设计器联合 → INCOMPLETE） |

**注（E_TGT_PHASE_REAL 非静态部分，终审 errata C，F-23）**：“相位无法由实系数因果 FIR 表示”在静态导入层不可判（f<fs/2 的任意有限相位点可由足够长度实系数 FIR 表示；可表示性需 taps/D/预算，属设计期），**不进入本静态码矩阵**；由设计层 T37（不可因果 → E_FIR_NONCAUSAL；相位/预算不达 → 判据 INCOMPLETE）与报告层 T55（corrected_phase_target/corrected_complex_error + E_TGT_BUDGET）负责。T11 不猜测、不静默放行。

### 6.10 消费映射（哪个家族吃哪种目标）

| 家族 | 幅度 | 相位 | 约束 |
|---|---|---|---|
| linear | 必须 | 不允许（phase_ref 必须 none） | 幅度家族；phase_ref=absolute/delay_removed → E_TGT_PHASE_UNSUPPORTED（建议 corrected），或设计器明确忽略相位并在报告写 phase_target=ignored（仅当调用者显式请求，默认拒绝） |
| min_fast / min_slow / apodizing / brickwall | 必须 | 不允许 | 同上 |
| corrected / lowdelay | 必须 | 必须（absolute 或 delay_removed） | 复误差判据 §3.6 |
| nos | 可选（仅记录） | 可选（仅记录） | 不参与设计 |

### 6.11 与测量 CSV 的区别（再强调）

测量分析产物的 phase 是已去粗延迟的相对相位（MIMO-COEFF-PLAN 2.4a 记录了该口径与符号陷阱）；target 的 phase 是设计者期望值，禁止套用测量的去延迟变换、禁止用测量 CSV 冒充 target。target 的 `source_hash` 只用于追溯；不复用测量 schema。

## 7. 与 RT 宿主 / P·G / 既有契约的接线

### 7.1 P、G、F 与行列方向

- 统一模型（主文 §1）：x_N → A_map(O×N，仅选择/零填) → G 嵌入（5×5 低带 + 其余 identity/共同 D）→ P_post（对角目标 FIR/分频/phase/delay/polarity/保护）→ scatter。
- F_ji = p_j * G_ji，G = deltaD*I + g；**含 p_j*deltaD 对角 baseline**；F 的融合长度 L_F = L_G + L_P − 1。口径（与 CPU-BUDGET §5 一致）：若延迟 D 已包含在支撑长度 L 内，直接 L_F=L_G+L_P−1；若 D 未包含在 L 中，需另加 D（并单独记账）。
- **输入侧 delay/polarity 作用于列（A/G 的输入）**，**输出侧作用于行（P 的输出）**；G、P 不交换（非对称矩阵），禁止把 P_j 与 G 混成 P_i。
- 融合不总是省算：G=4096 + P=1024 → L_F=5119；25 条 5119 tap 的 direct 成本约 12.2856 GFLOP/s，比分开的 10.32192 GFLOP/s 更多（CPU-BUDGET §5）；是否融合由 T12 cost 计划决定，必须报告两种方案的成本与选择。
- 融合后 L_F 超过 profile 上限：保留 factorized 图并重新准入，或报 INFEASIBLE/offline；**禁止静默截断**后仍称精确。

### 7.2 运行准入边界（硬规则）

| 项 | 规则 |
|---|---|
| 常规 profile 每路由有效支撑 | <= 4096 taps |
| extended profile | 8192 / 16384 taps，必须通过 RAM/最坏 deadline/延迟/热切峰值准入（T56） |
| > 16384 | 仅设计工具 offline 输出，runtime 不放行 |
| 系数内存（f32） | 4*taps bytes/路由；profile 必须声明总预算并检查（例：25×4096×4=409600 B=400 KiB） |
| 延迟 | D/fs + block + SRC + I/O 计入 15/25/40 ms 预算；不足则 INFEASIBLE_FOR_PROFILE，不截 taps |
| 线性相位 4096@48k | 42.65625 ms > 40 ms → 15/25/40 三档都不可直接满足（WBS 明确）；8192/16384 为 85.322917/170.65625 ms |
| 任意/最小相位 | 必须报频变 GD 与共同 D；不得用“低频用途”豁免延迟预算 |
| 频率/采样率 | FIR fs 必须 == 运行 fs；跨 fs 必须重新设计（不重采样复用旧系数） |

延迟速查（线性相位，D=(L−1)/2）：

| taps | D samples | ms @48k | ms @96k |
|---|---|---|---|
| 512 | 255.5 | 5.322917 | 2.661458 |
| 1024 | 511.5 | 10.656250 | 5.328125 |
| 2048 | 1023.5 | 21.322917 | 10.661458 |
| 4096 | 2047.5 | 42.656250 | 21.328125 |
| 8192 | 4095.5 | 85.322917 | 42.661458 |
| 16384 | 8191.5 | 170.656250 | 85.328125 |

### 7.3 零填充线性卷积（运行器/文件后端的正确性）

- 分块频域卷积必须保证每块结果与直接线性卷积一致：每块有效线性卷积长度 >= L + B − 1；overlap-save/partitioned 必须保留 b−p 历史；禁止只用 FFT 大小 = B 的循环卷积。
- 判据（T14/T15/T55 共用）：单位冲激输入经 **64/128（生产候选）** 块、含跨块状态；256 仅作参考/测试附加块长（T02 core-v1 v1.1.1 N8，不是 runtime/生产候选）。输出前 L 点必须精确等于 h（f64 设计、f32 运行容差 <= 1e-6*max|h|），L 点之后 <= −140 dB；块边界无重复/丢样。
- 手算反例见 §9 V10：B=8 时循环卷积 y[0]=0.333333，正确线性值 0.111111。

### 7.4 stale、版本与确定性

- 任何 P/G/目标/role/fs/taps/overlay DAC 映射/保护变更 → 相关系数标 stale 并重设计；manifest 必须记录 `source.profile_sha256 / target_sha256 / measurement_sha256 / calibration_sha256` 的适用项。
- 运行期 prepare/apply 必须 ack 实际版本；坏集不生效、last-good 可回滚；stereo_strict profile 不得 runtime mask 既有 G（必须独立受限重设计并验证，WBS 主文）。
- 系数文件/图哈希链：coefficients.sha256 → manifest sha256 → compile/plan 版本；T12/T20/T26 消费。

### 7.5 精度/FFI（与本规范相关的部分）

- core 参考 f64、运行 f32/f64 能力必须显式；同构建固定累加顺序可复现；跨架构用容差而非 bit 一致。
- FFI（如需要）用 repr(C)、版本、ptr+len+stride/alignment；禁止 Vec/String/trait 跨界、panic 穿边界（T02 定义 ABI，本规范只引用）。

## 8. 人工检查表（T04 DoD：七家族幅相/支撑/延迟规格评审）

下表为作者自查结果（self-check），并已按独立评审 T04-review（NEEDS-FIX，12 项）修订；**复审仍 PENDING**，未做复审前不得宣称 DoD 已由第三方通过。C25–C31 为评审 I12 新增检查项。

| # | 检查项 | 依据 | 自查 |
|---|---|---|---|
| C1 | 七家族齐全、标识/中文/相位类/IIR-FIR 属性表完整 | §1.7 | PASS |
| C2 | 每家族有参数 schema、参考构造、规范性判据、拒收边界 | §3.1–§3.7 | PASS |
| C3 | 每家族通带判据可判（ripple 定义+阈值+测量程序） | §1.2, §3 | PASS |
| C4 | 每家族相位判据可判（linear 常数 GD、min 最小相位、corrected 复误差） | §3.1/3.2/3.6 | PASS（min_* 改用 M2 相位–Hilbert，评审 I08 关闭） |
| C5 | 支撑/延迟判据（taps、D、delay_ms、onset）明确且禁称零延迟 | §1.3, §3 | PASS |
| C6 | 振铃口径统一（W、pre/post、能量）且 apodizing 有 ≤−45 dB 判据 | §1.4, §3.4 | PASS |
| C7 | brickwall 可行性式与超预算拒收明确 | §3.5 判据 5 | PASS |
| C8 | min_fast/min_slow 可区分（数值边界） | §3.2 判据 4 / §3.3 判据 6 | PASS |
| C9 | corrected 不能冒充 linear/min 等价；复误差门明确 | §3.6 判据 2/6 | PASS |
| C10 | NOS 恒等且不承诺 DAC 复刻、保护在图外 | §3.7 | PASS |
| C11 | 家族自洽/互斥检查表 | §3.8 | PASS |
| C12 | 10 段模态 PEQ：段数/shelf/总数/范围界限 | §5.2 | PASS |
| C13 | RBJ peaking/shelf 公式与不变量、S 语义与 CONTRACT v1.1 一致 | §5.3 | PASS |
| C14 | 系数导出/自洽/原图比对/负向（填谷）判据 | §5.5–§5.6 | PASS |
| C15 | IIR 不硬截 FIR 的尾长判据与长度估计 | §5.7 | PASS |
| C16 | target 字段/单位/相位/展开/归一化/插值/端点/序列化完整 | §6.2–§6.8 | PASS（新增 Endpoint union 与 source_hash 规范序列化，评审 I03/I07 关闭） |
| C17 | target 拒收码覆盖 WBS 列举 + 评审新增（28 码 + 映射矩阵） | §6.9/§6.9.1 | PASS（原表 23 码漏 PHASE_UNSUPPORTED/CSV 解析等，评审 I03 关闭） |
| C18 | target 与测量 CSV 区分、不套去延迟公式 | §6.1/§6.11 | PASS |
| C19 | 端点 20 Hz/20 kHz 报告要求（Endpoint union，禁裸字符串） | §2.4, §1.2, §2.2 | PASS（评审 I07 关闭） |
| C20 | P/G 行列方向、L_F、零填充、非静默截断 | §7.1/§7.3 | PASS |
| C21 | 运行准入上限 4096/8192/16384/offline、延迟预算表 | §7.2 | PASS |
| C22 | 与 calibration v1.1 / channelmap v2 / v2.1 草案兼容声明 | §0.3, §2.3, §5.8 | PASS |
| C23a | 解析向量可复算（V1–V6、V8–V10、V12–V14、V18） | §9 | PASS |
| C23b | V11 分数延迟 GD 数值修正后可复算 | §9 V11 | PASS（评审 I01 关闭：GD=31.5、GD−31=0.5） |
| C23c | V7/V15 数值在定网格/定方法下可复现 | §9 V7/V15, §5.4 | PASS（评审 I04/I10 关闭） |
| C23d | V16/V17 为可复现的 reference（非规范 golden） | §9 V16/V17, evidence/T04 | PASS（构造/脚本已给，评审 I04 关闭） |
| C24 | 上游输入哈希与未测项/假设记录 | §10, §12 | PASS |
| C25 | target 拒收码表完整性（必填缺失/CSV 解析/sidecar 混用/点内未知字段/PHASE_UNSUPPORTED） | §6.3, §6.9/§6.9.1 | PASS（28 码 + 唯一映射矩阵） |
| C26 | endpoints union（measured/not_applicable_at_fs）与 E_FIR_ENDPOINT | §2.2, §2.4, §2.5 | PASS |
| C27 | 多测点阈值 3 dB（拟合）vs 1 dB（验收）不混写 | §5.6 2a/2b | PASS（对齐 peq-optimizer 需求 5/8） |
| C28 | modal 只切 / allow_boost 仅 wideband；shelf 生产/消费语义 | §5.2, §5.5 | PASS（评审 I05/I06 关闭） |
| C29 | min 相位验证方法可复现（M2 参数/网格/dB 公式/根检查可选） | §3.2 判据 2 | PASS（评审 I08 关闭） |
| C30 | V15 固定评估网格 + E_FIR_* 失败码集中表 | §5.4, §2.5 | PASS（评审 I10/I12 关闭） |
| C31 | 评审 12 项逐条关闭/证据记录在 §12.7 | §12.7 | PASS（本表 C25–C31 与 §12.7 一致） |
| C32 | crossover 裁决落 spec：fc<=0.2fs 生产硬上限 / 0.2–0.225 实验门 / >0.225 INFEASIBLE；LR 0°、BW 180° 家族极性；AnalyticPhase 规范构造；BW 偶数阶 INFEASIBLE；T39 证据互引 | §4.1, §2.5, §12.9 | PASS（v1.2-draft errata B，2026-11-17） |

## 9. 手算向量与解析验证（V1–V18）

以下向量是**规范自检**：实现方/评审者用它们核对公式与单位；V 的期望值为解析值（或作者参考实现的记录值，注明）。

**V1 偶数长对称 FIR 的 Nyquist 零点**：L 偶数、h[n]=h[L−1−n] ⇒ H(0.5fs)=Σh[n](−1)^n=0（成对抵消）。验证：L=4096 随机对称序列数值得 |H(π)|≈1.6e-14（应为 0）。

**V2 线性相位延迟**：D=(L−1)/2；L=4096 → 2047.5 samples → 42.65625 ms @48k；8192 → 85.322917；16384 → 170.656250；1024 → 10.656250。证明：H(ω)=e^{−jωD}·A(ω)，A 实。

**V3 3-tap 手算**：h=[0.25,0.5,0.25]（L=3 奇长、Type I 对称，D=(3−1)/2=1；评审 I11 修正旧稿“偶长”文案）。H(0)=1；H(0.5fs)=0；H(0.25fs)=−0.5j → −6.0206 dB、phase −90°；GD=1 sample（任意 f）。

**V4 5-tap 手算**：h=[1,2,3,2,1]/9。H(0)=1；H(0.5fs)=1/9 → −19.084850 dB；GD=2 samples。

**V5 RBJ peaking 不变量**：|H(f0)|dB = gain_db。数值：f0=1k,g=+6,Q=1 → 6.000000；f0=80,g=−4.5,Q=1.2 → −4.500000；g=0 → b=a 恒等。

**V6 RBJ shelf 不变量**：|H(f0)|dB = gain/2；low shelf DC=g、Nyq=0；high shelf DC=0、Nyq=g。数值：100 Hz/−3/S0.7 low → −1.500000；5k/+2/S0.5 high → +1.000000；80 Hz/−6/S1.0 low → −3.0，DC −6.0，Nyq 0.0。S=1 时 alpha/sin(w0)=0.70710678。

**V7 peaking 半增益带宽（信息性）**：|H|dB=g/2 的两交点 f1<f0<f2；f0/(f2−f1)≈Q。解析二分求根：f0=40,g=+12,Q=8 → **8.00004**（f1=37.5781、f2=42.5780）；f0=1k,g=+6,Q=1 → **1.00428**（f1=618.5798、f2=1614.3144）。仅 f0<=fs/16 时作 ±2% 检查；复算 scripts 见 evidence/T04/recompute-vectors.mjs V7（原稿 7.9368 为粗网格/括号误差，已按评审 I04 修正）。

**V8 tilt10**：20 Hz 0；100 Hz −2.329900；1 kHz −5.663233；10 kHz −8.996567；20 kHz −10.000000（log-f 线性）。

**V9 target 插值**：(100 Hz,0 dB)→(1 kHz,−10 dB) 在 316.227766 Hz 处 = −5.000000 dB；200 Hz 处 = −3.010300 dB。

**V10 零填充必要性（循环 vs 线性）**：h=[1,2,3,2,1]/9，块 B=8，x 在 n=0 与 n=7 各一个冲激。N=8 循环卷积 y[0]=0.333333，正确线性卷积 y[0]=0.111111（差 0.222222）；N 必须 >= 12。

**V11 分数延迟（63-tap Kaiser beta 8.96，d=0.5）**：h[n]=sinc(n−31.5)·w_kaiser[n]，基准延迟 (63−1)/2=31，故总分延迟 **31.5 samples**。复算（FFT N=65536、相位展开中心差分）：**总 GD = 31.49988…31.50012 samples（20 Hz–10 kHz），即 GD−31 = 0.5000 ± 0.0002**；原稿“GD−31.5=0.500”错误 0.5 sample，已按评审 I01 修正。|H| 与 0 dB 最大偏差 0.00977 dB（20 Hz–20 kHz；DC −0.00954 dB）；极性 invert 为精确 −1。复算脚本 evidence/T04/recompute-vectors.mjs V11。

**V12 延迟预算**：4096@48k=42.65625 ms > 40 → 15/25/40 全不满足；2048=21.322917 → 25/40 可、15 不可；1024=10.65625 → 15 可；分母换 96k 时减半。

**V13 Kaiser 可行性**：L_req=ceil(1+(A_s−7.95)*fs/(14.357078*Δf))，14.357078=2.285*2*pi（评审 I11：旧稿 14.3567 为截断常数，7 组表值不变）。表：A_s=60,Δf=1k,fs=48k→176；80→242；100→309；120→376；80,Δf=500,48k→483；80,Δf=1k,96k→483；60,Δf=2k,96k→176。

**V14 融合长度**：4096+1024−1=5119（线性 GD 2559 samples=53.3125 ms）；8192+4096−1=12287（6143 samples=127.979167 ms）；2048+2048−1=4095（42.645833 ms）。

**V15 PEQ f32 极端段**（固定网格：20 Hz–20 kHz、2001 点 f_k=20*1000^(k/2000)，见 §5.4/evidence V15）：30 Hz/−12 dB/Q=20 → 极点半径 0.999804；f32 量化后最大响应偏差 **0.19646 dB**（>0.1 → requires_f64）；40 Hz/−10/Q=16 → 0.03974 dB；25 Hz/−8/Q=12 → 0.12813 dB。评审 I10：旧稿未定网格，线性网格会得 ~0.02 dB 的不同结论。

**V16 apodizing vs brickwall 振铃**（作者参考实现，L=1024，构造与参数见 §3.4/§3.5；复算 evidence/T04/recompute-vectors.mjs V16，W=16）：apodizing（升余弦 f_p=18k→Nyquist，Kaiser beta 8.96）→ pre_ring −54.64 dB、post_ring −54.64 dB、pre_energy −55.42 dB、Nyq带最大 −36.01 dB；brickwall（Kaiser-windowed sinc，f_p=19k/f_s=20.5k、fc=19750、beta 8.96）→ pre_ring −30.33 dB、pre_energy −24.61 dB、[21k,24k) max <= −108 dB。→ 阈值 −45 dB 可区分且 apodizing 可达。边界：f_p=20400（0.85Nyq）→ −49.90 dB；f_p=21600（0.9Nyq）→ −37.87 dB（故边界 0.85）。**独立复算说明**：不同过渡带/窗构造会差 1–2 dB，但量级与区分维持；若评审者复算得到 ≥−45 dB，必须报告构造差异并升级裁决，不得静默改判据。

**V17 min-phase**（作者参考实现，可复现；构造见 §3.2/§3.3 与 evidence/T04/recompute-vectors.mjs V17）：
- min_fast：升余弦幅度 f_p=16k→f_s=19k（floor −120 dB）、L=2048、beta 8.96 → 同态最小相位；**hilbert_consistency=0.0045°**（M2 通过，≤2°）；通带幅度差 2.3e-11 dB；GD = 1.5886（50 Hz）、1.5963（1 kHz）、2.7442（10 kHz）、7.5394（15 kHz）、31.3770（18 kHz）samples。
- min_slow：升余弦 f_p=16k→Nyquist、L=1024、beta 8.96 → **hilbert_consistency=0.0036°**；GD = 0.2336（50 Hz）、0.2346（1 kHz）、0.3864（10 kHz）、0.9288（15 kHz）、2.2037（22 kHz）samples；Nyquist 带 max −40.99 dB。
- 评审 I04/I08：旧稿 inverse-causality 数值对截断设计不构成 PASS 证据，已改为 M2（相位–Hilbert）规范判据；旧 −181.7 dB 仅保留为历史构造的诊断，不再作为门槛。

**V18 NOS**：h=[1.0]，L=1，D=0；任意 f 的 |H|=1、phase=0；max|h[0]−1| <=1e-12。

## 10. 假设、未测项与风险（必须随交付保留）

**假设（A1–A10）：**

1. 运行期以 f32 为主（与 T03/T07 计划的 f64 参考 + f32 运行一致）；极端高 Q 低频段可能需 f64 路径（§5.4）。
2. 设计工具为 f64 离线；导出 f32 WAV；系数确定性可复现。
3. 七家族参数/阈值是工程初稿，T10/T33–T38/T55 实现时可在**不放宽**判据的前提下细化；任何放宽需回写本规范并请 Lead 裁决。
4. 模态 PEQ 的拟合质量判据沿用 peq-optimizer requirements（T64 的 fit 由新 daemon 实现，§5.6 借用其验收口径），不是本规范新发明房间参数。
5. target 的 phase_ref=absolute/delay_removed 语义是 T11 的草案；若 T11 实现发现歧义，先改本节再实现。
6. m=5 低带 participants、overlay role→DAC 映射、stereo_strict 等以 T57/主文为准，本规范不硬编码角色。
7. 15/25/40 ms 是整链附加预算，本规范只计算 FIR D，其余 block/SRC/I/O 由 T26/T56/T29 实测；任何“满足预算”结论必须整链实测。
8. channelmap v2.1 per-route FIR 仍是草案（web/docs/API.md §10 自述“待 benchmark 后实现矩阵页”），本规范只对齐字段意图。
9. 96 kHz 下阈值按时间/相对带宽换算，本文表以 48k 为基准；T55 必须按实际 fs 重算。
10. 上游文档在任务期间被 Lead 更新（WBS/主文行数增加；修复期间 CONTRACT/ACCEPTANCE/API/MIMO 亦更新）；已比对 T04 相关语义未变，哈希以 §12 收尾版为准。
11. 评审建议“若坚持 V16 作为 −45 dB 依据，至少给可复现参考实现参数”已落实为 evidence 脚本；但不同构造可差 1–2 dB（评审独立复算 brickwall 得 −31.01 dB，本规范构造 −30.33 dB），阈值仍属工程默认，复审可要求再收紧或改判据。

**未测项（NOT_RUN）：**

- 未实现任何代码；未运行 cargo test；未生成真实系数 WAV；未跑 T55 报告器。
- §9 的 V16/V17 等“作者参考实现”现由 evidence/T04/recompute-vectors.mjs 脚本定义并复算（可复现），但**不是**规范 golden，也不是 T10/T33–T41 的实现证据；T55 必须用自己的独立实现复算。
- 独立评审 2026-11-16 结论 NEEDS-FIX（12 项）；v1.1-draft 修复 12/12（§12.7），但**复审尚未完成**（按 task-34 不得自审）。
- 未做板端/ALSA/音频/RT 测试；未做真实房间测量/校准/应用后复测。
- 未做独立人工评审（Lead/第三人），本文仅作者自查。
- 未验证 96k 与 44.1k 端到端；未验证 MOTU/USB 实际延迟。

**风险：**

1. 阈值偏紧/偏松：min_fast 的最小相位验证、apodizing 的 −45 dB、corrected 的 1 dB/5° 需 T10/T33–T37/T55 首轮实现后按实测校准；调整必须同步改本规范版本与所有消费者。
2. target phase 语义若与 T11 实现不一致，可能导致 corrected 设计无法收敛；需先对齐 schema 再实现。
3. 极端模态 Q（>10）在 4096 tap FIR 常规预算内不可行；必须保留 IIR 路径（§5.7），否则会误导运行器。
4. 延迟预算：线性相位 4096 已超 40 ms，若产品坚持线性相位 maka 只能 1024/2048 或接受超过 40 ms；必须显式产品决策，不能用“看起来没问题”绕过。

## 11. 变更记录与评审状态

| 日期 | 变更 |
|---|---|
| 2026-11-16 | v1-draft：首版（T04）；七家族 + 10 段模态 PEQ + target 导入 + 运行边界 + 手算向量；作者自查 C1–C24；独立评审 PENDING。 |
| 2026-11-17 | v1.1-draft：按独立评审 T04-review（NEEDS-FIX 12 项）修复 I01–I12（12/12，0 DECLINED）：V11/V7 数值、§5.6 多测点 1 dB、target 28 码+映射矩阵、C17/C23 收紧、modal 只切/shelf 生产消费、Endpoint union、min-phase M2、antisymmetric/HP、V15 网格、P3 文案；新增 evidence/T04 脚本与结果、§12.7 关闭表。 |
| 2026-11-17 | v1.2-draft：Lead 裁决 fs/2 边界——magnitude-only 目标点允许 freq <= fs/2；带 phase 目标点禁止 freq = fs/2（Nyquist 相位退化，E_TGT_PHASE_REAL 拒收）。§6.4/§6.6/§6.9/§6.9.1 已改并记 evidence/T04/RULING-fs2-2026-11-17.md。 |
| 2026-11-17 | v1.2-draft errata A：§3.1 判据 4/5 的 HP 写法改为镜像读法——HP 阻带 [0,f_s]（整段纹波峰值 < −A_s），过渡用“最后一次离开 −A_s 的穿越点 >= f_s − tol”，tol=max(0.05*(f_s−f_p),20 Hz) 与 LP/§3.5 判据 4 同口径；§1.2 过渡测量同步 LP/HP 首次/最后口径；同时修 T04:45/682 的 64/128/256 措辞（生产候选只 64/128；T02 v1.1.1 N8）。记 evidence/T04/ERRATA-hp-crossing-2026-11-17.md。 |
| 2026-11-17 | v1.2-draft errata B：按 Lead 裁决回写 T39 crossover 判据——fc 生产硬上限 0.2fs（含）、0.2–0.225fs 实验门（flag+warning、非生产、不得 accepted）、>0.225fs INFEASIBLE；LR 全阶家族极性后 fc 0° 同相和、BW 奇数阶一支反相 180°（fc 按解析目标，不强制 0°/线性相位）、BW 偶数阶解析和不平坦必须 INFEASIBLE；AnalyticPhase 为规范构造、LinearPhaseMagnitude 仅非规范对照；§2.5 增 E_FIR_XOVER_* 码，§3.8 增工具行，§8 增 C32，§12.9 互引 T39 代码/测试/日志证据。记 evidence/T04/ERRATA-crossover-2026-11-17.md。 |
| 2026-11-17 | v1.2-draft errata C（Cursor v0.9 终审修复组 A）：F-22 参数域码唯一化——撤销 `E_FIR_XOVER_BAND`（alias→`E_FIR_BAND`，§2.5 唯一参数域码）、`E_FIR_XOVER_LAYOUT` 正式补登记（布局/拓扑非法）；F-23 `E_TGT_PHASE_REAL` 责任层拆分——fs/2 由静态导入层 T11 判定，实系数可表示性静态层不可判、由 T37/T55 设计期承担（§6.6/§6.9/§6.9.1）；F-24 BW 偶数阶相位项只报告（NOT_APPLICABLE、不计 pass），拒收只走 `E_FIR_XOVER_SUM`（§4.1/§2.5）。记 evidence/cursor-overflow/F-22|F-23|F-24/ 与 evidence/T04/ERRATA-crossover-2026-11-17.md §7。 |

评审状态：**独立评审 2026-11-16 NEEDS-FIX（12 项）→ 2026-11-17 修复完成（12/12）→ 复审 PENDING（按 task-34 不得自审；作者自查仅 C1–C31 self-check）**。

## 12. T04 Manifest（本文件的证据）

### 12.1 产物

- 交付文件：projects/tspi-audio-dsp/rt-stack/spec/fir-toolbox.md（本文件，v1.2-draft）。
- 证据文件（task-34 授权写域 rt-stack/evidence/T04/）：
  - evidence/T04/recompute-vectors.mjs（19,502 B；V1–V18 全部可复算脚本，含 V16/V17 构造参数）；
  - evidence/T04/vector-results.json（8,539 B；脚本输出，sha256 f9411b3e…2780）；
  - evidence/T04/MANIFEST.md（证据摘要：输入/命令/exit/关闭表/未测项）；
  - evidence/T04/recompute.err.txt（0 B，脚本 stderr 为空）；
  - evidence/T04/RULING-fs2-2026-11-17.md、ERRATA-hp-crossing-2026-11-17.md、ERRATA-crossover-2026-11-17.md（Lead 裁决记录，见 §12.9）；
  - evidence/T04/check_spec.py + spec-check.log（可复现 spec 检查脚本；`python evidence/T04/check_spec.py` exit 0）。
- 说明：T04 原 WBS 写域只有 spec 文件；v1.1 修复由 task-34 新增 evidence/T04 授权。除上述文件外未创建/修改任何其它文件；未 git commit；未触板/音频/服务/模式。

### 12.2 输入文件与 SHA256（收尾读取版；size bytes）

| 输入 | sha256 | bytes |
|---|---|---|
| scratch/lead/planner/TSPI-RT-STACK-WBS.md | ffe5cde9e27470f0c2db9c4242383478422f553defba9370c8d268d0643204b9 | 35728 |
| scratch/lead/planner/DSP-HOST-DECISION.md | ab1056477d8e1c01dbe61df6f2d15c4bb0cca6b05f9177ef51559df3e2d25ded | 27951 |
| scratch/lead/planner/DSP-HOST-CPU-BUDGET.md | 5954bfa2c64210e1dd4c75268aa674976c3f67fa35c2000363cc6dc5e25a2aa1 | 18859 |
| calibration-spec/CONTRACT.md | 522640c38c98e8d826ede82cfac15cbdb97766d935071f836bfe336c4d5727e9 | 22256 |
| calibration-spec/ACCEPTANCE.md | 24731cdea518c0996447246e5d3792e39cdc0594f573ad13bd5a633b8ec4d431 | 28692 |
| web/docs/API.md | ffcd0898f98ba317ab836918d7fc61fcf15f8a540ae639382c0e97b842ac6f08 | 26056 |
| audio/overlay/etc/tspi-audio/layout-presets.yml | 4856109265371666a6001804330e2a595dad8599b3aecc2f4390be679b2427da | 11228 |
| audio/overlay/usr/local/bin/tspi-channelmap | d7eeeb93c55c5ae26489a7c2f0d56d1a2303b6e3429215a15671ef3d19276365 | 74505 |
| web/src/channelmap.rs | ac5bf41e123ce723fc75bcb01910769529e785d8fc17a75aafa31cec7a192d87 | 102498 |
| upmix/docs/PEQ-TYPE.md | 5f0691f2c14510d6b3685d9a10574db6d086d22fbba454c9e25e873913c2761f | 4909 |
| docs/MIMO-COEFF-PLAN.md | fc832832487e73182787ad19e4fb3639edb3355584a33a5f9a4d707f539c3f30 | 45259 |
| docs/E2E-ACCEPTANCE.md | 17b6c4ea74417a52e7252a13332c14eeb0f82e6b9bc2a113314160f72fd8bd71 | 12262 |
| specs/peq-optimizer/requirements.md | c0bf7cf706c0544f292e9db58eebc52a97a9c018152911ee42750c52d21a0c73 | 13017 |
| specs/peq-optimizer/design.md | 872107be08cc3d35a686562d0965106434a46d57b3664feacfcb29ac21531b69 | 9618 |
| audio/tools/mimo-engine/README.md | f185e5a0b81e631d9506edf3b08e5e49afb266666d19917d5e0541376b3b488e | 3240 |
| audio/configs/fir/gen-fir-wav.py | 064bf589755ebfe045297a8d9a399f792a1d2961ca8ef1c0c5c47763063a8464 | 2909 |
| scratch/lead/rt-reviews/T04-review.md（评审输入） | 500af634333655b38165a76a824051f9cfdbc6d41154160a3c61399649068185 | 20593 |
| rt-stack/evidence/T04/recompute-vectors.mjs | 6abab7642a7c577f8cdae790b252f09a354c1e4de8c6b92734c7e30ec4591484 | 19502 |
| rt-stack/evidence/T04/vector-results.json | f9411b3ec5d5ee8c491a459c35b4d5b9142c3957b331a3c67545314240ba2780 | 8539 |
| rt-stack/spec/fir-toolbox.md（本文件，§12.4 记录前快照） | 3eeb6c7f9e0bdcbf437388565cb678ad0e77b2de745323602ab6189a78ed3805 | 76907 |

哈希命令（本机 PowerShell 7，exit 0；20 个文件：16 上游 + 评审 + 脚本 + 结果 + 本文件快照）：

    Get-FileHash -Algorithm SHA256 -LiteralPath <上述文件>

注：与 v1 相比，CONTRACT / ACCEPTANCE / API / MIMO-COEFF-PLAN 已被上游更新（文件变大/哈希变化）；作者已复核本规范引用的关键语义（CONTRACT shelf producer/consumer 约定、API v2.1 per-route 草案、§5.6 阈值来源 peq-optimizer 未变）未变。其余 12 个输入与 v1 哈希一致。**上游文档仍在并发修订**：本节 hash 是 2026-11-17 00:0x 快照（例如 CONTRACT 在 review 后已再次更新为 1664028e…/22329 B）；复查/复审时请以 `Get-FileHash` 当前值为准，语义复核结论按本注保持不变。

### 12.3 生成与验证命令、结果（v1.2-draft，2026-11-17）

1. **输入哈希**：PowerShell `Get-FileHash -Algorithm SHA256` 批量执行 **20/20 成功，exit code 0**（16 上游 + 评审 + 脚本 + 结果 + 本文件快照；输出即 §12.2 表）。
2. **手算向量复算（可复现）**：`cd rt-stack/evidence/T04 && node ./recompute-vectors.mjs` → **node_exit=0**，写 `vector-results.json`（8,539 B，sha256 f9411b3e…2780；node v24.13.0）。V1–V18 全覆盖；V16/V17 的构造参数写死在脚本内。关键复算值：V3/V4/V5/V6 解析不变量、V7=8.00004 / 1.00428、V11 总 GD=31.49988…31.50012（GD−31=0.5000±0.0002）、V15=0.19646/0.03974/0.12813 dB、V16 apodizing pre −54.64 dB / brickwall pre −30.33 dB / 边界 −49.90、−37.87、V17 hilbert 0.0045°/0.0036°、V13 7 组 L_req 不变。
3. **评审 12 项 RG 关闭检查（PowerShell，exit 0）**：32 个 I01–I12 断言字符串全部 PASS；checklist_rows=34、vectors=18、review_checks_failed=0；输出 `T04_REVIEW_CLOSURE_RG PASS`。
4. **内容一致性（本会话 run_code，分两段 read 拼接以绕过读取工具单次返回上限）**：fails=[]；888 行 / 50,579 字符；13 个一级章节、7 个家族小节、C1–C31（34 行含 C23a–d）、V1–V18、28 个 E_TGT 码、延迟表 6 行复算、Kaiser 7 组、tilt10/log 插值/alias/融合/振铃/f32 等数值串齐全；旧错值（`7.9368`、`GD−31.5=0.500`）仅出现在“原稿/评审/错误”说明行（0 处无上下文残留）。
5. **人工检查表**：§8 C1–C31 作者自查 PASS（self-check）；**复审 PENDING（task-34 明确不得自审）**。

### 12.4a v1 收尾检查（历史记录，2026-11-16）

收尾检查于 2026-11-16 执行，命令与结果如下（本记录追加前文件快照：770 行 / 40,358 字符 / 61,688 字节 / sha256 5467cb09a732dfc3f56c6f1906c2a940c1ffdb7be870957ca78ff99cee5c7f1f）。

1) **内容一致性程序（本会话 run_code，分两段 read 拼接以绕过读取工具约 32KB 单次返回上限）**：

   检查项：13 个一级章节标题、7 个家族小节、C1–C24、V1–V18、13 个拒收/失败码、
           延迟表 6 行与 (L-1)/2 复算、Kaiser L_req 7 组、tilt10/log 插值/alias/融合/振铃/
           min-phase/PEQ f32 共 13 个数值串、无模板/路径转义伪影。
   结果：fails = []，allPass = true，chars = 40358，totalLines = 770。

2) **PowerShell 结构检查（实际命令，exit code 0）**：

   - 命令：Get-Content -Raw 后对标题/家族/清单/向量/拒收码/延迟值做 Contains 与正则计数；
   - 输出：file bytes=61688 sha256=5467cb09a732dfc3f56c6f1906c2a940c1ffdb7be870957ca78ff99cee5c7f1f；checks_failed=0；STRUCTURE_CHECK PASS；exit 0。
   - 追加本记录后文件 sha256 会变化（自引用），最终值以 Get-FileHash -Algorithm SHA256 -LiteralPath 本文件 复算为准；本记录不改变 §0–§11 的任何语义。

3) **人工检查表**：§8 C1–C24 作者自查 PASS（self-check），独立人工评审 PENDING（见 §11）。

4) **未做**：任何代码实现/cargo test/板端/音频/RT/真实测量/独立评审（见 §10 NOT_RUN）。

### 12.4b v1.1 修复后验证（task-34，2026-11-17）

快照（本节写入前）：888 行 / 50,579 字符 / 76,907 字节 / sha256 3eeb6c7f9e0bdcbf437388565cb678ad0e77b2de745323602ab6189a78ed3805。

1) **手算向量脚本**：`node evidence/T04/recompute-vectors.mjs`，exit 0；vector-results.json 8,539 B / sha256 f9411b3e…2780；node v24.13.0。
2) **评审 12 项 RG 关闭**：PowerShell 32 断言 + 计数（checklist_rows=34、vectors=18、review_checks_failed=0）→ `T04_REVIEW_CLOSURE_RG PASS`，exit 0。
3) **内容一致性**：chunked read 拼接 → fails=[]，888 行 / 50,579 字符；13 章 / 7 家族 / C1–C31 / V1–V18 / 28 码 / 延迟表复算 / 旧错值仅存于说明行。
4) **输入哈希**：20/20，exit 0（§12.2）。
5) **未做**：代码/cargo/板端/音频/RT/真实测量；**复审**（不同人）未做。
6) **最终快照（§12.7/§12.8 追加后、本行写入前）**：953 行 / 56,159 字符 / 85,388 字节 / sha256 610dd2a570d57f1ba5f6e794267a06a4a32a1c0252f7d133e37286a3292226fc；evidence/T04 目录 4 文件（脚本 19,502 B、结果 8,539 B、stderr 0 B、MANIFEST.md 6,434 B）。

注：本节写入会改变本文件哈希（自引用），最终值以 `Get-FileHash -Algorithm SHA256 -LiteralPath <本文件>` 复算为准；本节只增验证记录，不改 §§0–11 语义。v1 记录保留在 12.4a 与 task-24 卡。

### 12.5 未测项与假设

同 §10（NOT_RUN 清单与 A1–A11 假设），此处不重复。核心未测：无代码/cargo/板端/音频/RT/真实测量；V16/V17 是作者参考构造（脚本可复现但不是规范 golden）；**复审 PENDING**。

### 12.6 交接与下一步

- Reviewer/Lead：按 §8 与 §12.7 做**独立复审**（不得由作者自审）；确认 I01–I12 关闭后可把本文件标为 T04 PASS。evidence/T04/ 已由 task-34 授权建立（见 §12.1/§12.8）。
- T10/T33–T38：实现七家族，逐条对照 §3 判据与 §9 向量；任何判据不可实现/需修订先回写本文并 Lead 裁决。
- T11：实现 §6 全字段与拒收码；先冻结 phase_ref 语义再实现 corrected 消费。
- T41/T64：实现 §5（10 段 modal PEQ、RBJ 不变量、原图比对、IIR 尾长）。
- T55：按 §1.2/§1.4 实现独立报告器，并把 §9 V1–V18 作为自检用例；V16/V17 的作者参考值不是 golden，需用自己的实现复算。
- T12/T56：按 §7.1/§7.2 做融合 cost 与准入，禁止静默截断。

### 12.7 独立评审 12 项关闭记录（task-34；12/12 FIXED，0 DECLINED）

评审输入：scratch/lead/rt-reviews/T04-review.md（sha256 500af634…8185）。修复快照：本文件 v1.1-draft（3eeb6c7f…3805，§12.4b 记录前）。

| # | 严重度 | 问题 | 修复/位置 | 证据 | 状态 |
|---|---|---|---|---|---|
| I01 | P1 | V11 分数延迟 GD 错（GD−31.5=0.5 差 0.5 sample） | §9 V11 + §4.2：总 GD=31.5、GD−31=0.500；DC −0.00954 dB、最大偏差 0.00977 dB | evidence V11；§12.4b | FIXED |
| I02 | P1 | §5.6 多测点 3 dB 与 peq-optimizer 需求 8.4 的 1 dB 冲突 | §5.6 拆 2a（拟合单点 ≤3 dB，需求 5 AC7）/2b（多测点验收恶化 ≤1 dB，需求 8 AC4）/2c | 引用需求编号；§12.3 检查 | FIXED |
| I03 | P1 | target 拒收码表漏 PHASE_UNSUPPORTED/必填缺失/CSV 解析/sidecar 混用/点内字段；source_hash 序列化不精确 | §6.3 精确序列化算法 + §6.9 扩为 28 码 + §6.9.1 唯一映射矩阵 | RG 检查 5 个新码 + 28 码 | FIXED |
| I04 | P1 | V7/V16/V17 不可复现，C23 过宽 | V7=8.00004；V16 构造参数+数值；V17 构造参数+hilbert 0.0045°/0.0036°；新增 evidence 脚本；C23 拆 a–d | evidence/T04/recompute-vectors.mjs + vector-results.json | FIXED |
| I05 | P2 | modal allow_boost 与 peq-optimizer 需求 5 冲突 | §5.2：modal 恒 ≤0；allow_boost/+3 dB 仅 wideband（需求 6 AC4）；§5.5 注释 | §13 检查 I05 | FIXED |
| I06 | P2 | shelf 段数生产/消费语义未区分 | §5.2 生产端（T41/T64）约定；消费方不得假设最多一段、总数 ≤32 | RG 检查 | FIXED |
| I07 | P2 | endpoints not_applicable 表示与 manifest 类型冲突 | §2.2/§2.4 Endpoint union（measured/not_applicable_at_fs）+ E_FIR_ENDPOINT（§2.5） | RG 检查 | FIXED |
| I08 | P2 | min 相位验证不可复现/表述不严 | §3.2/§3.3：M2 相位–Hilbert（N=65536、floor −120 dB、≤2°）；roots 仅可选；inverse-causality 降为诊断（MAY） | V17 复算 0.0045°/0.0036° | FIXED |
| I09 | P2 | antisymmetric/LP 矛盾与 HP Nyquist 规则 | §3.1：antisymmetric 仅 HP；判据 9 改为“仅当 0.5fs 在 passband 才需 L 奇数/频谱反转”，0.499fs 不要求 | RG 检查 | FIXED |
| I10 | P2 | V15 网格未定义 | §5.4/V15：固定 20 Hz–20 kHz、2001 点 log 网格；值 0.19646/0.03974/0.12813 dB | evidence V15 | FIXED |
| I11 | P3 | 文案/常数：target_delay 二义、fs/2 矛盾、“加速延迟”、14.3567、V3 奇偶、L_F 表述 | §3.6/§6.4/§6.6/§3.5+V13/V3/§7.1 逐条改；常数改 14.357078=2.285*2π | RG 检查 | FIXED |
| I12 | P2 | C1–C24 遗漏/自评错误 | §8 增 C25–C31，C4/C16/C17/C19 改注，C23 拆 C23a–d；状态如实 | RG checklist_rows=34 | FIXED |

汇总：**P1 4/4、P2 7/7、P3 1/1 全部 FIXED；DECLINED = 0**。剩余：独立复审（PENDING）、T10/T33–T41/T55 实现验证（未开始）。

### 12.8 evidence/T04 文件与复现

| 文件 | bytes | sha256 | 用途 |
|---|---|---|---|
| recompute-vectors.mjs | 19502 | 6abab764…1484 | V1–V18 可复算脚本（含 V16/V17 构造） |
| vector-results.json | 8539 | f9411b3e…2780 | 脚本输出（node v24.13.0） |
| recompute.err.txt | 0 | — | stderr（空） |
| MANIFEST.md | 9503 | 89451b4be4e2c76f7dfb73e2d6256e7dfa3d6f304247cf43d5bd97856bb9154f | 任务证据摘要/命令/exit/关闭表/未测项（自引用，最终值以复算为准） |
| RULING-fs2-2026-11-17.md | 2598 | 2097aca09a3e206abf69ea3111f3b0d08a93c8d856742f19d3e3e10998989b92 | Lead fs/2 裁决与规范改动记录（magnitude-only ≤fs/2；带 phase 禁 =fs/2） |
| ERRATA-hp-crossing-2026-11-17.md | 5394 | ad8388c2f38b20621c04dd46a811b22ed1cd22aff41e865653611a151a1ba5a8 | Lead 裁决 A：HP 阻带/穿越判据镜像读法 + block 64/128/256 措辞（T02 N8） |
| ERRATA-crossover-2026-11-17.md | 7447 | 1925b8fb0a94a19613151108c723f1917bfe6314ccc95c288d1e892262fdb0f2 | Lead 裁决 B：T39 crossover fc≤0.2fs/实验门/家族极性/AnalyticPhase 回写记录 |
| check_spec.py | 5087 | 57c4ff06128634f7fce6db5b0dde6832f08383e5cd553b8171b72c3db8bbd098 | T04 spec 检查脚本（exit 0；断言版本/章节/V1–V18/C1–C32/错误码/errata A+B） |
| spec-check.log | 150 | 8131a4079eb4b7f7b3f5abdf2786415e73500f1da55be35afabde7e8587be798 | check_spec.py 实际输出（RESULT: PASS） |

复现命令：

    cd rt-stack/evidence/T04 && node ./recompute-vectors.mjs   # exit 0, 重写 vector-results.json
    Get-FileHash -Algorithm SHA256 -LiteralPath vector-results.json   # 与上表比对

未测：T55 独立报告器未实现，脚本结果只能作为参考而非 T55 oracle；V16/V17 构造不同实现的差异（评审实测 brickwall −31.01 dB vs 本构造 −30.33 dB）已在 §3.4/§3.5/§9 说明。

### 12.9 互引下游证据（T39 crossover，RULED 2026-11-17；只读引用，本任务未修改）

| 对象 | 实测 bytes / sha256（2026-11-17 13:0x） | 用途 |
|---|---|---|
| crates/designer/src/crossover.rs | 89,661 / 6df537d0e15fc4c48ff31c1a9702d66a994b7c74e562d431455f123ae1403dba | RULED 模块文档（§45-58）、FC 常量（§113-118）、AnalyticPhase/家族极性实现 |
| crates/designer/tests/T39/crossover_cases.rs | 50,719 / 973bde610173b7d27af30c5dc37f1e057ca7efbd060a4fd2b8f47ffa9d8616fa | RULED 档位（§1082+）/家族极性（§1133+）/AnalyticPhase 自洽用例 |
| rt-stack/evidence/T39/run-tests.log | 2,561 / b98ff89e10d8c15df6907493be7bb3f113625f9d9b331df64bbf63c4b27d8160 | `cargo test -p khaen-designer crossover`：15 passed / 0 failed |
| rt-stack/evidence/T39/metrics.log | 16,605 / b5726a1353d411314f2d7ac87762b346d0d9a5d446c5df926aff2eab3d66e776 | LR4 sum dev 0.0288 dB；BW6 flat_dev 2.8710 accepted=false；BW7 0.000422 等 |
| rt-stack/evidence/T39/MANIFEST.md | 16,615 / c2bcf3b0d1bccac06047295dde31e6f9f0427d8259d864e7400611eac65c6a9f | §5 §4.1 映射 / §6 设计口径 / §7 实测快照（注意 §2 源码/测试哈希为修复前口径） |
| scratch/lead/rt-reviews/FIR-DESIGNER-review.md | 10,884 / 848750cf87a2cc55e62651940045e23dc483c9bd3e1b4b6a82ab19baf608f5f1 | 外部评审：I01 fc>0.2fs 未落地、I02 §4.1 构造冲突 → 由 errata B + T39 修复关闭 |

复现/复核：`Get-FileHash -Algorithm SHA256 -LiteralPath <上表文件>`；`python rt-stack/evidence/T04/check_spec.py`（exit 0）。
**证据同步提示（不阻塞本 spec errata，交 T39 owner）**：①T39 MANIFEST §2 记录的源码/测试哈希仍为修复前（80,146/41,027 B），当前已更新为 89,661/50,719 B，需重跑 `make_evidence` 类流程更新；②`evidence/T39/RULED-T39-2026-11-17.md` 被 crossover.rs 引用但尚未落盘，需补；③T39 现有 run-tests.log 为 15 tests 版本，当前测试文件含 RULED 新增组，需重跑并更新日志。

### 12.10 终审修复 errata C 互引（F-22/F-23/F-24，2026-11-17；组 A）

> 来源：Cursor v0.9 终审 finding F-22/F-23/F-24（P2/P2/P3）与 Lead F-22 裁决（as-built 为准：参数域统一 `E_FIR_BAND`）。执行：fir-fix（task-1）。写域：本文件 + `crates/designer/src/{crossover,targets}.rs` + `tests/T39`/`tests/T11` + `evidence/cursor-overflow/F-22|F-23|F-24`。未改 ABI/公共 API/其它家族码名，未 git commit。

| finding | 规格改动（本文件） | 代码/测试（互引） | 证据目录 |
|---|---|---|---|
| F-22 | §2.5 撤销 `E_FIR_XOVER_BAND`（alias→`E_FIR_BAND`，参数域唯一码）+ 错误码唯一性说明；新增 `E_FIR_XOVER_LAYOUT` 行（布局/拓扑非法，含最小触发）；§4.1“无默认角色 HP/LP”补布局码触发；§3.8 crossover 行更新 | `crates/designer/src/crossover.rs`（参数域仍输出 `E_FIR_BAND`；布局码常量与触发不变；注释标注唯一权威名）；`tests/T39/crossover_cases.rs::t39_error_codes_align_spec`（代码+规格一致性断言） | `rt-stack/evidence/cursor-overflow/F-22/` |
| F-23 | §6.6/§6.9 #19/§6.9.1 责任层拆分：fs/2 子条件由静态导入层 T11 判定（唯一静态可判子条件）；“实系数可表示性”静态层不可判，由设计层 T37 + 报告层 T55 设计期判定（INCOMPLETE/E_TGT_BUDGET） | `src/targets.rs` 模块文档与 `PhaseReal`/stage 19 注释；`tests/T11/target_import_cases.rs`（fs/2 实系数反例、非 Nyquist 静态不判边界、责任层规格断言） | `rt-stack/evidence/cursor-overflow/F-23/` |
| F-24 | §4.1 家族相对极性/判据④：BW 偶数阶 `xover_family_polarity` 与 `xover_phase_at_fc` 标 NOT_APPLICABLE（只报告、不计 pass，不得以相位门放行/拒收），拒收只走合并平坦判据 → `E_FIR_XOVER_SUM`；§2.5 `E_FIR_XOVER_PHASE` 行注；§3.8 行注 | `crossover.rs` `build_criteria`（偶数阶相位项 limit=None/pass=false/statement 标 NOT_APPLICABLE）/ `design_strict`（偶数阶不映射 `E_FIR_XOVER_PHASE`）/ `family_phase_rule`；`tests/T39/crossover_cases.rs::t39_bw_even_honest_infeasible` 断言更新（奇数阶/LR 断言不变） | `rt-stack/evidence/cursor-overflow/F-24/` |

复跑（组 A 收尾实测，详见各证据目录 COMMANDS.md）：

    cargo test -p khaen-designer --offline            # 0 failed
    cargo test -p khaen-designer --offline xover      # T39 filter，executed > 0
    cargo test -p khaen-designer --offline t11        # T11 filter，executed > 0
    python rt-stack/evidence/T04/check_spec.py           # exit 0（本 errata 的断言已加入）

注：本轮修复后 `crossover.rs` 与 `tests/T39/crossover_cases.rs` 哈希再次变化，§12.9 表中的 2026-11-17 13:0x 快照仅作历史记录；最新哈希以 `evidence/cursor-overflow/F-22|F-24/after.sha256` 为准。本 errata 除 F-22/F-23/F-24 条款外不改其它判据语义。
