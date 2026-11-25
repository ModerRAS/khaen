# 需求文档

状态：**已定稿（2026-11-16）**。技术方案见同目录 `design.md`。

## 介绍

做一个通用的频响优化器：输入是测得的频响、一条目标曲线、各通道的工作频带和可选的多个测点，输出是契约允许的双二阶校正。程序不内置任何一间房间的模态频率、尺寸、话筒型号或历史 EQ。

某次房间测量只可作为调用时的输入文件，或开发时的可选回归样本。验收用合成频响，频点和 Q 由夹具给出，不与某一间房的模态表对照。

优化器把校正做在生产者侧。校准文件只增加消费方能执行的滤波器。目标曲线写入报告，不写入校准文件的应用字段。

## 已确认

1. `fit` / `merge` 写出的滤波器用来逼近目标曲线：叠到实测上之后靠近目标。不再保留「让滤波器模仿实测形状」的旧行为。现有把模仿实测锁进断言的测试要改成断言校正方向。
2. 缺省目标曲线是 `flat`。其他曲线由文件提供，调用时显式选择。
3. 模态段只切。宽带段默认也不许正增益；`--boost-cap-db` 显式打开后，正向增益总和不超过所给值，且配置值最大为 +3 dB。
4. 缺省使用调用者给出的测点。只有一个测点时照常出滤波器，模态置信度标 `low`。不把某一路近场麦写进算法。
5. 模态 Q 按 −3 dB 带宽估计，不夹到现行契约的 10。窄模态是模态均衡要处理的对象。带宽窄于当前频响网格能分辨的宽度时，Q 记为网格所能支持的最细值，并在报告中标 `q_unresolved`。这是测量分辨率，不是房间策略。
6. 契约升到 `khaen-calibration/v1.1`：保留 Peaking，增加 `low_shelf` 与 `high_shelf`；Q 上限按本需求放宽。现行校验器与 channelmap 的 `PeqBand`（只有 freq/q/gain_db，且 Q 必须在 0.1–10）会拒绝这些结果，消费方必须同步放宽。
7. 分频频率由调用参数或该通道的 HP/LP 给出。分频点附近允许放模态段；该 pair 的复数响应复检失败时，回退引起失败的段并写入报告。
8. 不把某一份旧 EQ 导出当作验收反例。
9. 校验、pair 复检和响应对拍都在 `khaen-calibrate` 里用 Rust 完成。本功能不新增、不调用 Python。

## 已核对的程序现状

这些事实约束「通用程序要改哪里」，不构成房间参数。

| 项 | 现状 |
| --- | --- |
| 拟合目标 | `fit_peq` 让 ΣPEQ 去贴近实测。`merge` / `check_pair` 再把这些滤波器加到实测上。离线样例在 16 kHz 约 −6 dB，写出的增益是 −5.94 dB，加上去会把跌落加深。 |
| 平滑 | 拟合前有 7 点滑动平均，不是倍频程平滑。 |
| 网格 | `fr_from_h` 写死 300 点，约 30 点/倍频程。第三个参数是 FFT 长度。50 Hz 附近点距约 1.2 Hz；把相距约 0.13 oct 的两峰糊在一起的是这 7 点平均。 |
| 现行契约 | Peaking；Q 0.1–10；单段 gain ±24 dB；≤32 段。未知 `type` 整份拒绝。目标曲线与 FIR 不是校准文件的内容。 |
| pair | `channel_h` 加了 PEQ 幅度，没加 biquad 相位。 |
| 分频来源 | upmix 预设和 `--crossovers` 已能提供每通道 HP/LP。优化器读这些数，不在代码里写死一套客厅分频。 |

## 需求

### 需求 1 - 校正

**用户故事：** 作为使用者，我给任何通道的实测频响和一条目标曲线，希望得到叠上去之后靠近目标的滤波器。

#### 验收标准

1. When 优化器生成滤波器时，khaen-calibrate shall 最小化 `w(f)·(FR(f) + ΣH(f) − target(f))`。`FR` 与 `target` 都是 1 kHz 归一化后的幅度形状，`H` 是各段双二阶的幅度。
2. When 实测某处低于目标、且该频带不允许正向增益时，khaen-calibrate shall 留下残差并写入报告，shall 不放出会把该处再拉低的段。
3. When 调用 `fit` 或 `merge` 时，khaen-calibrate shall 使用校正目标，包括未额外指定优化模式的调用。程序 shall 不再提供让 ΣH 去模仿实测的拟合模式。

### 需求 2 - 契约 v1.1

**用户故事：** 作为消费方，我能执行优化器写出的 Peaking 与 shelf，并拒绝自己不认识的类型。

#### 验收标准

1. When 写校准文件时，khaen-calibrate shall 使用 schema `khaen-calibration/v1.1`，滤波器 `type` 仅为 `peaking`、`low_shelf`、`high_shelf`。频率升序。缺省 `type` 仍是 `peaking`，以便只含 Peaking 的文件能被明确为 v1.1 的子集。
2. When 校验 v1.1 文件时，校验器 shall 接受有限的 Q ≥ 0.1，shall 拒绝非有限 Q、非有限增益，以及未知 `type`。单段 gain 仍为 ±24 dB，每通道仍 ≤32 段，`gain_db` trim 仍为 −12..+6。
3. When 同一份文件只含 Peaking 且每段 Q ≤ 10 时，校验器 shall 同时接受它作为 v1 消费方可读的子集；含 shelf 或 Q > 10 的文件 shall 被未升级的 v1 消费方整份拒绝。
4. When 使用目标曲线时，khaen-calibrate shall 把曲线 id 与点表写入报告，不写入校准文件的应用字段。
5. The khaen-calibrate shall 使模态段与宽带段合并后每通道不超过 22 段。

### 需求 3 - 频响与测点

**用户故事：** 作为使用者，我可以加密频响网格，并给测点起名、加权；不加密时旧的 300 点频响保持不变。

#### 验收标准

1. When 指定 `--fr-points N` 或 `--fr-oct 1/48` 时，khaen-calibrate shall 按该对数网格输出频响。缺省维持 300 点。
2. When 做模态检测时，khaen-calibrate shall 另写更密的模态频响，分辨率不低于 1/48 oct，并用 1/24 oct 平滑。模态检测 shall 不使用按点数计的滑动平均。
3. When 测量带有 `--position <name>` 时，khaen-calibrate shall 把结果放在 `<run>/positions/<name>/`，并记录名称与可选的权重。缺省名称是 `listening`，权重是 1。
4. While 测点不少于 2 个，when 做空间平均时，khaen-calibrate shall 按权重平均；模态用幅度平均，宽带用功率平均后再开方。某一频点上测点间偏差大于 6 dB 时，该频点在模态检测中降权。
5. When 200 Hz–8 kHz 的 SNR 低于 25 dB，或扫频峰值高于 −1 dBFS 时，khaen-calibrate shall 将该通道或该测点标为 `invalid` 并排除出拟合。

### 需求 4 - 目标曲线

**用户故事：** 作为使用者，我用一条与房间无关的形状曲线当目标，默认是平直。

#### 验收标准

1. When 加载 schema `khaen-target-curve/v1` 的曲线文件时，khaen-calibrate shall 在对数频率上对 dB 线性插值，端点外保持端值，并拒绝频率非递增或 dB 非有限的曲线。
2. When 传入 `--target flat|<id>|@file` 时，khaen-calibrate shall 使用对应曲线。缺省是 `flat`。
3. The khaen-calibrate shall 随程序提供 `flat`。其他命名曲线只是示例数据，不改变缺省，也不代表某间房的听感。
4. While 目标在低频需要整体抬升或在高频需要整体倾斜，when 宽带拟合使用 shelf 更省段时，khaen-calibrate shall 可以用 `low_shelf` / `high_shelf` 表达这段形状，其余仍用 Peaking。

### 需求 5 - 模态均衡

**用户故事：** 作为使用者，我希望程序在我给出的低频工作带里只削峰，Q 跟实测带宽走，不填谷。

#### 验收标准

1. When 检测峰时，khaen-calibrate shall 在 1/24 oct 平滑后的频响上计算突出度：峰值减去 ±1/3 oct 内左右两侧最低点里较高的那个。突出度低于 3 dB 的峰不生成滤波器。谷不生成滤波器。邻域中位数会把宽峰自己的肩算进本底，Q≈4 的峰再高也过不了 3 dB，因此不用中位数。
2. While 测点不少于 2 个，when 某峰在加权平均上的突出度低于 2 dB，或同向测点少于 2 个时，khaen-calibrate shall 跳过该峰。
3. When 估计 Q 时，khaen-calibrate shall 用 −3 dB 带宽反算，下限为 0.1。不把 Q 夹到 10。带宽窄于频响网格分辨率时 shall 标 `q_unresolved`，并采用该网格能表示的最细 Q。
4. When 计算模态增益时，khaen-calibrate shall 只使用负增益：`g = −min(max_cut, prominence − target_margin)`，默认 `max_cut=12` dB、`target_margin=1` dB。
5. When 确定某通道的模态搜索带时，khaen-calibrate shall 取调用者给出的模态搜索范围（缺省 20–250 Hz）与该通道工作频带的交集。工作频带来自该通道 HP/LP 或显式 `--band`。高通高于模态搜索范围的通道不做模态段。
6. When 两个峰相距小于 1/6 oct 时，khaen-calibrate shall 合并为一段，保留更深的增益与更高的 Q。每通道模态段不超过 10。
7. When 某段使任一已测位置在模态带内的峰高出目标 3 dB 以上，或使某位置相对修正前变差超过 3 dB 时，khaen-calibrate shall 降低该段增益或跳过，并写入报告。
8. When 一段模态滤波器使任一已声明的 pair 在复数响应上不再满足 ACCEPTANCE §6.1 时，khaen-calibrate shall 回退该段。没有声明 pair 时不做这项回退。

### 需求 6 - 宽带优化

**用户故事：** 作为使用者，我希望在通道的工作频带内用低 Q 段和必要的 shelf 对齐目标，同类声道保持接近。

#### 验收标准

1. When 做宽带拟合时，khaen-calibrate shall 使用 1/6 oct 平滑后的空间平均频响。频带为调用者给出的宽带范围（缺省 80 Hz–16 kHz）与该通道工作频带的交集。高通以下不放段。
2. When 搜索 Peaking 时，khaen-calibrate shall 使用 Q 0.3–2.0、频率网格不粗于 1/12 oct，并逐步加入加权误差下降最大的候选。
3. When 宽带残差 RMS 低于 0.5 dB，或宽带段数达到 12，或下一步增益绝对值低于 0.2 dB 时，khaen-calibrate shall 停止该通道的宽带搜索。
4. When 限制增益时，khaen-calibrate shall 使单段增益绝对值不超过 6 dB。正向增益总和不超过 `--boost-cap-db`。该参数缺省为 0；调用者给出的值若大于 +3 dB，shall 按 +3 dB 执行并写入 warning。
5. When 候选与已采纳的模态段相距小于 1/6 oct 时，khaen-calibrate shall 不再在该处放宽带段。
6. When 调用者把若干通道标成同一匹配组，且组内 1/3 oct 预测差超过 3 dB 时，khaen-calibrate shall 用联合权重重拟合一次；仍超差则 warning，并保留权重最高的测点优先的结果。未标匹配组时不做左右绑定。

### 需求 7 - 预测与 pair

**用户故事：** 作为验收者，我用解析双二阶级联复算预测，pair 检查能看到滤波器的幅度和相位。

#### 验收标准

1. When 拟合完成时，khaen-calibrate shall 写出预测表，至少包含频率、目标、预测、逐测点预测与残差。
2. When 写报告时，khaen-calibrate shall 记录 mode、target、模态段数、宽带段数、预测 RMS、预测最大偏差与 warnings。
3. When 合并两级滤波器时，khaen-calibrate shall 按频率升序写入，重算预测，总段数不超过 22。
4. When pair 检查纳入这些滤波器时，khaen-calibrate shall 使用双二阶的复数响应。只加幅度的现有实现不能作为通过依据。
5. When 与 CamillaDSP 文件后端比较时，解析幅度与渲染幅度在拟合网格上的差 shall ≤ 0.1 dB。

### 需求 8 - 验收数据

**用户故事：** 作为开发者，我用与具体房间无关的合成频响证明检出、只切、目标曲线和契约。

#### 验收标准

1. When 合成频响含 8 个已知模态、一条宽带倾斜和噪声，且其中至少两个模态的真实 Q 大于 10 时，khaen-calibrate shall 使检出频率误差在 ±3 Hz 内、Q 误差在 ±30% 内，模态增益全部 ≤ 0，并且不把 Q>10 的模态夹成 10。
2. When 对同一合成响应用 `flat` 与一份非平直曲线各拟合一次时，1/6 oct 残差 RMS shall ≤ 1 dB，段数不超过预算。
3. When 输入含深谷且正向增益上限为 0 时，khaen-calibrate shall 不输出填谷的正增益段。
4. When 用文件后端复测优化结果时，1/3 oct 相对目标的 ripple shall ≤ 3 dB，模态带内的峰 shall ≤ 目标 +3 dB，且无新增超过 6 dB 的窄带缺口。多测点时，各测点相对修正前的偏差恶化不超过 1 dB。

## 非目标

- 把某一间房的模态表、话筒位置或旧 EQ 写进算法或验收标准。
- 设计分频器。各通道 HP/LP 已在 channelmap / upmix 预设里。本需求只读取这些频率来划工作频带，并在 pair 复检失败时回退滤波器。
- 类 Dirac ART 的 MIMO。执行引擎在 `audio/tools/mimo-engine`（板端已测实时性），每条路径的 FIR 字段草案在 channelmap v2.1。本需求不计算输入×输出的 FIR 矩阵。
- 在测量质量任务完成之前，用某一次实板扫频宣称优化器已对某房间生效。合成夹具不依赖该测量任务。
