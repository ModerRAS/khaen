# TSPI RT-first DSP core/host 接口与 RT 禁止行为规范（core-v1 / v1.1.3）

| 项 | 内容 |
|---|---|
| 文档 | projects/tspi-audio-dsp/rt-stack/spec/core-v1.md |
| 版本 | **1.1.3（2026-11-17，errata E1/E2/E3/E4：E3 把 I01 delay_q16 回退为 RESERVED-0（以实现为准），E4 统一 plan flags 位表 bit5..7=0；ABI size 不变、139 项检查表不变）；v1.1.3 errata E5（2026-11-18，P3 收口，N-09）：`rt-host/src/swap.rs` 的 `PLAN_FLAG_MASK` 已 0x1F，删除文档「0x3F 待对齐」旧述，不改 ABI** |
| 任务 | WBS T02（api-designer）；本文件是 T02 的唯一写域产物 |
| 状态 | P0 设计规范 v1.1.3 DESIGN-REVIEW（T02-review 15 项 CLOSED；T02-rereview N1–N8 CLOSED；**errata E1/E2 已按 T105 实现对齐；E3/E4 已按 Cursor 终审 F-10/F-11 以实现为准回退/统一**；独立 targeted 复审 PASS（task-80 后仍待签）；**E5 文档对齐：swap 掩码 0x1F 已按 task-207/E4 收窄，0x3F 仅为历史快照**）；未实现代码/未编译/未跑 cargo/未上板；代码级验证延后 T06/T16/T20/T21/T26，不冒充已测；证据 evidence/T02/ |
| 上游权威 | WBS T02 行与 §1/§7；DSP-HOST-DECISION §0/§1/§3/§4/§5/§6/§7/§8；DSP-HOST-CPU-BUDGET §2/§4/§5/§10；calibration-spec/CONTRACT.md v1/v1.1；audio/overlay/etc/tspi-audio/layout-presets.yml；web/docs/API.md |
| 下游消费者 | T06（crate stub/lint/rt-check）、T07/T08/T09（direct/partitioned/workers）、T12/T23（compile/nodes）、T13/T25/T57（compat/roles/channel_map）、T16（spsc）、T17（alsa）、T18（admission）、T19（clock）、T20（swap）、T21（fault）、T22（format）、T24/T42/T44/T60/T61/T62/T66/T70（control/calibration/Web）、T26（主机集成）、T45/T47/T56（调度/bench/plan）、T14（file runner） |
| 证据 | 附录 A manifest；evidence/T02/manifest.json（17 inputs/hash、命令、exit code、post-fill 数字、未测项）；evidence/T02/{abi_vectors.py,check_core_spec.py} 为**文件外**可执行自检（I10；checker 已按 N7 收紧） |

> 规范性用语：**必须**（MUST）、**禁止**（MUST NOT）、**应当**（SHOULD）、**可**（MAY）。源码/结构体/CLI 在本文件写作时全部是 **future/拟建**；T06 实现前不存在，本文只固定契约，不声称已实现或已测试。

## 0. 一页摘要（T06/T26 的最小必读）

本文件固定六件事，并把“RT 路径不会执行任何析构器”作为可复核的硬条件：

1. **Block**：B ∈ {64,128} 帧、f32 planar、单 epoch 内帧序号连续；SPSC 固定容量；块带 `epoch_id`/`frame_index`/`valid_frames`/`plan_version`；过期整块丢弃，playback 永不读半块。
2. **状态**：`HostState`（Init→Admitted→Preparing→Prepared→Starting→Running→Draining→SafetyMute/Faulted→Stopped）、`EpochLifecycle`、`PlanLifecycle`（Built→Validated→Admitted→Staged→Committed→Superseded→Retired，外加 last-good 回滚）、`DeviceOwner`（Playback/Calibration 互斥）、`SafetyMode`（Normal/ProtectionFallback/SilenceMute）全部有迁移表与事件。
3. **Epoch**：`epoch_id: u64` 进程内单调、永不复用；所有 discontinuity（启动、rate/B/N/O 不兼容变化、clock 失锁、设备重开、不可恢复 xrun、2 核重准入）必须新建 epoch，禁止跨 epoch 拼接历史。
4. **Event**：固定 64 B 记录、1024 槽有界环；RT 只做序号递增+按值拷贝；kind/severity/payload 语义固定；丢事件只增 `dropped` 计数，RT 不格式化、不打印。
5. **Worker-ack**：4 个 DSP worker 的 phase 完成位图（B1/B2/B3），master=worker0；有界 spin+cutoff；任一 worker 迟到→整块 invalid→迟到结果必须按 epoch/version 丢弃；旧 plan 退役必须 4-worker all-ack+旧版本块终态；quarantine 槽 all-ack 前禁止复用。
6. **RT 错误 / ABI / 禁止行为**：`RtStatus(u32)` 固定错误码与动作表；`repr(C)` v1 结构布局（偏移/大小/对齐/版本/哈希/端序/stride）固定；DSP 纯 worker 区、I/O 区、控制区三区允许/禁止表固定；**无 RT 析构**由 §13 类型清单+D-01..D-20 清单+INV 生命周期不变量覆盖；代码级证据显式延后到 T06 lint、T20 swap、T21 fault、T26 集成（标 DEFERRED-CODE）。

三个先写死的边界：

- **本规范不定义 DSP 数学**：direct/partitioned/FFT 算法与 f64 参考在 T03/T07/T08；本文只固定 block 语义、内存所有权、worker-ack、精度/确定性约束。FIR 家族与 target 导入在 T04；REST/WS/YAML/AoIP 兼容在 T05。
- **本规范不实现上混/下混**：运行时只允许 A_map 的“选择/零填/校验/scatter”；禁止任何节目上混、下混、矩阵造声道、自动内容上混；`stereo_strict=true` 默认下 FL/FR 之外的输出必须显式零填，`modal_support_to_other_outputs=false`。若未来启用纯 FL/FR 节目+低带跨箱支援，必须独立 profile、独立求解/应用/holdout，不能靠 runtime mask 已有 G。
- **本规范不做 f64 运行时**：runtime core 只接受 f32 plan（`precision=0`）；f64 只在 reference/设计器离线侧；ABI 保留 `precision` 字段，但 v1 admission 拒绝非 0。

### 0.1 v1.1 修订摘要（逐条对应 T02-review I01–I15）

| 评审 ID | 严重度 | v1.1 处置 | 落点 |
|---|---|---|---|
| I01 分数/半整数 D 无 ABI 映射 | P1 | **NOT-LANDED（v1.1.3 errata E3，以实现为准）**：ABI v1 的 Path/Diag/Role 无 `delay_q16` 槽（偏移 28/28/24 为 `reserved1`/`reserved1`/`reserved3`，必须 0，与 `abi.rs` 一致）；v1.1 原 FIXED 处置作废。分数/半整数 D 只能由 T12 在打包前折入 P/A 系数或取整（T66 GAP-T66-2 对非零 q16 返回 `E_CA_DELAY_Q16_ABI`）；`total_delay_q16` 仅保留为内部/报告口径（ABI 下低 16 位恒 0）。未来在 ABI 承载分数 D 必须升 ABI 版本 | §3.4、§11.10（errata E3）、§12.3、§15.1、A.7 |
| I02 RT 共享结构无 ABI | P1 | **FIXED**：新增 §11.13 `TspiRtControlV1/TspiSwapAckV1/TspiEventRingLaneV1/TspiWorkerRegsV1/TspiMasterRegsV1/TspiCrossfadeBudgetV1/TspiWorkerAffinityV1`（repr(C,align64)、偏移/原子语义/needs_drop=false）；§13.2 清单补齐；B-13..B-19 静态断言 | §11.13、§11.12、§12.3、§13.2 |
| I03 2 核 ack 与 0xF 矛盾 | P1 | **FIXED**：ACK 掩码参数化 `ack_mask(expected)=(1u32<<expected)-1`；0xF 仅 expected=4；§7.3 真值表/§11.6 按 expected；Q-03 关闭（2 核=独立 plan/epoch，expected=2，位 0..1） | §3.4、§7.3、§11.6、§15.2 |
| I04 tick 预算矛盾 | P1 | **FIXED**：`TICK_READS_PER_BLOCK_MAX=3`（B1 进入 1；spin 耗尽后 hard_cutoff 判定可选 1；B3 退出 1）；无第三次则不读；compute_ticks=t_exit−t_entry；§6.4/§7.3/§11.13 一致 | §3.4、§6.4、§7.3 |
| I05 block 启动/唤醒原语缺失 | P1 | **FIXED**：`RtControl.block_start_seq`（AtomicU64）+ block_start_epoch/plan_version/frame 字段；master 先写字段后 Release fetch_add(seq)，worker Acquire 等 seq 变化；spin_max_iters/cutoff_drain_iters 入 WorkerRegs/AdmissionReport；§7.7 伪代码同步 | §6.4、§7.3、§11.13、§12.3 |
| I06 crossfade 双图预算 | P2 | **FIXED**：`TspiCrossfadeBudgetV1`（双图/双历史字节、D_total_q16、峰值 CPU/RAM、可用量、接受位）；只允许同总 D；不足则 `ERR_CROSSFADE_BUDGET` 拒绝并硬切/静音；T20 测试范围写明 | §5.3、§7.4、§9.2、§11.13 |
| I07 状态机闭合缺口 | P2 | **FIXED**：定义 `PlanAborted`/`EpochAborted`；补 SafetyMute/Draining→Faulted、Starting 事件 `EV_STARTING`；事件幂等/去重规则；capture-full slot 中止路径 | §5.1–§5.3、§6.5、§8.2–§8.3 |
| I08 ack 槽耗尽/旧槽检测 | P2 | **FIXED**：ack 槽 4 个带 (epoch,block_seq,plan_version) 标签；槽不可用即不得启动 block→`RT_E_ACK_SLOT_EXHAUSTED`→SafetyMute；迟到 worker 先比对标签，不写坏当前槽；quarantine/ack 失联终态写死 | §7.3、§7.6、§9.2 |
| I09 D/F 可自动检查过度声明 | P2 | **FIXED**：D/F 每行加规则 ID+工具/测试+失败判据+豁免；D-20 fixture 扩到 8 类；新增 §13.7 审计证据模板与豁免流程；如实标注“仅审查”项 | §10.2、§10.3、§13.5、§13.6、§13.7 |
| I10 “118 项自测”不可证伪 | P2 | **FIXED**：自检脚本移出正文至 `evidence/T02/check_core_spec.py`（按章节/表头结构化解析，断言状态枚举/编号连续/证据列非空/数量与声明一致，且脚本删除或内容缺失时 FAIL）；ABI/timing 复算移入 `evidence/T02/abi_vectors.py`；C.3/A.5 记录 post-fill 实测 | 附录 A/B/C、evidence/T02 |
| I11 ALSA period→core B 未闭环 | P2 | **FIXED**：B 来源链固定为 ALSA actual period；period_frames ∈{64,128} 否则 `ERR_DEVICE_PARAMS`；T05/REST 必须显式返回不支持，不静默夹取；T17 重开/新 epoch 规则写死 | §6.1、§11.9、§9.2、§15.2 |
| I12 输入 hash 漂移/A.7 结论过期 | P2 | **FIXED**：A.2/A.7 按当前文件重算 hash（API.md/MIMO/T04/T05/CONTRACT/ACCEPTANCE）；重做 T02↔T03/T04/T05 交叉核对：block256 由 Q-11 决议、半整数 D 由折入系数解决（ABI v1 无 delay_q16，E3）、T04 新 fs/2 与 T05 快照分歧登记 | §15.2、A.2、A.7 |
| I13 block 256 三方不一致 | P2 | **FIXED**：Q-11 决议：runtime 仅 {64,128}；T03/T04 的 256 是参考/测试口径；T05 REST 对非 64/128 显式 400/unsupported；T17 actual period 不匹配→`ERR_DEVICE_PARAMS` | §3.2、§9.2、§15.2、A.7 |
| I14 命令/证据小错 | P3 | **FIXED**：A.4 CMD-ABI 指向 B.5；CMD-SCOPE 用真实输出；A.5/C.3 补 post-fill 行数/字节/hash 与 exit code | A.4、A.5、C.3 |
| I15 T45 接口覆盖不足 | P2 | **FIXED**：新增 `TspiWorkerAffinityV1` 与 PlanHeader affinity 表；AdmissionEnv/Report 增 cpu_set_mask/actual_cpu[4]/priority[4]/irq_dependency_mask/expected_workers/ack_mask/crossfade 字段；T45 只消费不自动写内核 | §11.10、§11.13、§12.3、§15.1 |

> P1 5/5、P2 9/9、P3 1/1 = **15/15 FIXED，0 DECLINED**（v1.1 历史处置；**I01 经 v1.1.3 errata E3 回退为 NOT-LANDED**，见下）；复审由独立 reviewer 做（作者不自审）。v1.1 只增补/澄清，不删除 v1 语义；ABI 大小不变（reserved 字段复用），ABI_VERSION 维持 1（additive），若 T06 需要真正 breaking 变更必须升 1.1 规范版本并重新冻结。


**v1.1.1 二修（2026-11-17，按 T02-rereview；不重开原 15 项）**：

| ID | 处置 | 落点 |
|---|---|---|
| N1 | **FIXED**：T04 v1.2-draft 已落 Lead fs/2 裁决（magnitude-only ≤fs/2；带 phase <fs/2，=fs/2→E_TGT_PHASE_REAL；>fs/2→E_TGT_FREQ_RANGE）；Q-12 标 RESOLVED，A.2 T04 hash 更新 86fb97fd（97,033 B），A.7 重写 | §15.2 Q-12、A.2 #14、A.7 |
| N2 | **CLOSED**：T03 SPEC:148 已改“参考/测试候选 64/128/256；runtime 仅 {64,128}”；**T04 v1.2 errata A 已改 §7.3/§7.1 措辞**（生产候选仅 64/128，256 仅参考/测试附加块长）；原 A.7 旧句已删 | §3.2/Q-11、A.7 |
| N3 | **FIXED**：新增 T02→T03 oracle 映射规则——T12/T15 在 T03 reference 对拍前必须把全部分数/非整数 D 折入 P/A 系数（对拍图 q16=0；ABI v1 无 `delay_q16` 槽，E3）或声明 T03 schema 扩展；T03:70 已注 reference 只验证整数 D | §15.1 T12/T15、A.7 |
| N4 | **FIXED**：§11.13 intro 改为“除 TspiWorkerAffinityV1（32B/align8，plan 内 POD 表项）外均 align64”；B-18/B.4 标注 align=8 例外 | §11.13、§11.12 B-18、B.4 |
| N5 | **FIXED**：§3.4 WORKER_BITS 行参数化：“仅 expected_workers=4 时 all-ack=0xF；通用 ACK_MASK(expected)”；expected=2 时 0x3 | §3.4 |
| N6 | **FIXED**：A.2 补 T01 baseline/T73 当前 hash 并更新 T03/T04/T05 hash（17 输入）；A.7 增 T73/T01 行与 T05 现状 | A.2、A.7 |
| N7 | **FIXED**：check_core_spec.py 的 v1_1_fixed 改为“行含 FIXED 且落点列含 §/evidence”，并新增 N1–N6 回归断言（Q-12 RESOLVED/T04 hash/work值的参数化/align 例外/T03 折入/T73） | evidence/T02/check_core_spec.py |
| N8 | **CLOSED**：T04 v1.2 errata A 已修 §7.3/§45 的 64/128/256 措辞（生产候选仅 64/128；256 参考/测试口径）；T02 runtime 仍只 {64,128}（§3.2/Q-11） | A.7、§3.2 |

**v1.1.1 关闭：N1–N8 全部 CLOSED（8/8）；原 15/15 CLOSED；0 DECLINED；139 项检查表/ABI 不变。**

**v1.1.3 errata E3/E4（2026-11-17，Cursor 终审 F-10/F-11；以实现为准）**：

| ID | 处置 | 落点 |
|---|---|---|
| E3 | **FIXED（规格回退一致；F-10 P0）**：ABI v1 的 `TspiPathV1`/`TspiDiagFirV1` 偏移 28、`TspiRoleV1` 偏移 24 保持 `reserved1`/`reserved1`/`reserved3`（必须 0），删除 v1.1 误写的 `delay_q16` ABI 字段名；I01 回退为 **NOT-LANDED**；分数/半整数 D 由 T12 在打包前折入 P/A 系数（T66 GAP-T66-2 按 `E_CA_DELAY_Q16_ABI` 拒收非零 q16）；`total_delay_q16` 仅内部/报告口径（ABI 下低 16 位恒 0，crossfade D 对齐等价整数 D 相等）。**§11.10/B.4 不得再写 delay_q16 ABI 槽。** | §0.1 I01/I12、§3.4、§5.3、§7.4、§9.2、§11.10、§11.12 B-07、§11.13 CrossfadeBudget 注、§12.3、§14.2 A-21、§15.1 T12/T15、A.7 N3、B.3、B.4 |
| E4 | **FIXED（位表统一；F-11 P1）**：plan flags 统一为 bit0 stereo_strict、bit1 modal_support_to_other_outputs、bit2 crossfade、bit3 extended_taps_admitted、bit4 fallback_graph、**bit5..7=0（reserved）**；v1.1 的 bit5 affinity 表存在位未在实现消费（`abi.rs` 结构注释 bit5..7=0），取消该位义；affinity 表存在性改按 `affinity_table_offset != 0 / affinity_count > 0` 判定。`rt-host/src/swap.rs` 的 `PLAN_FLAG_MASK` 已在 task-207 **收窄为 0x1F**（bit0..bit4；bit5..7 置位即 `ERR_BAD_LAYOUT`，T20 `t20_admit_plan_flags_e4_rejects_bits5_to_7` 钉死），与 `abi.rs`/T66 一致；**0x3F 仅为 task-207 之前的历史快照**（v1.1.3 errata E5 收口，见 evidence/T02/ERRATA-v1.1.4.md）。 | §11.10 flags 表/语义/表顺序规则 3 |

> v1.1.3 只做 E3/E4 两处 errata + 引用条目；其余语义、ABI size/offset/align、139 项检查表不变。旧 core-v1 hash 引用待刷新清单见 `evidence/T02/ERRATA-v1.1.3.md`（本任务不改他人 evidence）。

**v1.1.3 errata E5（2026-11-18，v1.1 P3 收口；N-09；文档对齐，不改 ABI）**：

| ID | 处置 | 落点 |
|---|---|---|
| E5 | **FIXED（文档收口；N-09 P3）**：`rt-host/src/swap.rs` 的 `PLAN_FLAG_MASK` 已在 task-207 收窄为 **0x1F**（bit0..bit4；bit5..7 置位即 `ERR_BAD_LAYOUT`；T20 `t20_admit_plan_flags_e4_rejects_bits5_to_7` 钉死），与 `abi.rs`/T66 一致；本条删除 E4 行「0x3F 待对齐」旧述，**0x3F 仅作为 task-207 之前的历史快照**保留在 `evidence/T02/ERRATA-v1.1.3.md`；ABI size/offset/align、139 项检查表与全部语义不变。 | 本节 E4、§11.10、A.2.3、A.5、C.3、C.4；evidence/T02/ERRATA-v1.1.4.md、evidence/cursor-fix-v1-p3/ |

> E5 是纯文档 errata：实现已在 task-207（N-06）按 E4 收窄；本条目只把「当前事实」与实现/证据对齐，不重开语义、不改代码/ABI。

## 1. 范围、非目标与写域纪律

### 1.1 范围（本文件必须固定的内容）

| 编号 | 范围 | 本文位置 | 未来验证任务 |
|---|---|---|---|
| S1 | block 模型、SPSC 环、frame/epoch/valid_frames、deadline/gate/cutoff | §6 | T16 spsc、T17 alsa、T26 |
| S2 | host/epoch/plan/device-owner/safety 状态机与迁移事件 | §5 | T20 swap、T21 fault、T44 modes、T45 |
| S3 | worker-ack（B1–B3 位图、master、all-ack、迟到丢弃、quarantine） | §7 | T09 workers、T20、T21 |
| S4 | event/计数/有界直方图（RT 写入、非 RT 导出） | §8 | T21 fault、T47 bench |
| S5 | RT 错误码、错误→动作映射、no-panic/无分配策略 | §9 | T21、T26 |
| S6 | ABI v1：结构布局、版本/魔数、对齐/stride、plan hash、FFI 规则、端序 | §11 | T06 静态断言、T12/T13/T66 |
| S7 | RT 三区禁止行为总表 | §10 | T06 lint/rt-check、全部 RT crate review |
| S8 | 无 RT 析构专项（类型清单、生命周期不变量、证明骨架、评审清单） | §13 | T06/T20/T21/T26 |
| S9 | API 表面（crate/module/关键签名、`[RT]`/`[NONRT]`/`[IO]` 标注规则） | §12 | T06 stub、全部实现任务 |
| S10 | P0 人工复核记录（API/禁止行为/无析构/ABI 检查表与输入 hash） | §14、附录 A–C | T02 完成后独立复核；T06 对照 |

### 1.2 非目标（不规定、不得据此越权）

- 不规定 FIR 系数设计、七家族幅相/支撑、target CSV/sidecar 字段：T04 `spec/fir-toolbox.md`。
- 不规定 f64 参考算法、手算 impulse 向量与夹具：T03 `rt-stack/reference/`。
- 不规定 REST/WS 路径/字段/鉴权、YAML/校准文件字段、AoIP 端点协议：T05 `spec/compat-matrix.md` 与 T24/T42/T43/T60。
- 不规定 ALSA 具体 hwparams 数值、设备名、恢复命令：T17；不规定 admission 具体测量阈值：T18/T45。
- 不规定多核 speedup/deadline 实测门判读流程：T46/T30；不规定部署/服务/退役：T28/T71/T72。
- 不规定 CamillaDSP 兼容；开发期 oracle 不得进入本文 RT 路径。

### 1.3 写域与变更纪律（WBS §1/§7 摘录，本任务照做）

- 本任务唯一写域：`projects/tspi-audio-dsp/rt-stack/spec/core-v1.md`；本 agent 未创建 Cargo 根、未改任何生产代码/文档、未 git commit、未碰板/ALSA/CueMix/服务。
- 本文是 v1 契约：语义冻结；T06–T70 实现不得静默偏离。编辑性澄清可升 `1.0.x`（只改文字，不改字段/状态/常量）；任何字段语义、容量、状态迁移、错误码、ABI 布局变化必须升 `1.1` 并在 §15 记录，由 lead/integrator 裁决。
- 公共文件（Cargo.toml/Cargo.lock/lib.rs/mod.rs/CLI 注册/README/CI）只由 integrator 在 T06/T26/T32 写；本文只给拟建接口，不建文件。

## 2. 规范用语与术语

| 术语 | 定义（本文语义） |
|---|---|
| RT 区（DSP RT） | 4 个 DSP worker（含 master=worker0）在 block 处理中执行的代码；FIFO 70；禁止行为见 §10。 |
| I/O 区 | capture/playback 两个 ALSA owner 线程；FIFO 80；允许且预算 read/write/poll/mmap/恢复 syscall 与格式转换。 |
| 控制区（CTL） | 所有非 RT 线程：prepare/compile/load、REST/WS、校准 job、metrics 导出、retire/drop；SCHED_OTHER；可分配可阻塞，但不得写 RT 私有状态。 |
| watchdog 区 | 独立核上的监测线程；只读 heartbeat/epoch 计数、只置位安全请求；禁止释放/创建 RT 内存。 |
| block | 一个 DSP 处理单位：B 帧 × N 输入 × O 输出（planar f32）；身份 =(epoch_id, block_seq, plan_version)。 |
| frame_index | 输入时间轴绝对帧号（u64）；epoch 内从 0 起连续；frame_index = block_seq × B（drain 尾块也按整帧对齐）。 |
| epoch | 一次连续流的时间轴容器；rate/B/N/O/时钟连续性/不兼容 plan 任一改变必须新建 epoch。 |
| plan / graph | 已编译、已校验、已准入的不可变执行图（路径系数、对角 FIR、节点参数、tile 分配、预算）；RT 只读。 |
| plan_version | 进程内单调 u32，每次成功 commit 的 plan 递增；不是文件版本号，不跨进程复用。 |
| graph_id | plan 的规范化 SHA-256（32 B），绑定系数/图/role/rate/B/D；用于 stale/hash 校验与 REST/WS 实际 ack。 |
| A_map | O×N 的 input→output 映射：只做选择/零填/校验，禁止隐式混音。 |
| G | 低带 M×M 矩阵嵌入（M≤8）；G = deltaD·I + g，含对角基线。 |
| P_post | 每输出对角 FIR（目标/分频/phase/delay/polarity/保护）；P 与 G 不交换。 |
| worker-ack | 每个 block 每个 phase 的 4-worker 完成位图；以及 plan swap 的 all-ack。 |
| cutoff | 有界等待上限（admission 校准的 spin 预算+master 的 2 次预算内 tick 采样共同确定）；超过即整块 invalid。 |
| gate_ns | 验收门：完整 DSP 图 block 处理时间必须 < T_block/2（128@48k = 1.3333 ms）。 |
| hard_cutoff_ns | 安全截止：play_deadline_ns − io_tail_allowance_ns；超过即丢弃该块并输出预分配安全输出。 |
| quarantine | 被 cutoff 判 invalid 后仍可能有迟到 worker 写入的 buffer/partial/graph 隔离区；all-ack 前禁止复用或释放。 |
| declared load | 冻结的 profile（fs/B/N/O/plan hash/G/P/PEQ/保护/worker 数/taps）下的生产负载；不含未准入调试/扩展组合。 |
| safety output | 预先分配且已准入的安全输出：ProtectionFallback（G=I 且保留 P 最终保护）或 SilenceMute（全零）。 |
| last-good | 上一次 admission 通过并可回滚的自研 plan/二进制/配置；P1 失败回退只允许 last-good 或 mute，禁止回 CamillaDSP。 |

## 3. 常量、容量与时限基础

以下常量是 **v1 契约常量**：T06 必须以 `pub const` 固化并加编译期静态断言；实现不得在运行期改这些上界。所有 **容量类** 上界只表示“准入上限”，不表示任何组合都保证 RT；实际准入由 T18/T45/T56 按测量与预算判定。

### 3.1 通道 / 布局 / 容量

| 常量 | 值 | 说明（依据） |
|---|---|---|
| `MAX_INPUT_CHANNELS` | 20 | MOTU UltraLite-mk5 capture 上限 20；H16 16 TX/逻辑 12 容器；Toslink USB capture 20@48k 中实际 Optical 12/13。 |
| `MAX_OUTPUT_CHANNELS` | 22 | MOTU playback 上限 22（10 路模拟 + 其余补零）；AoIP 16 容器 ≤22。 |
| `MAX_INPUT_ROLES` | 16 | 输入布局 role 数上限（H16 逻辑 12/可能 16 容器）。 |
| `MAX_OUTPUT_ROLES` | 16 | 输出容器/布局 role 数上限（4.1.2=7、5.3.2=10、7.1.4=12、9.1.4=14；16 为硬上限）。 |
| `MAX_LOW_BAND_PARTICIPANTS` | 8 | 低带 MIMO 显式 participants 上限（P1=5：FL/FR/SL/SR/SUB1；CPU 预算表扩展到 8）。 |
| `MAX_MATRIX_PATHS` | 128 | 矩阵路径表容量（M×M≤64 的 2 倍余量，覆盖 16 输入×8 低带上界与未来扩展；P1 目标 25）。 |
| `MAX_DIAG_FIRS` | 22 | 每输出对角 FIR 上限=（=MAX_OUTPUT_CHANNELS）。 |
| `MAX_PLAN_NODES` | 256 | 图节点数上限（Mixer/FIR/IIR/Volume/Limiter/Scatter/保护）。 |

### 3.2 采样率 / 块 / 时限

| 常量 | 值 | 说明 |
|---|---|---|
| `SAMPLE_RATES_HZ` | {48000, 96000} | 部署口径 48k/96k（CONTRACT.md §5）；其它值 v1 admission 拒绝（ERR_RATE），44.1/88.2 不承诺。 |
| `BLOCK_SIZES` | {64, 128} | v1 runtime 准入只支持 64/128（生产 128；最终独立窗口 64）。T03:147/T04:45,682 的 256 是参考数学/设计测试口径，**不是 runtime 准入**；T05 REST 必须对非 64/128 返回 400/unsupported（Q-11 已决议，见 §15.2）。32/256/512/1024 在 v1 core 一律拒绝 `ERR_BLOCK_SIZE`。 |
| `BLOCK_DEFAULT` | 128 | declared load 默认。 |
| `GATE_NUM / GATE_DEN` | 1 / 2 | gate_ns = T_block/2；128@48k=1.3333 ms、64@48k=0.6667 ms；96k 按自身 T_block 缩放。 |
| `TBLOCK_128_48K_NS` | 2666667 | 2.666667 ms（128/48000，向上取整 ns；实现用有理数或 u64 纳秒近似时必须保证不放宽门）。 |
| `TBLOCK_64_48K_NS` | 1333333 | 1.333333 ms。 |
| `IO_TAIL_ALLOWANCE_NS_DEFAULT` | 100000 | hard_cutoff 前留给 playback/I/O 尾部的默认安全量（100 µs）；由 admission 校验必须 < 25% × T_block。 |
| `WATCHDOG_PERIOD_MS` | 10 | 非 RT watchdog 采样周期；只读 heartbeat，不做 RT 动作。 |
| `WATCHDOG_STALE_BLOCKS` | 4 | 连续 4 个 block 周期未见 heartbeat → 置安全 mute 请求（非 RT 执行）。 |

### 3.3 Taps / 分区 / worker / 队列 / 事件 / quarantine

| 常量 | 值 | 说明 |
|---|---|---|
| `TAPS_NORMAL_MAX` | 4096 | 常规 profile 每 route 有效支撑上限；固定群延迟 42.65625 ms@48k（线性相位），已超 15/25/40 全部附加预算，故不得承诺“全档低延迟”。 |
| `TAPS_EXTENDED_MAX` | 16384 | 8192/16384 为显式 extended profile，必须通过 RAM/最坏 deadline/时延/热切峰值准入。 |
| `TAPS_DESIGN_ONLY_ABOVE` | 16384 | 超过仅设计工具 offline 输出，runtime 准入拒绝（ERR_TAPS）。 |
| `FUSED_LEN_MAX` | L_G + L_P − 1 | 融合长度按加法计算（D 计支撑）；超 profile 上限则保留 factorized 图重新准入或 INFEASIBLE，禁止静默截尾。示例 4096+1024−1=5119。 |
| `MAX_PARTITIONS` | 256 | 每路径分区数上限 ceil(16384/64)=256（B=64, 16384 taps）；B=128 时 128。 |
| `FFT_SIZE` | 2 × B | 共享 FFT 长度：B=64→128、B=128→256。 |
| `RT_WORKERS` | 4 | 生产 4 worker；master=worker0。 |
| `RT_WORKERS_FALLBACK` | 2 | 2 核是独立 profile 重新准入，不是失核后继续半图。 |
| `RT_WORKERS_BENCH` | {1,2,4} | 仅 benchmark 比较；1/2 不授予生产资格。 |
| `PARALLEL_PHASES` | 3 | B1/B2/B3 三个 worker phase（见 §7）。 |
| `SPSC_CAPTURE_SLOTS` | 4 | capture→DSP 环固定 4 槽（不可无界）。 |
| `SPSC_PLAYBACK_SLOTS` | 4 | DSP→playback 环固定 4 槽。 |
| `QUARANTINE_SLOTS` | 8 | 隔离池固定 8 块；耗尽 → 拒绝新 commit/切图并进入已准入安全 mute，禁止无界分配。 |
| `EVENT_RING_SLOTS` | 1024 | 事件环固定 1024 × 64 B = 65536 B。 |
| `HIST_BUCKETS` | 32 | 每 worker 每 metric 固定 32 桶有界直方图（桶边界由非 RT prepare 写死）。 |
| `MAX_WORKER_STACK_BYTES` | 524288 | 每 RT 线程栈上限（512 KiB）由 admission 预创建并预触；超出即拒启。 |

### 3.4 worker-ack / 版本 / 哈希 / 计数上界

| 常量 | 值 | 说明 |
|---|---|---|
| `WORKER_BITS` | 4 | 位 0..3 对应 worker0..3；**仅 `expected_workers=4` 时 all-ack=0xF**；通用完成判据一律 `ACK_MASK(expected)=(1u32<<expected)-1`（expected=2→0x3）；禁止把 0xF 写成所有 profile 的 all-ack。 |
| `ACK_MASK_ALL` | 0xF | **仅当 `expected_workers=4` 时**等于全完成掩码；v1.1 起 ack 一律用参数化 `ACK_MASK(expected)=(1u32<<expected)-1`，禁止把 0xF 写成所有 profile 的常量。 |
| `EPOCH_ID_WRAP_BOUND` | u64 | 见附录 B：2^64 个 block@750 block/s ≈ 7.8 亿年，实际不可回绕；禁止降为 u32（u32 会在 66 天回绕）。 |
| `PLAN_VERSION_WRAP_BOUND` | u32 | 2^32 次 commit；实际每秒 <<1，不可回绕；回绕即 fault 并拒绝。 |
| `GRAPH_ID_BYTES` | 32 | SHA-256 原始 32 字节；不得截断到 64 bit 后仍称“完整 hash”。 |
| `ABI_VERSION` | 1 | ABI 结构/语义版本；不兼容改变必须 +1。 |
| `TSPI_PLAN_MAGIC` | 0x54535031 | ASCII "TSP1"（LE u32）。 |
| `TICK_READS_PER_BLOCK_MAX` | 3 | master 每 block 最多 3 次预算内 tick：**B1 进入 1 次**；**仅当 spin 耗尽后**为 hard_cutoff 判定可选 1 次（正常不读）；**B3 退出 1 次**。compute_ticks=t_exit−t_entry；§6.4/§7.3/§11.13 三处一致。纯 worker 0 次。 |
| `ACK_MASK(expected)` | (1 << expected) − 1 | expected∈{2,4}（4=生产，2=独立降级 profile）；位 k=worker k。真值表/完成判定必须按本式。 |
| `ACK_SLOTS` | 4 | 固定 ack 槽数（与 playback ring 对齐）；槽生命周期/耗尽策略见 §7.3/§7.6。 |
| `DELAY_Q16_SCALE` | 65536 | 次采样 Q16 刻度（内部 DelayPlan/`total_delay_q16` 口径）；`delay_q16` ∈ [0,65535]，残差 < 1/65536 sample；**ABI v1 无该槽（Path/Diag/Role reserved 必须 0，errata E3）**。 |
| `CROSSFADE_MAX_FRAMES` | 128 | 最大 crossfade 帧数（=B，且不得跨 epoch）；更长必须新 profile 并重新准入。 |
| `WORKER_AFFINITY_COUNT` | 4 | plan 内 affinity 表最多 4 条（2-worker profile 仅前 2 条有效）。 |

### 3.5 时限模型（固定公式）

~~~text
T_block(fs, B)      = B / fs                                   # 秒
gate_ns(fs, B)      = T_block * 1e9 / 2                        # 验收门：50% deadline
play_deadline_ns(b) = capture_publish_mono_ns(b) + latency_budget_ns
hard_cutoff_ns(b)   = play_deadline_ns(b) - io_tail_allowance_ns
block_compute_ns(b) = master_tick_exit(b) - master_tick_entry(b)  # 2 次预算内采样，非 RT 换算
~~~

| fs / B | T_block | gate_ns | 说明 |
|---|---:|---:|---|
| 48000 / 128 | 2.666667 ms | 1.333333 ms | 128 满图验收：worst < gate_ns 且 p99.9 同门、0 miss。 |
| 48000 / 64 | 1.333333 ms | 0.666667 ms | 64 独立最终窗：worst < gate_ns。 |
| 96000 / 128 | 1.333333 ms | 0.666667 ms | 96k 按自身 T_block 缩放，不沿用 48k 数字。 |
| 96000 / 64 | 0.666667 ms | 0.333333 ms | 同上。 |

**判定语义**：gate_ns 是验收统计门（允许继续流，但记录 `EV_DEADLINE_MISS` 与计数）；hard_cutoff_ns 是安全动作门（整块 invalid、输出安全输出、计数、进入 quarantine）。二者禁止混用；不得用 hard cutoff 掩盖 gate miss，也不得用平均 CPU/理论 GF 代替 worst。

## 4. 分层、线程、内存所有权与生命周期

### 4.1 线程集合（admission 前全部预创建，RT 内禁止创建线程）

| 线程 | 数量 | 调度 | 职责 | 允许行为 |
|---|---:|---|---|---|
| `dsp_worker[k]` k=0..3 | 4 | FIFO 70（初值） | block 处理、phase 完成、ack | 见 §10 DSP 区；master=worker0 额外 2 次预算 tick 采样。 |
| `capture` | 1 | FIFO 80（初值） | ALSA capture→format→planar→SPSC 发布+时间戳 | I/O 区允许且预算 syscall；不得进入 DSP 私有状态。 |
| `playback` | 1 | FIFO 80（初值） | SPSC 消费→反向 format→ALSA write/poll；欠载输出静音 | 同上。 |
| `watchdog` | 1 | 独立核 SCHED_OTHER | 读 heartbeat/epoch 计数、置安全请求 | 不得释放/创建 RT 内存、不得直接改 plan。 |
| `control` | ≥1 | SCHED_OTHER | REST/WS、prepare/commit、校准 job、metrics 导出、retire/drop | 可分配可阻塞；禁止写 RT 私有状态。 |
| `metrics`（可并入 watchdog/control） | 0..1 | SCHED_OTHER | 非 RT 读取计数/直方图/事件环并导出 | 只读快照；不得采样热路径 clock。 |

### 4.2 内存对象与所有权矩阵

| 对象 | 分配者 | 所有者（唯一） | 释放者 | 生命周期 |
|---|---|---|---|---|
| `PlanArena`（不可变系数/表/历史/FFT scratch/plan 元数据） | control（非 RT mmap/Box） | `PlanRegistry`（control） | control 的 retire 回收器 | 发布后只读；retire gate（§7.5）全过才 unmap。 |
| `RtArena`（可变：输入/输出 block、worker 私有 partial、quarantine 槽、ack 槽、事件环、统计） | control/admission 一次分配 | host 进程 | 进程退出 | 整个 RT 运行期地址稳定；禁止 realloc/munmap。 |
| `SpscRing`（slot 数组+头） | admission | 固定一对一（capture→worker master；master→playback） | 进程退出 | 固定容量；满/空行为见 §6.3。 |
| `EventRing` | admission | RT 写、control 读 | 进程退出 | 固定 1024 槽；满即丢并计数。 |
| `PlanRef`（RT 可见，仅 raw pointer+version） | master 从 `RtControl` 读取 | 无（借用，不拥有） | 不适用（禁止析构） | 只在当前 block 内有效；见 INV-L1。 |
| `RtLease`（worker 借用 plan 的凭证，Copy） | master/worker 本地 | 无 | 不适用（禁止析构） | block 边界后失效；旧 plan 未 all-ack 前不得 unmap。 |
| `last-good` 二进制/plan/配置 | control | control | control（仅 rollback 切换时新建/保留旧文件） | 与 RT 内存无关；P1 回退只允许它或 mute。 |

### 4.3 生命周期不变量（INV，强制）

| ID | 不变量（必须始终成立） | 违反后果 |
|---|---|---|
| INV-L1 | `RtControl.current_plan_ptr` 只在 block 边界由 master 读取一次并写入该 block 的 `plan_version`；非 RT 只能在 `switch_state=ARMED` 且 master all-ack 后替换指针。 | fault → SafetyMute。 |
| INV-L2 | 旧 plan 的 `PlanArena` 只有在 §7.5 retire gate 四条件全满足后才允许 control 线程 unmap/drop；RT 线程永不持有 `Arc`/`Box`/`PlanArena` 所有权。 | UAF 风险 → 设计禁止；评审拒绝。 |
| INV-L3 | `RtArena` 地址在运行期稳定；RT 内禁止 mmap/munmap/mremap/realloc/alloc/dealloc。 | fault → SafetyMute。 |
| INV-L4 | 同一 block slot 的写者唯一（capture 写 payload；worker 写 partial；master 写 output；playback 只读）；状态位以 Release/Acquire 发布。 | fault → SafetyMute。 |
| INV-L5 | quarantine 槽在其所有可能迟到 worker 的 ack/撤销前禁止复用、禁止 unmap、禁止作为新 block slot。 | 若无法 all-ack（worker 失联）：永不复用，安全 mute，等进程重启；不回收。 |
| INV-L6 | 每 slot 状态只能由下表指定 owner 在指定迁移边沿写；其他线程只读。 | fault → SafetyMute。 |
| INV-L7 | `epoch_id`、`plan_version`、`block_seq` 单调递增；回绕/倒退 = fault。 | fault → SafetyMute。 |
| INV-L8 | 禁止跨 epoch 复用历史/partial/output；新 epoch 前旧 epoch 必须 Draining→Retired 或隔离。 | fault → SafetyMute。 |
| INV-L9 | RT 调用图（§12 标 `[RT]` 的函数及其被调者）内禁止任何析构器（§13 D-01..D-20）。 | 评审拒绝 + T06 lint 失败。 |
| INV-L10 | 所有跨 RT 边界类型必须是 `repr(C)` POD/原子类型且 `needs_drop::<T>() == false`（§13.2 清单）；不得传 `Vec/String/Box/Arc/Result<_,E:Drop>`。 | 评审拒绝 + T06 静态断言失败。 |

### 4.4 Plan 借用与退役模型（文字证明骨架，详见 §13.4）

1. **Build/Validate（control）**：T12/T13/T66 产出 plan 字节；计算 `graph_id`；T06/T12 校验容量/版本/hash/精度/通道/rate/block。
2. **Admit（control，T18/T45 提交）**：按 declared load 预分配 `RtArena`、双图 slot、quarantine pool、ring、事件/直方图；mlockall 预触成功；失败则 **不启 RT 主模式**（保留 last-good 或 mute）。
3. **Publish（control→master）**：把新 `PlanArena` 指针写入 `RtControl.pending_ptr`，置 `switch_state=ARMED`；该 arena 由 control 的 `PlanRegistry` 持有（引用计数只增在 control）。
4. **Commit（master，block 边界）**：master 读 pending，更新 `current_plan_ptr` 与 `plan_version`，在下一 block 的 BlockDesc 写新 version；4 worker 在 B1 进入时读同一 version。
5. **Ack（worker→master→control）**：worker 在完成首个新版本 block 后写 `swap_ack[old_version].worker_bit`；master 在 all-ack 后置 `retire_ready`，并记录 `EV_PLAN_SWAPPED/EV_SWAP_ACK`。
6. **Retire（control）**：retire gate =（a）指针已切换；（b）4 worker 都已 ack 新版本（不再可能读旧指针）；（c）旧版本 block 全部 terminal（DONE/RECYCLED/QUARANTINED 且 all-ack）；（d）playback 环与 quarantine 中无旧 version 未终态。满足后才删除 `PlanRegistry` 的强引用并 unmap。**任何一步超时/失联 → 不释放，进入安全 mute + quarantine，等进程重启**（WBS/决策允许“保隔离内存不回收”）。

## 5. 状态机

### 5.1 HostState（进程/主机级）

| 状态 | 含义 | 允许进入的下一状态 | 进入动作/事件（每次进入**恰一次**） |
|---|---|---|---|
| `Init` | 启动、解析配置、无音频 owner | `Admitted` / `Stopped` | 设备 inventory（非 RT）；`EV_HOST_INIT`。 |
| `Admitted` | admission 通过：拓扑/RLIMIT/mlockall/预触/线程栈/CPU set/权限/clock 源全部 OK | `Preparing` / `Faulted` / `Stopped` | `EV_ADMISSION_OK`；失败保留 last-good 或 mute，不得 warn 后自称 RT。 |
| `Preparing` | control 编译/校验/hash/容量、加载 last-good 候选 | `Prepared` / `SafetyMute` / `Faulted` | `EV_PLAN_PREPARED`；非 RT 可分配。 |
| `Prepared` | plan 已 admit（容量/双图/ring/quarantine 预分配）但未启流 | `Starting` / `SafetyMute` / `Stopped` | `EV_PLAN_ADMITTED`。 |
| `Starting` | 打开设备、协商 actual hwparams、启动 capture/playback/workers、建立 epoch0 | `Running` / `Prepared`（启动失败回滚）/ `SafetyMute` / `Faulted` | `EV_STARTING`（kind=36）。 |
| `Running` | 正常处理；可能临时 ProtectionFallback | `Draining` / `SafetyMute` / `Faulted` | `EV_RUNNING`；epoch 可多次新建。 |
| `Draining` | 停新提交、drain ring、旧 epoch 收尾、等待 ack | `Prepared`（切图/重准入）/ `Stopped` / `SafetyMute` / `Faulted` | `EV_DRAINING`。 |
| `SafetyMute` | 安全静音：只输出预分配静音/ProtectionFallback；不处理新块 | `Prepared`（非 RT 完整重新 admission）/ `Stopped` / `Faulted` | `EV_SAFETY_MUTE_ENTER`；不得自动恢复 Running。 |
| `Faulted` | 不可恢复故障（worker 失联/ABI/不变量/内存/ack 槽耗尽）；保 quarantine 不回收 | `Stopped`（重启）/ `Prepared`（仅非 RT 完整重新 admission） | `EV_FAULT`；watchdog 只置位，不自行恢复。 |
| `Stopped` | 线程退出、ring 停、设备关闭、内存在 control 最后释放 | 进程退出 / `Init`（重启） | `EV_STOPPED`。 |

**硬规则**：`Faulted`/`SafetyMute` → `Running` 必须经过 `Prepared` 的完整 admission；`SafetyMute` 期间不得消费任何非安全输出；任何状态进入必须有且仅有一次事件；**SafetyMute 期间若再发生 fatal fault（worker 失联/不变量/ABI）允许 → `Faulted`**；**Draining 期间若发生不可 drain 的 fatal fault 允许 → `Faulted`**。
**幂等/重复规则**：control 侧每次迁移生成 `transition_key=(from,to,reason,epoch_id,plan_version)`；相同 key 的重复请求被忽略并计 `dup_transition_requests++`（事件环不得重复写同一迁移事件）；不同 key 的并发请求按“Faulted > SafetyMute > Draining > Prepared”优先级串行处理（先到先生效，后续按新状态判定）。
**Starting 失败路径**：Starting→Prepared（可回滚启动失败）必须记 `EV_ROLLBACK`；若设备/clock 无法恢复则应 SafetyMute/Faulted，不允许留在 Starting。

### 5.2 EpochLifecycle

| 状态 | 含义 | 迁移 |
|---|---|---|
| `EpochPreparing` | 新时间轴参数/plan 已确定，尚未发布 | →`EpochActive` / →`EpochAborted`（准备失败/取消，定义于 v1.1） |
| `EpochActive` | frame_index/block_seq 从 0 连续推进；当前 plan_version 生效 | →`EpochDraining`（正常停流） / →`EpochQuarantined`（cutoff/失联） / →`EpochAborted`（fault 中止，不再输出） |
| `EpochDraining` | 停止新块提交，drain SPSC 与 workers；旧版本块必须终态 | →`EpochRetired`（全部终态）/ →`EpochAborted`（drain 失败/fault） |
| `EpochQuarantined` | 有 invalid 块/迟到 worker/失联；buffer/graph 隔离 | →`EpochRetired`（all-ack 后由 control）/ 永不（失联时保留至重启） |
| `EpochAborted` | epoch 未能启动或不可安全收尾；无输出承诺、事件记录、内存交 control | →`EpochRetired` |
| `EpochRetired` | 计数/事件已导出；内存由 control 回收 | 终态（epoch_id 永不复用） |

**触发新 epoch 的条件（必须全列，禁止隐式）**：进程启动；fs/B/N/O/plan 的变化不兼容（rate/通道数/block 改变）；plan 的 D/phase/role/fs/P/G 变化导致历史不可复用且放弃 crossfade；PTP/clock discontinuity（失锁阈值由 T19 定，本规范只要求“判定为 discontinuity 即新 epoch”）；设备重开/拔插恢复；不可恢复 xrun/queue 溢出超过策略阈值；2 核重准入/失核；任何 fault 后经完整重新 admission 的恢复。新 epoch 必须重置历史、partial、quarantine 归属，禁止复用旧 epoch 数据。
**事件**：`EpochPreparing→Active` 记 `EV_EPOCH_START`；Draining/Quarantined/Aborted→Retired 记 `EV_EPOCH_END`；任一 →`EpochAborted` 记 `EV_EPOCH_ABORTED`（kind=38，v1.1 新增）。

### 5.3 PlanLifecycle

| 状态 | 含义 | 迁移 | 证据 |
|---|---|---|---|
| `PlanBuilt` | 设计器/校准 daemon 产出字节 | →`PlanValidated` | T12/T13/T66 产物 |
| `PlanValidated` | schema/字段/长度/taps/rate/role/精度/hash 校验通过 | →`PlanAdmitted` / 拒绝 | `graph_id` 计算 |
| `PlanAdmitted` | 容量/双图/ring/栈/mlock 预算准入通过（含 crossfade 预算，若声明） | →`PlanStaged` | `EV_PLAN_ADMITTED` + AdmissionReport |
| `PlanStaged` | 已写入 pending_ptr、switch_state=ARMED，尚未生效 | →`PlanCommitted` / →`PlanAborted`（定义于 v1.1） | `EV_PLAN_STAGED` |
| `PlanCommitted` | 在 block 边界生效；plan_version 已发布 | →`PlanSuperseded` / →`PlanRolledBack` | `EV_PLAN_SWAPPED` |
| `PlanSuperseded` | 新 plan 已提交，旧 plan 等待 retire gate | →`PlanRetired` | `EV_SWAP_ACK` |
| `PlanRolledBack` | commit/prepared 校验失败或 ack 超时，last-good 仍在生效 | →`PlanCommitted`（last-good） | `EV_ROLLBACK` |
| `PlanAborted` | stage/commit 被显式中止或未生效即失败；无输出承诺 | →`PlanRetired`（丢弃） | `EV_PLAN_ABORTED`（kind=37，v1.1 新增） |
| `PlanRetired` | retire gate 全过，control 已释放 | 终态 | `EV_PLAN_RETIRED` |

**规则**：`PlanCommitted` 前不得改变音频输出内容；主动失败（校验/apply/ack 超时）必须回到 last-good 或 SafetyMute，且保留旧 plan 与证据；REST/WS 的 “applied” 只能在实际 plan_version 已出现在至少 1 个完整 block 的 BlockDesc 且 4 worker all-ack 后返回（T24/T42/T70 消费本条）。
**Crossfade（I06）**：仅当 plan flags bit2=1 **且** admission 已按 §11.13 `TspiCrossfadeBudgetV1` 校验通过时允许；要求新旧 plan 的 `total_delay_q16`（§11.10；ABI v1 下 = `delay_samples << 16`，低 16 位恒 0，故等价于整数 D 相等）严格相等（不等则禁止，必须硬切+安全静音）；峰值双 CPU/RAM 必须 ≤ 可用量，否则 `ERR_CROSSFADE_BUDGET` 拒绝。跨 rate/N/O/B 变化一律禁止 crossfade（必须停流新 epoch）。
**PlanAborted/EpochAborted**：v1.1 新增定义，旧文引用补齐；abort 只允许由 control 线程在非 RT 上下文发起；RT 只能请求（safety/abort_request 位）。

### 5.4 DeviceOwner（音频设备唯一 owner）

| 状态 | 含义 | 迁移 |
|---|---|---|
| `OwnerIdle` | 无进程打开 ALSA 设备 | →`OwnerPlayback` / →`OwnerCalibration` |
| `OwnerPlayback` | 正常播放 host 持有 capture+playback 设备 | →`OwnerIdle`（停流）或 →`OwnerCalibration`（必须 Draining→Stopped 后释放设备） |
| `OwnerCalibration` | 校准 daemon 独占（扫频/采集）；日常播放暂停 | →`OwnerIdle`（job 完成/取消/失败）后重新进 `OwnerPlayback` |
| `OwnerTransition` | 交接窗口：旧 owner 已 release、新 owner 尚未 acquire | 只允许短时；超时 → SafetyMute/FAIL，不得两个进程同时开设备。 |

**硬规则**：calibration job 与 playback 不得同时打开声卡（决策 §2/§6）；设备稳定 ID/宽度必须由 T17/T57 解析并校验，禁止假 card4 永不变；owner 交接必须记录 `EV_DEVICE_OWNER_CHANGE` 与前后 actual hwparams hash。

### 5.5 SafetyMode 与 fallback

| 模式 | 输出 | 进入条件 | 退出条件 |
|---|---|---|---|
| `Normal` | 正常工作图 | admission 通过且无 fault | fault/超阈值 |
| `ProtectionFallback` | 预分配 fallback 图：G=I（identity）但保留 P_post 最终保护；**不得超时后临时重算昂贵 P** | deadline miss 超阈值/worker 迟到整块/输入持续无效/2 核重准入未过但安全输出可保证 | control 重新 admission 或 SafetyMute |
| `SilenceMute` | 预分配全零输出（保持 block 节奏与设备时钟） | 无法在 play_deadline 前完成安全输出、worker 失联、ABI/不变量 fault、quarantine 耗尽、watchdog 置位 | 仅非 RT 完整重新 admission；禁止自动恢复 |

**硬规则**：fallback graph 与静音 buffer 必须在 admission 时预分配并单独验证 worst deadline 与资源容量；任何情况下不得消费半图、不得绕保护、不得在超时后临时运行昂贵 P 产生第二次超时。bypass（G=I）不等于绕保护：P_post/limiter 仍必须在输出前执行。

## 6. Block 模型与 SPSC 协议

### 6.1 Block 身份与字段语义

`TspiBlockDescV1`（布局见 §11.3）是 block 的唯一身份；RT 每条路径必须只通过它判断 epoch/version/边界，禁止依赖“全局当前状态”隐式推断。

| 字段 | 语义 | 约束 |
|---|---|---|
| `abi_version` | 结构版本 | 必须 =1；否则非 RT 拒绝（ERR_ABI_VERSION）；RT 运行期不一致 = fault。 |
| `block_size` | B（帧） | 必须 ∈ {64,128}；与 plan/epoch 一致，否则 ERR_BLOCK_SIZE。 |
| `n_in` / `n_out` | 本 block 输入/输出通道数 | 必须 ≤ plan 的 N/O；与 epoch 一致。 |
| `epoch_id` | 时间轴 ID | u64 单调；block 内不可变。 |
| `block_seq` | epoch 内块序号 | 从 0 连续 +1；drain 尾块仍是完整 B 帧（zero-pad），不许变长。 |
| `frame_index` | 输入绝对帧号 | = block_seq × B；u64；播放端用 clock mapping 换算设备帧（T19），core 不做重采样。 |
| `plan_version` | 产生该 block 的 plan 版本 | master 在 block 边界写；worker/playback 必须按它校验 plan 一致性。 |
| `flags` | 状态位（§6.2） | 未知位必须为 0；unknown flag = fault（RT_E_ABI_RUNTIME）。 |
| `valid_frames` | 有效帧数 | 0..=block_size；只有 DRAIN_TAIL 可 <B；其余必须 ==B。 |
| `slot_index` | RtArena 中的 payload 槽号 | 必须 < 对应 ring 的槽数；越界 = fault。 |
| `mono_publish_ns` | capture 发布时刻（CLOCK_MONOTONIC） | 由 I/O 区写；纯 worker 只读传递，不自采时钟。 |
| `play_deadline_ns` | 本 block 必须被 playback 消费的绝对时刻 | 由 capture/playback 所有者按 latency budget 计算；core 只用它做 hard cutoff 安全动作。 |

**B 的来源链（I11）**：core 的 `block_size` 只来自 ALSA/T17 协商后的 **actual period_frames**，且必须 ∈ {64,128}。actual period 不是 64/128 时 T17 必须拒绝启动/重开设备并返回 `ERR_DEVICE_PARAMS`，禁止静默夹取、禁止把 256 当内部 block 再拆；T05/REST 的允许范围必须显式返回不支持值（不静默接受）。

### 6.2 flags 位定义（v1 冻结）

| 位 | 名称 | 语义 |
|---|---|---|
| 0 | `VALID` | 块有效；=0 只能用于内部占位，禁止发布给 playback。 |
| 1 | `DRAIN_TAIL` | 停流/epoch 尾块；valid_frames 可 <B，playback 补零。 |
| 2 | `EPOCH_FIRST` | 新 epoch 第一块；playback 必须按 ramp/mute 策略处理（T17/T21）。 |
| 3 | `DISCONTINUITY` | 时钟/队列 discontinuity 标记；与 EPOCH_FIRST 不同：允许在同一 epoch 内标记 I/O 侧事件；core 不拼接历史。 |
| 4 | `SAFETY_SILENCE` | 该块为预分配安全静音（SilenceMute）；payload 必须全零，不得被 DSP 处理。 |
| 5 | `PLAN_SWITCH` | 新 plan_version 的第一块；crossfade/硬切由 plan 声明。 |
| 6 | `CROSSFADE_TAIL` | crossfade 尾块；双 plan 历史均须保留到本块结束。 |
| 7 | `INPUT_SANITIZED` | I/O 区检测到 nonfinite 并已替换为 0；计数并记录事件，不是 fault。 |
| 8..31 | reserved | 必须写 0。 |

### 6.3 SPSC 环协议（capture→DSP、DSP→playback）

**结构**：固定容量 4 槽的 `TspiSpscHeaderV1`（§11.7）+ 4 个 payload 槽（RtArena 内）。每槽 = B 帧 × N 或 O × f32；payload 地址在 admission 时固定。环只存 `TspiBlockDescV1` 值，不存指针。

**发布-消费配方（必须逐字实现）**：

~~~text
producer(slot p):
  1. 写 payload（仅 producer 可写）
  2. 构造 desc（含 slot_index、epoch、seq、deadline、valid_frames、flags）
  3. desc_slot[p % CAP] = desc
  4. write_pos.store(p + 1, Release)          # 单次 release 发布

consumer(slot q):
  1. w = write_pos.load(Acquire)
  2. if q >= w: empty -> 静音输出 + 计数（不得忙等无限）
  3. desc = desc_slot[q % CAP]
  4. 校验 desc.abi_version/flags/valid_frames/epoch/plan_version；非法 -> 丢块 + 事件
  5. 读 payload（只读）
  6. read_pos.store(q + 1, Release)
~~~

**固定规则**：

- P-01：环容量固定为 `SPSC_CAPTURE_SLOTS` / `SPSC_PLAYBACK_SLOTS`；禁止扩容、禁止无界队列、禁止链表。
- P-02：单生产者单消费者；禁止两个线程同时写同一环；跨线程只通过 `write_pos/read_pos` 的 Acquire/Release 通信，payload 用普通写，不得用 SeqCst 补丁替代配方。
- P-03：满时 producer **丢弃新块**并计数（`EV_INPUT_OVERRUN`、`RT_E_QUEUE_OVERRUN`）；连续 ≥4 块未恢复 → 请求新 epoch（由 control 决断），不得覆盖消费者尚未读完的块。
- P-04：空时 consumer 输出预分配静音（保持设备时钟），计数 `EV_OUTPUT_UNDERRUN`；连续 ≥3 块 → 请求 SafetyMute（由 master/control 执行），禁止重复播放旧块冒充连续。
- P-05：禁止半块读取：consumer 只在 `write_pos` 已覆盖后才看 desc；禁止先读 payload 再等发布。
- P-06：ring、payload、desc 数组都必须 mlock 且预触；RT 中禁止 mmap/munmap/mremap/realloc。
- P-07：`slot_index` 只能来自本环的固定槽；跨环引用 = fault。
- P-08：退化情况（drain 尾块）必须置 `DRAIN_TAIL` 且 valid_frames 正确；DSP 把 [valid_frames, B) 视作 0 输入；playback 把 [valid_frames, B) 写 0。块边界 zero-pad 与 DRAIN_TAIL 不得改变线性卷积语义；流式 flush 长度 = L_F−1 按 T03 SPEC §3.3/§3.4（T08/T14 验收），core 不做周期卷积/跨块丢样。
- P-09（I07/v1.1）：capture 满丢弃新块时**不得**开始该块 payload 的填写；若实现已预占 payload，必须在发布前走 `S_CAPTURE_FILL`→`S_FREE` 中止路径并计数，禁止留下半写槽或未回收槽；中止只能由 capture 在发布前完成，playback/DSP 永不可见半写块。
- P-10（I05/v1.1）：worker 不直接读 capture ring；master 用 §6.4/§7.3 的 `block_start_seq` 发布 block 描述，worker 只在看到新 seq 后处理其描述。

### 6.4 时间轴与 deadline

- epoch 起点：`epoch_start_mono_ns` + frame_index 0；`t(frame) = epoch_start_mono_ns + frame * 1e9 / fs`（有理数精确计算，禁止累积浮点误差；T19 实现）。
- **Block 启动原语（I05/v1.1）**：master 在 block 边界依次写 `RtControl.block_start_epoch / block_start_frame / block_start_plan_version / block_start_slot`（普通存储），随后 `block_start_seq.fetch_add(1, Release)`；worker 用 `Acquire` 读 `block_start_seq`，发现相对本地值 +1（或首次）后读取上述描述并开始本 block。禁止 worker 读 capture ring 或由 master 直接写每 worker 私有标志（除 ack 槽/phase 位）。`block_start_seq` 回绕/跳变 = fault；每个 seq 只能被每个 worker 消费一次（本地缓存 last_seq）。
- capture 发布时写 `mono_publish_ns`；playback 的硬截止取 `play_deadline_ns`（由 latency budget 与队列水位决定；具体由 T17/T19 定，本规范只要求单调、可审计、不倒退）。
- **tick 预算（I04/v1.1，三处一致）**：master 每 block 最多 `TICK_READS_PER_BLOCK_MAX=3` 次预算内 tick：B1 进入 1 次（t_entry）；**仅当** spin 预算耗尽、需要判定 hard_cutoff 时最多 1 次（t_cut）；B3 退出 1 次（t_exit）。正常路径只读 2 次。`compute_ticks=t_exit−t_entry`；不得用“spin 次数猜时间”代替 t_cut，也不得超 3 次。
- **spin/cutoff 参数来源**：`spin_max_iters / cutoff_drain_iters` 由 T18 admission 按目标核标定后写入 `TspiWorkerRegsV1`（§11.13）与 `AdmissionReport`；plan 只携带 profile 标识，不携带未标定值；标定值必须记录在 admission 证据中。
- gate/hard cutoff 公式与判定见 §3.5；deadline 触发动作见 §9.2。
- 96k 的 gate 按 T_block/2 缩放，不得沿用 48k 常量；不同 fs/B 的统计必须分表导出（T47/T30）。

### 6.5 Block / slot 状态机

| 状态 | 写者 | 允许迁移 | 说明 |
|---|---|---|---|
| `S_FREE` | control（初始化） | →`S_CAPTURE_FILL` | 空闲槽，payload 可写。 |
| `S_CAPTURE_FILL` | capture | →`S_READY`（发布）/ →`S_FREE`（中止，I07：未发布前由 capture 独立完成） | capture 独占写 payload；中止必须清空有效标记，禁止半写发布。 |
| `S_READY` | capture→master | →`S_DSP_IN_USE` / →`S_QUARANTINED` | 已发布；master 取用或判 invalid。 |
| `S_DSP_IN_USE` | workers | →`S_DSP_DONE`（all phase ack）/ →`S_QUARANTINED`（cutoff/迟到） | 4 worker 只写自己的 partial。 |
| `S_DSP_DONE` | master | →`S_PLAYBACK_READ`（已发布） | 输出 payload 已由 master/固定 reduce 写完；VALID=1。 |
| `S_PLAYBACK_READ` | playback | →`S_RECYCLABLE`（读毕） | playback 只读；不得写。 |
| `S_RECYCLABLE` | control/非 RT | →`S_FREE` | 只有非 RT 可回收；RT 禁止回收。 |
| `S_QUARANTINED` | master/control | →`S_RECYCLABLE`（all-ack 后 control） | 迟到 worker 仍可能写；禁止复用/释放直到 all-ack；失联则永不回收。 |

**规则**：状态迁移只能由表中写者在指定边沿执行；状态字用单个 `AtomicU32`，迁移必须 CAS（expected 旧值）并记录失败事件；任何不合法迁移 = fault。



## 7. 四 worker 静态 DAG 与 worker-ack

### 7.1 角色与分工（固定于 prepare，运行期不改）

- `dsp_worker[0..3]`：固定 4 线程；`master = dsp_worker[0]`。master 额外负责：block 边界读 plan、启动 phase、bounded wait、发布 output/ack、2 次预算内 tick 采样、安全请求仲裁。master 的 DSP 计算任务不豁免。
- 任务表（`PlanHeader` 内静态表）：每个 worker 获得固定 tasks：path/tap tile、FFT input/bin tile、对角 FIR/PEQ tile、reduce/IFFT/保护固定分段。分配必须写进 plan（prepare 期完成），RT 不 work-steal、不动态负载均衡、不创建线程。
- 私有 partial：每 worker 每输出有固定 scratch（cache-line 隔离，至少 64 B 对齐，禁止相邻 worker 共享同一 cache line 写）；累加不得用原子浮点或无序 atomic。
- 固定 reduce：master 在 B3 按 worker0→1→2→3 的固定顺序合并 partial；同一构建、同一 plan 的浮点结果必须逐位可复现（跨 ISA 用容差，见 §11.9）。

### 7.2 Phase 定义（B1/B2/B3）

| Phase | 内容 | 完成条件（worker k） |
|---|---|---|
| B1 | 读 block/plan（只读）、input 预处理/PEQ、输入侧 FFT（如 plan 选择 FFT）、R 类节点 | 本 worker B1 任务写 partial/history 完成；`phase_done[0]` 置位 k。 |
| B2 | 矩阵 G MAC / 对角 P FIR / FFT 频点 MAC / 长行 tile | 本 worker B2 任务完成；`phase_done[1]` 置位 k。 |
| B3 | 固定 reduce / IFFT / P_post 保护 / 输出 scatter / 最终 finite/clip 检查 | 本 worker B3 任务完成；`phase_done[2]` 置位 k；然后 ack。 |

**约束**：phase 内任务数不足 4 时如实串行（记录 serial_fraction），不得 spin 凑忙；禁止一个 phase 内偷读未就绪的输入/频谱；FFT 的每个 input/bin tile 的依赖发布必须先于消费（不得只设一个总 barrier 后读未完成谱）。B1/B2/B3 的边界与数据依赖必须全部落进 plan 的静态 DAG。

### 7.3 worker-ack 协议（`TspiBlockAckV1`，布局 §11.6；expected 参数化）

- **expected_workers / ack_mask（I03）**：生产 4（expected=4，ack_mask=0xF）；2 核降级是**独立 plan/epoch**（expected=2，ack_mask=0x3，只用位 0..1）。所有“all-ack”判据必须用 `ACK_MASK(expected)`，禁止硬写 0xF；位图在 §11.6 的 `@phase_done` 中按 expected 有效位判定，其余位必须为 0。
- 每 block 一个 ack 槽（容量 `ACK_SLOTS=4` 与 playback ring 对齐），由 master 在 block 开始时初始化：`block_seq/epoch_id/plan_version/expected_workers/cutoff=0/deadline_mono_ns`；初始化必须先于 Release `block_start_seq`。ack 槽带标签，仅当标签匹配本块时才允许 worker 写位。
- worker k 完成 B1：`phase_done[0].fetch_or(1 << k, Release)`；B2/B3 同理。禁止 Relaxed 发布完成位；master 用 Acquire 读取。
- **bounded wait / cutoff（I04/I05）**：master 先查位图；未齐则在 `spin_max_iters`（(T18 标定)）内 bounded spin；耗尽后**可选**读 1 次 t_cut 判定 hard_cutoff；超过 `hard_cutoff_ns` → `cutoff.store(1, Release)`，再等 `cutoff_drain_iters`，然后判 invalid（不无限等）。B3 退出读 t_exit，compute_ticks=t_exit−t_entry；tick 总数 ≤3。
- 迟到 worker：任何 phase 完成后必须检查 `cutoff`；若=1，只能记录 `EV_BLOCK_LATE` 并**丢弃本块所有输出**。写 ack 前必须先校验 ack 槽标签（epoch/block_seq/plan_version），不匹配则置 late_seen 并**不得修改当前槽**（I08）。
- **位图真值表（参数化）**：令 m=ACK_MASK(expected)；位图==m 且 cutoff=0 → all-ack；位图!=m → 继续 bounded wait；任意位图且 cutoff=1 → 过期，丢弃+quarantine，不得发布。0xF/0x3 仅为 expected=4/2 的具体值。
- **ack 槽生命周期（I08）**：槽状态 FREE→INIT→WAIT→ALL_ACK→RECYCLABLE；只有 nonRT 可在 playback 读完（或 quarantine all-ack）后置 RECYCLABLE；RT 禁止提前复位/清零下一块 ack 槽。ack 槽耗尽（4 槽均未终态）→ master 不得启动新 block，置 SafetyMute 并报 `RT_E_ACK_SLOT_EXHAUSTED`（§9.2）。
- **all-ack 定义**：`phase_done` 达到 ACK_MASK(expected) 且 cutoff=0。playback 只能消费 master 发布的 `S_DSP_DONE` 块；worker ack 本身不等于对 playback 可见。

### 7.4 任务表与 2 核降级

- 4-worker 任务表是 declared load 的一部分（plan hash 覆盖）；2 核降级是**独立 plan**：停旧 epoch 提交→4 worker ack/撤销→非 RT 编译+admit 2 核 plan（内存/clock/输出保护/时限检查）→新 epoch；未过则 mute/last-good。禁止失核后剩两核播放半图。
- 1/2/4 worker benchmark 必须同 kernel/precision/coeff/input/fs/B/编译/ISA/频率/热身，只改 worker 数（T46）；本规范只固定“worker 数进 plan/epoch，不运行期变”。
- **Crossfade（I06/v1.1）**：仅当 plan flags bit2=1 且 admission 写入的 `TspiCrossfadeBudgetV1` 校验通过（新旧 total_delay_q16 相等——ABI v1 下低 16 位恒 0，即整数 D 相等；双图/双历史 RAM、峰值双 CPU 均 ≤ 可用量）才允许。不足则 `ERR_CROSSFADE_BUDGET` 拒绝并只允许硬切+安全静音；跨 rate/N/O/B 变化禁止 crossfade。T20 必须覆盖“双图峰值超限→拒绝/静音”负向测试与 D 对齐断言（D_total_q16 相等）。

### 7.5 Swap / retire gate（与 §4.4/§5.3 一致）

旧 plan 的 `PlanArena` 释放前必须同时满足：

| 条件 | 检查者 | 证据 |
|---|---|---|
| RG-1 指针已替换：`current_plan_ptr` 不再指向旧 arena；`switch_state` 已回 IDLE | master + control | `EV_PLAN_SWAPPED` |
| RG-2 4 worker 都已在至少一个 block 上使用新 `plan_version`（`swap_ack` all-ack） | master | `EV_SWAP_ACK` |
| RG-3 旧版本 block 全部终态：DONE/RECYCLED/QUARANTINED 且 quarantine 已 all-ack；playback ring 无旧版本未终态块 | control | 槽状态快照 |
| RG-4 `retire_ready` 置位后由 control 线程在非 RT 上下文删除强引用并 unmap | control | `EV_PLAN_RETIRED` |

**硬规则**：RG-2/RG-3 任一未满足（含 worker 失联永不 ack）→ **禁止释放**，保持 quarantine、进入 SafetyMute、等进程重启。RT 线程不得成为旧 plan 的最后一个引用持有者，也不得执行 `drop`。

### 7.6 Quarantine 与 ack 槽耗尽规则（I08 补齐）

- invalid 判定（cutoff/迟到/epoch mismatch/plan mismatch/不变量）后，master 把该 block slot 标 `S_QUARANTINED`，并把“可能迟到 worker 集合”写进 quarantine 元数据（epoch/version/block_seq/slot）。
- 迟到 worker 只允许写本 block 的私有 partial；写完必须 ack“本块不再使用”；全部 ack 后 control 才能把槽置 RECYCLABLE。写 ack 前必须校验 ack 槽标签，不匹配（旧槽/新块）→ late_seen 计数且不写坏当前槽。
- **ack 槽与 quarantine 联动**：每个未终态 block 占用 1 个 ack 槽；`ACK_SLOTS=4`。master 仅当有 FREE ack 槽时才可启动新 block；4 槽全被占（连续 invalid+late 未 ack）→ 停止启动 → SafetyMute，报 `RT_E_ACK_SLOT_EXHAUSTED`，不得覆盖/复用旧槽。
- 池容量 `QUARANTINE_SLOTS`；耗尽 → 拒绝新 commit/切图（`ERR_QUARANTINE_EXHAUSTED`）、请求 SafetyMute；禁止临时分配、禁止覆盖。
- quarantine 中所有内存保持 mlock 直到释放/进程退出；watchdog/control 不得为“省内存”强行回收；worker 失联时该块的 ack 永不到齐 → 该槽与内存永不回收，等进程重启（安全优先）。

### 7.7 Worker / master 主循环（伪代码，未来 T09 实现必须语义等价）

~~~text
#[RT] fn worker_loop(k: u8) -> ! {
  loop {
    wait_for_block_start();                    // bounded wait, no syscall
    let lease = load_current_plan();           // once per block, INV-L1
    let desc = read_block_desc();              // epoch/version/flags
    if !validate(desc, lease) { ack_fail(); continue; }   // no panic, no drop
    run_phase(B1, k, &lease, &desc);           // fixed tasks
    ack_phase(0, k);
    run_phase(B2, k, &lease, &desc);
    ack_phase(1, k);
    if cutoff_is_set() { discard_and_ack(); continue; }
    run_phase(B3, k, &lease, &desc);
    ack_phase(2, k);
    finish_block(k);                           // no alloc, no clock
  }
}

#[RT] fn master_loop() -> ! {
  loop {
    let desc = pop_capture_ring_bounded();     // empty -> underrun policy
    let t0 = rt_tick();                        // budgeted read #1
    publish_block_start(&desc);                // release workers
    let ack = bounded_wait_phases(&desc);      // spin + cutoff
    let t1 = rt_tick();                        // budgeted read #2
    if ack.all && !ack.cutoff {
      finalize_output(&desc); push_playback_ring(&desc); // Release publish
    } else {
      quarantine(&desc); safety_output_for_slot(&desc); emit_event(EV_BLOCK_LATE);
    }
    write_stats(compute_ticks = t1 - t0);
  }
}
~~~



## 8. 事件、计数与有界直方图

### 8.1 Event 记录（`TspiEventV1`，布局 §11.5）

| 字段 | 语义 |
|---|---|
| `seq` | 每 lane 内单调 u64（从 0 起）；用于检测丢事件。 |
| `mono_ns` | 仅 I/O/watchdog/control 区可写 CLOCK_MONOTONIC；纯 worker/master 的 DSP 事件写 0（由非 RT 侧按 block 统计补时间）。 |
| `epoch_id` / `block_seq` | 关联块；非块事件可为 0/最近值。 |
| `kind` | 事件类型（§8.2）。 |
| `lane` | 0..3 DSP worker、4 capture、5 playback、6 watchdog、7 control。 |
| `severity` | 1 info / 2 warning / 3 error / 4 fatal。 |
| `value_a / value_b` | 类型化小整数（如 status code、worker、计数）；语义见 §8.2。 |
| `payload[2]` | kind 专用（如 hash 低 128 位、两个 u32 计数）；未用必须写 0。 |

### 8.2 Event kinds（v1 冻结）

| kind | 名称 | severity | 产生者（lane） | value_a / value_b / payload 语义 |
|---:|---|---|---|---|
| 1 | `EV_HOST_INIT` | 1 | control | 0 / 0 / 0 |
| 2 | `EV_ADMISSION_OK` | 1 | control | admission flags / 0 / arena 字节 |
| 3 | `EV_ADMISSION_FAIL` | 3 | control | RtStatus / 失败阶段 / 0 |
| 4 | `EV_PLAN_PREPARED` | 1 | control | plan_version / path_count / graph_id 低 128 位 |
| 5 | `EV_PLAN_ADMITTED` | 1 | control | plan_version / arena_bytes / hash |
| 6 | `EV_PLAN_STAGED` | 1 | control | plan_version / 0 / hash |
| 7 | `EV_PLAN_SWAPPED` | 1 | master/control | 新 version / 旧 version / 0 |
| 8 | `EV_SWAP_ACK` | 1 | master | version / done_mask / 0 |
| 9 | `EV_PLAN_RETIRED` | 1 | control | 旧 version / 0 / 0 |
| 10 | `EV_ROLLBACK` | 2 | control | 回滚到 version / 原因码 / 0 |
| 11 | `EV_EPOCH_START` | 1 | control/master | epoch_id / 原因码 / 0 |
| 12 | `EV_EPOCH_END` | 1 | control/master | epoch_id / 原因码 / 0 |
| 13 | `EV_RUNNING` | 1 | control | epoch_id / block_size / fs |
| 14 | `EV_DRAINING` | 1 | control | epoch_id / 0 / 0 |
| 15 | `EV_BLOCK_LATE` | 3 | master | block_seq / done_mask / compute_ticks |
| 16 | `EV_DEADLINE_MISS` | 3 | master | block_seq / compute_ticks / gate_ns |
| 17 | `EV_BLOCK_INVALID` | 3 | master | block_seq / RtStatus / epoch_id |
| 18 | `EV_INPUT_OVERRUN` | 3 | capture | consecutive / capacity / 0 |
| 19 | `EV_OUTPUT_UNDERRUN` | 3 | playback | consecutive / capacity / 0 |
| 20 | `EV_INPUT_NONFINITE` | 2 | capture/format | samples / channel_mask 低 64 位 / 0 |
| 21 | `EV_CLIP` | 2 | playback/format | clipped_samples / max_abs_q / 0 |
| 22 | `EV_OUTPUT_NONFINITE` | 4 | master/playback | samples / out_index / 0 |
| 23 | `EV_SAFETY_MUTE_ENTER` | 4 | control/watchdog | 原因码 / epoch_id / 0 |
| 24 | `EV_SAFETY_MUTE_EXIT` | 1 | control | 原因码 / 新 plan_version / 0 |
| 25 | `EV_FALLBACK_ENTER` | 3 | master | 原因码 / epoch_id / 0 |
| 26 | `EV_FALLBACK_EXIT` | 1 | control | 0 / 0 / 0 |
| 27 | `EV_QUARANTINE_ENTER` | 3 | master | block_seq / epoch_id / plan_version |
| 28 | `EV_QUARANTINE_RELEASE` | 1 | control | block_seq / 0 / 0 |
| 29 | `EV_QUARANTINE_EXHAUSTED` | 4 | master | 0 / 0 / 0 |
| 30 | `EV_CLOCK_DISCONTINUITY` | 3 | clock/capture | old ppm_q16 / new ppm_q16 / 0 |
| 31 | `EV_DEVICE_OWNER_CHANGE` | 1 | control | from owner / to owner / 0 |
| 32 | `EV_WORKER_LOST` | 4 | watchdog/master | worker / last_epoch / last_block_seq |
| 33 | `EV_WATCHDOG_MUTE` | 4 | watchdog | stale_blocks / epoch_id / 0 |
| 34 | `EV_FAULT` | 4 | control | RtStatus / 阶段 / 0 |
| 35 | `EV_STOPPED` | 1 | control | 0 / 0 / 0 |
| 36 | `EV_STARTING` | 1 | control | epoch_id / 0 / 0 |
| 37 | `EV_PLAN_ABORTED` | 2 | control | plan_version / 原因码 / 0 |
| 38 | `EV_EPOCH_ABORTED` | 3 | control | epoch_id / 原因码 / 0 |
| 39 | `EV_ACK_SLOT_EXHAUSTED` | 4 | master | 占用槽数 / epoch_id / 0 |
| 40 | `EV_CROSSFADE_BUDGET_REJECT` | 3 | control/master | 请求 id / 原因码 / 峰值差值 |

> 上表 kind 编号是 v1 契约：1–35 为 v1 冻结、不得改号；**v1.1 追加 36–40**（I07/I08/I06），同步 T06 常量与事件导出映射；新增仍需升版并更新本文。

### 8.3 事件环（固定容量，单 lane 单生产者）

- `EVENT_LANES=8` × `EVENT_LANE_SLOTS=128` = 1024 槽 × 64 B = 65536 B；每 lane 单写者（worker 各自 lane、capture、playback、watchdog、control），禁止多写者竞争。
- 写入配方：`seq = lane.next_seq++`（仅本 lane 写者）；若 `seq - lane.read_seq >= 128` → 丢弃并 `lane.dropped.fetch_add(1, Relaxed)`；否则写记录到 `seq % 128`，最后 `lane.write_seq.store(seq+1, Release)`。读端非 RT 用 Acquire。
- RT 内禁止把事件格式化为字符串、禁止分配、禁止打印；事件导出（含 kind→名称、tick→ns）由非 RT 完成。
- 统计数据源与事件分离：计数/直方图用于验收；事件用于审计/排障，不要求零丢；丢事件必须体现在 `dropped` 中，导出时一并报告。

**事件幂等/去重（I07/v1.1）**：状态迁移事件必须携带 `transition_key=(from,to,reason,epoch_id,plan_version)`；control 对相同 key 只执行一次迁移并只写一次事件，重复请求计 `dup_transition_requests++` 且不重复写事件。非状态事件（block/worker/I/O）不去重，但同一 lane 的 seq 必须单调；消费端按 (lane,seq) 排序，跨 lane 不保证全序。事件丢失只影响审计，不得作为状态迁移依据。

### 8.4 计数与直方图（固定布局，非 RT 导出）

| 名称 | 类型 | 粒度 | 定义 |
|---|---|---|---|
| `blocks_done` | u64 | worker/全局 | 完成 ack 的 block 数。 |
| `late_blocks` | u64 | worker/全局 | 该 worker 迟到导致 invalid 的块数。 |
| `cutoff_blocks` | u64 | 全局 | cutoff=1 的块数。 |
| `deadline_gate_miss` | u64 | 全局 | compute_ticks 超 gate 的块数（验收门，不停流）。 |
| `hard_late` | u64 | 全局 | 超过 hard_cutoff_ns 的块数。 |
| `input_overruns` / `output_underruns` | u64 | 全局 | 环满/空导致的丢块/补静音次数。 |
| `nonfinite_in` / `nonfinite_out` | u64 | 全局 | 输入被 sanitize 的样本数 / 输出检测到 nonfinite 的样本数（后者应为 0，>0 即 fault）。 |
| `clipped_samples` | u64 | 全局 | 输出保护/格式转换 clip 的样本数。 |
| `quarantine_entries` / `retained` | u64 | 全局 | 进入/仍保留的隔离块数。 |
| `swap_count` / `retire_count` | u64 | 全局 | 成功 swap/retire 计数。 |
| `worker_stats` | `TspiWorkerStatsV1[4]` | per-worker | busy/spin/max_block_ticks/blocks/late/invalid/flags（布局 §11.8）。 |
| `hist[metric][worker][32]` | u32 | 固定数组 | 6 个 metric（block_ticks、spin_ticks、io_read_ticks、io_write_ticks、queue_depth、phase_gap_ticks）×4 worker×32 桶；桶边界由 prepare 写死；溢出桶为第 32 桶。 |

**导出规则**：非 RT 线程以 Acquire 读取快照（允许轻微不一致），导出必须带 `graph_id/plan_version/epoch_id` 与采样窗口；不得从 RT 内写文件/网络；直方图边界与单位（ticks，及换算后的 ns）必须写入报告，避免“每秒/每块”混用。



## 9. RT 错误模型与安全动作

### 9.1 `RtStatus` 表示

`RtStatus` 是 `#[repr(transparent)] struct RtStatus(pub u32)` 的 **Copy** 值：0=OK；非 0 为错误码。**禁止** 用 `Result<T, String>`、`Result<T, Box<dyn Error>>` 或任何带 Drop 的错误物体跨 RT 边界；错误码范围即分类，低 16 位为 code，无 payload。

| 码段 | 类别 | 是否可能由 RT 返回 | 典型动作 |
|---:|---|---|---|
| 0 | OK | 是 | 继续。 |
| 101–199 | 配置/ABI/准入（非 RT） | 否（admission/prepare 返回） | 拒绝 plan/启动；保留 last-good 或 mute。 |
| 201–299 | block 局部 RT 错误 | 是 | 计数、丢块或 sanitize、继续流（具体见 9.2）。 |
| 301–399 | 流级安全错误 | 是（由 master 汇总） | 进入 ProtectionFallback 或 SafetyMute。 |
| 401–499 | 进程级 fault | 是（fatal） | SafetyMute/Faulted；不自动恢复；watchdog 记录。 |

### 9.2 错误码与动作（v1 冻结）

| code | 名称 | 返回者 | 动作（必须） |
|---:|---|---|---|
| 101 | `ERR_ABI_VERSION` | 非 RT | 拒绝；不得尝试运行期迁移。 |
| 102 | `ERR_MAGIC` | 非 RT | 拒绝。 |
| 103 | `ERR_BAD_LAYOUT` | 非 RT | 拒绝（字段/大小/偏移不符）。 |
| 104 | `ERR_RATE` | 非 RT | 拒绝（fs 不在 48k/96k）。 |
| 105 | `ERR_BLOCK_SIZE` | 非 RT | 拒绝（B 不在 64/128）。 |
| 106 | `ERR_CHANNELS` | 非 RT | 拒绝（N/O 越界或与设备不符）。 |
| 107 | `ERR_ROLES` | 非 RT | 拒绝（role/output 映射/宽度不符）。 |
| 108 | `ERR_PRECISION` | 非 RT | 拒绝（runtime 只收 f32 plan）。 |
| 109 | `ERR_STRIDE_ALIGN` | 非 RT | 拒绝（stride 非 16 样本倍数或 channel 基址非 64B 对齐）。 |
| 110 | `ERR_TAPS` | 非 RT | 拒绝（>16384 或 extended 未准入）。 |
| 111 | `ERR_PATH_TABLE` | 非 RT | 拒绝（路径重复/越界/长度不符/非零尾缺失）。 |
| 112 | `ERR_HASH` | 非 RT | 拒绝（graph_id 与内容不符）。 |
| 113 | `ERR_CAPACITY` | 非 RT | 拒绝（arena/双图/quarantine 容量不足）。 |
| 114 | `ERR_ADMISSION_MEMORY` | 非 RT | 不启 RT 主模式；last-good/mute。 |
| 115 | `ERR_ADMISSION_SCHED` | 非 RT | 不启 RT 主模式。 |
| 116 | `ERR_ADMISSION_CLOCK` | 非 RT | 不启 RT 主模式。 |
| 117 | `ERR_DEVICE_BUSY` | 非 RT | 拒绝（owner 冲突）；不得抢占。 |
| 118 | `ERR_DEVICE_PARAMS` | 非 RT | 拒绝：actual period/rate/channels 不满足（period 必须∈{64,128}，见 §6.1/I11）；禁止静默夹取。 |
| 119 | `ERR_PLAN_STALE` | 非 RT | 拒绝 commit（role/fs/P/G/hash 变化未重设计）。 |
| 120 | `ERR_SWITCH_BUSY` | 非 RT | 拒绝（在途切换）；REST 返回 conflict。 |
| 121 | `ERR_QUARANTINE_EXHAUSTED` | 非 RT/RT | 拒绝新 commit/切图；SafetyMute。 |
| 122 | `ERR_UNSUPPORTED` | 非 RT | 拒绝（上混/下混/未实现节点/未知命令）。 |
| 123 | `ERR_CROSSFADE_BUDGET` | 非 RT | 拒绝 crossfade/降级硬切+安全静音（双图/双历史峰值或 D 对齐不满足）。 |
| 201 | `RT_E_PARTIAL_LATE` | worker/master | 本块 invalid；丢弃；计数；quarantine。 |
| 202 | `RT_E_CUTOFF` | master | 本块 invalid；安全输出；计数；quarantine。 |
| 203 | `RT_E_BLOCK_INVALID` | master | 本块 invalid；安全输出。 |
| 204 | `RT_E_EPOCH_MISMATCH` | worker/master | 丢弃本块；不跨 epoch 使用；计数。 |
| 205 | `RT_E_PLAN_MISMATCH` | worker/master | 丢弃本块；请求重新 stage/abort；计数。 |
| 206 | `RT_E_SLOT_STATE` | any RT | fault（不变量破坏）→ SafetyMute。 |
| 207 | `RT_E_QUEUE_OVERRUN` | capture | 丢新块；计数；连续≥4 → 请求新 epoch。 |
| 208 | `RT_E_QUEUE_UNDERRUN` | playback | 补静音；计数；连续≥3 → 请求 SafetyMute。 |
| 209 | `RT_E_INPUT_NONFINITE` | capture/format | sanitize 为 0；计数；置 INPUT_SANITIZED；继续。 |
| 210 | `RT_E_INPUT_CLIP` | capture/format | 饱和到 [-1,1]（格式层语义）；计数；继续。 |
| 301 | `RT_E_OUTPUT_NONFINITE` | master/playback | 本块替换为预分配安全静音；计数；进入 SafetyMute（>0 即 fault 级）。 |
| 302 | `RT_E_GUARD_FAIL` | master | 保护/合理性检查失败；SafetyMute。 |
| 303 | `RT_E_LATE_STREAK` | master | 连续迟到超阈值（默认 2 块）；ProtectionFallback。 |
| 304 | `RT_E_UNDERRUN_STREAK` | playback | 连续欠载超阈值；SafetyMute。 |
| 305 | `RT_E_OVERRUN_STREAK` | capture | 连续溢出超阈值；请求新 epoch。 |
| 306 | `RT_E_QUARANTINE_FULL` | master | 拒绝新提交/切图；SafetyMute。 |
| 307 | `RT_E_FALLBACK_UNAVAILABLE` | master | 进入 SilenceMute（若有安全静音）否则 Faulted。 |
| 308 | `RT_E_ACK_SLOT_EXHAUSTED` | master | 停止启动新 block；SafetyMute；quarantine 不回收；等待非 RT 恢复。 |
| 309 | `RT_E_CROSSFADE_ALIGN` | master/control | 拒绝 crossfade（total_delay_q16 不等——ABI v1 下等价整数 D 不相等/D 不可对齐）；硬切+安全静音。 |
| 401 | `RT_E_WORKER_LOST` | watchdog/master | SafetyMute/Faulted；不释放 quarantine；等进程重启。 |
| 402 | `RT_E_MASTER_LOST` | watchdog | SafetyMute/Faulted。 |
| 403 | `RT_E_INVARIANT` | any RT | Faulted；禁止继续输出。 |
| 404 | `RT_E_ABI_RUNTIME` | any RT | Faulted（结构/字段/flag 非法）。 |
| 405 | `RT_E_DEADLINE_HARD` | master | 本块 invalid；硬截止超时；连续 2 块 → SafetyMute。 |
| 406 | `RT_E_WATCHDOG` | watchdog/control | SafetyMute/Faulted（heartbeat 超时）。 |
| 407 | `RT_E_DEVICE_LOST` | I/O 区 | 进入设备恢复状态机（T17）；期间静音；恢复需新 epoch。 |

### 9.3 no-panic / 无分配 / 确定性错误处理规则

- RT 函数（§12 标 `[RT]`）**禁止** `panic!/unwrap/expect/assert!/unreachable!/todo!/unimplemented!`、禁止可 panic 的索引/转换、禁止 `catch_unwind`、禁止 `debug_assert!`（debug build 不得用于生产 admission；admission 必须拒绝 `cfg!(debug_assertions)` 的生产 RT 二进制）。
- RT 函数**禁止**构造/返回任何带 Drop 的错误值、禁止格式化错误消息；错误必须用 `RtStatus` 或固定事件返回；文本解释由非 RT 侧在导出时映射。
- 整数算术必须显式语义：能用 `checked_*` 的用 `checked_*` 并返回错误；计时/计数用 `wrapping_*` 且上界由 §3.4 保证；禁止依赖 release 自动回绕而不写意图。
- worker 只返回本 worker 的局部错误；master 负责汇总并决定块级/流级动作；**只有 master（或 control/watchdog）可以请求 SafetyMute**，worker 不得直接改全局安全状态。
- 错误处理不得递归再触发错误（事件/计数写失败只丢事件/增 dropped，不覆盖原始错误）。



## 10. RT 三区禁止行为（固定表）

### 10.1 三区允许/禁止摘要

| 行为 | DSP RT（worker/master） | I/O 区（capture/playback） | 控制区（CTL） |
|---|---|---|---|
| heap 分配/释放 | **禁止** | 稳态禁止（启动/恢复时允许，且必须预分配优先） | 允许 |
| 锁/阻塞/await | **禁止**（只允许 §7.3 有界 spin+ack 原子） | 禁止互斥锁；可阻塞在 ALSA poll/read/write（计入 I/O 预算） | 允许 |
| syscall | **禁止**（含 clock/read/write/ioctl/mmap/futex） | 允许 read/write/poll/mmap 相关+恢复状态机，须记预算与尾延迟 | 允许 |
| 读时钟 | 仅 master 每 block **≤3 次**预算内 tick（B1 进入/可选 cutoff/B3 退出；I04/E2）；纯 worker 0 次 | 允许 CLOCK_MONOTONIC（时间戳/队列水位），预算 | 允许 |
| 日志/printf/文件/网络 | **禁止** | 禁止热路径；只经事件环/计数 | 允许 |
| 线程创建/join/亲和设置 | **禁止** | 禁止 | 允许（admission 后一般禁止新增 RT 线程） |
| FFT plan 创建/销毁 | **禁止**（plan 在 CTL 预建） | 禁止 | 允许 |
| panic/unwind | **禁止** | 禁止 | 允许但必须捕获在边界，不得穿到 RT |
| 析构器（Drop/dealloc/Arc-drop） | **禁止**（§13） | 禁止作用到 RT 对象；临时转换 buffer 预分配 | 允许；旧 plan 只在此释放 |
| 共享可变状态 | 只允许 RtArena 规定槽+原子/事件环 | 只允许 ring header 与自有 slot | 只允许控制结构 |
| 原子内存序 | 只用配方指定 Acquire/Release/Relaxed；热循环禁止 SeqCst/fence | 同上 | 不限 |
| unsafe/FFI | 只允许白名单模块+SAFETY 注释/INV 引用；v1 无 FFI | 同左 | FFI 可在 CTL 边界，须 repr(C) 规则 |

### 10.2 禁止行为清单（F-01..F-44；v1.1 增补规则 ID/工具/失败判据，I09）

> 适用范围：`DSP` = DSP RT 区；`IO` = I/O 区；`CTL` = 控制区；`WD` = watchdog。除特别注明外，禁止项对 DSP 为绝对禁止。规则 ID 与工具类见 §10.3；**每行都必须有可执行检查或审计证据，不允许“口头保证”**。

| ID | 禁止行为 | 适用 | 允许替代 | 规则 ID / 工具 | 失败判据 / 豁免 |
|---|---|---|---|---|---|
| F-01 | heap 分配/释放（Box/Vec/String/HashMap/BTreeMap/Cow/format!/to_string/collect/vec!） | DSP | 固定 arena+固定数组+Copy 值 | RT-FORBID-01 / L1+L2 | 命中即 FAIL；DSP 无豁免 |
| F-02 | 隐式临时分配（闭包装箱、迭代器 collect、动态分派构造） | DSP | 显式循环/固定容量迭代 | RT-FORBID-02 / L2+R1 | 命中即 FAIL；DSP 无豁免 |
| F-03 | Arc/Rc/Weak clone/drop/upgrade | DSP | PlanRef/raw pointer+version（§4.4） | RT-FORBID-03 / L1+L2 | 命中即 FAIL；DSP 无豁免 |
| F-04 | Box<dyn Trait>/trait object 创建或析构 | DSP | 枚举/静态分派 | RT-FORBID-04 / L2 | 命中即 FAIL |
| F-05 | 路径/字符串/容器类型（PathBuf/CString/OsString/Map/Set） | DSP | 只传 id/索引 | RT-FORBID-05 / L2 | 命中即 FAIL |
| F-06 | 无界递归/无界大栈数组 | DSP | 固定深度+admission 预触栈上限 | RT-FORBID-06 / T1+R1 | 栈审计缺失/超限即 FAIL |
| F-07 | FFT plan 创建/销毁/重规划 | DSP | CTL 预建 FFT plan 与 scratch | RT-FORBID-07 / L2 | 命中即 FAIL |
| F-08 | mmap/munmap/mprotect/mlock 在 RT | DSP | admission 前完成 | RT-FORBID-08 / L2 | 命中即 FAIL |
| F-09 | Mutex/RwLock/OnceLock/Condvar/parking_lot 等锁 | DSP | 无锁 SPSC/原子 ack | RT-FORBID-09 / L1+L2 | 命中即 FAIL |
| F-10 | 阻塞等待/sleep/yield/futex/await | DSP | 有界 spin+cutoff | RT-FORBID-10 / T1 | 超时测试失败即 FAIL |
| F-11 | 无 cutoff 的 CAS/spin 循环 | DSP | §7.3 有界协议 | RT-FORBID-11 / T1+R1 | 无上界即 FAIL |
| F-12 | std::sync::mpsc 或分配式 channel | DSP | 固定 SPSC ring | RT-FORBID-12 / L1+L2 | 命中即 FAIL |
| F-13 | 任何 syscall（read/write/open/ioctl/clock_gettime/futex/sched_*/getpid/prctl/epoll 等） | DSP | IO 区完成或 admission 前完成 | RT-FORBID-13 / R1（符号/系统调用审查） | 审查记录缺失或命中即 FAIL；DSP 无豁免 |
| F-14 | 纯 worker 读时钟（Instant::now/clock_gettime/rdtsc/CNTVCT） | DSP 纯 worker | master 每 block ≤3 次预算 tick | RT-FORBID-14 / L2+T1 | 命中即 FAIL |
| F-15 | 文件/网络/socket/进程操作 | DSP | 事件环+CTL 导出 | RT-FORBID-15 / L2 | 命中即 FAIL |
| F-16 | 日志/打印/panic hook/journald | DSP/IO 热路径 | 事件环+计数 | RT-FORBID-16 / L2 | 命中即 FAIL |
| F-17 | 创建/join 线程、改亲和/优先级/调度 | DSP/IO | admission 前一次设置 | RT-FORBID-17 / L2 | 命中即 FAIL |
| F-18 | TLS/thread_local/懒初始化（OnceLock/lazy_static） | DSP | 显式传入上下文 | RT-FORBID-18 / L2 | 命中即 FAIL |
| F-19 | 信号处理/atfork/panic hook | DSP/IO | CTL 侧统一处理 | RT-FORBID-19 / R1 | 审查记录缺失或命中即 FAIL |
| F-20 | /proc、/sys、环境变量读取 | DSP/IO 热路径 | CTL 读取后以 plan 字段传入 | RT-FORBID-20 / L2 | 命中即 FAIL |
| F-21 | panic!/unwrap/expect/assert!/unreachable!/todo! | DSP/IO RT | RtStatus+显式分支 | RT-FORBID-21 / L1 | 命中即 FAIL；DSP 无豁免 |
| F-22 | 可能 panic 的索引/切片（未证明边界） | DSP | get()/显式边界检查+错误码 | RT-FORBID-22 / L1+T1 | 负向测试失败即 FAIL |
| F-23 | 未声明语义的整数溢出/截断 | DSP | checked_*/wrapping_*+注释 | RT-FORBID-23 / R1 | 审查记录缺失即 FAIL |
| F-24 | catch_unwind/跨边界 unwind | DSP | 不 panic；边界处 CTL 捕获 | RT-FORBID-24 / L1+L2 | 命中即 FAIL |
| F-25 | 带 Drop 的错误类型/Result<_,String> | DSP/IO | RtStatus(u32) | RT-FORBID-25 / L1+L2 | 命中即 FAIL |
| F-26 | abort/exit/process::Command | DSP/IO | 返回错误由 CTL 处理 | RT-FORBID-26 / L2 | 命中即 FAIL |
| F-27 | 任何 drop/drop_in_place/dealloc（含作用域结束） | DSP | 无 Drop 类型（§13） | RT-FORBID-27 / L1+S1 | 命中/size/needs_drop 断言失败即 FAIL |
| F-28 | 用 mem::forget/ManuallyDrop 规避析构 | DSP 所有 | 类型本身 needs_drop=false | RT-FORBID-28 / R1+L2 | 审查记录缺失或命中即 FAIL |
| F-29 | 释放/减引用旧 plan/graph 的最后引用 | DSP | RG-1..4 retire gate（§7.5） | RT-FORBID-29 / T1 | T20 用例失败即 FAIL |
| F-30 | 把 buffer/槽归还分配器或 arena 回收器 | DSP | 只置状态，CTL 回收 | RT-FORBID-30 / T1+R1 | 审查/测试失败即 FAIL |
| F-31 | quarantine all-ack 前复用/释放槽 | DSP/CTL | INV-L5 | RT-FORBID-31 / T1 | T20/T21 负向测试失败即 FAIL |
| F-32 | 把带 Drop 的对象移入 RT 区/临时作用域 | DSP | POD/Copy | RT-FORBID-32 / L1+S1 | 命中即 FAIL |
| F-33 | 热循环 SeqCst/fence（未在配方中声明） | DSP | 配方 Acquire/Release/Relaxed | RT-FORBID-33 / R1 | 审查记录缺失或命中即 FAIL |
| F-34 | 未声明的共享可变状态（跨 worker 写同一 cache line） | DSP | 私有 partial+cache-line 隔离 | RT-FORBID-34 / S1+T1 | 布局断言/测试失败即 FAIL |
| F-35 | 原子浮点/无序浮点归约改变固定顺序 | DSP | 固定 reduce 顺序（§7.1） | RT-FORBID-35 / T1 | T07/T08/T15 对拍失败即 FAIL |
| F-36 | work stealing/动态线程池/嵌套 rayon | DSP | 静态任务表 | RT-FORBID-36 / L2 | 命中即 FAIL |
| F-37 | 白名单模块外的 unsafe 或无 SAFETY/INV 注释 | DSP/IO | 仅 spsc/ack/arena/simd 白名单 | RT-FORBID-37 / R1 | 审查记录缺失或命中即 FAIL |
| F-38 | RT 内 FFI 调用/跨 C ABI 传 Rust 类型 | DSP | v1 无 FFI；见 §11.11 | RT-FORBID-38 / L2 | 命中即 FAIL |
| F-39 | RT 图内 dyn Trait 动态分派 | DSP | 枚举/静态分派 | RT-FORBID-39 / L2 | 命中即 FAIL |
| F-40 | 运行期 CPU feature 探测/CPUID | DSP | admission 一次探测，按 plan flag 分派 | RT-FORBID-40 / C1+L2 | 命中即 FAIL |
| F-41 | 非有限值进入输出（未 sanitize/未检测） | DSP/IO | 输入 sanitize+输出 finite 检查 | RT-FORBID-41 / T1 | 参考向量/测试失败即 FAIL |
| F-42 | 未声明语义的 float→int（截断/取整/饱和） | IO | HwParams 固定格式语义（§11.9/T22） | RT-FORBID-42 / T1 | 参考向量失败即 FAIL |
| F-43 | 未声明 dither/种子导致非确定输出 | IO | plan/HwParams 显式 dither 配置；默认 off | RT-FORBID-43 / T1 | 测试失败即 FAIL |
| F-44 | 运行期改变 N/O/B/stride/精度 | DSP/IO | 变化必须新 epoch+新 plan | RT-FORBID-44 / T1 | 测试失败即 FAIL |

### 10.3 规则 ID、工具类、失败判据与豁免（I09/v1.1）

**规则 ID**：`RT-FORBID-01..44` 与 F-01..F-44 一一对应（规则 ID 编号=F 编号）；D 类规则见 §13.5 的 `RT-DROP-01..20`。

**工具类（T06 必须实现对应入口，测试必须 executed test_count>0）**：

| 类 | 工具/机制 | 覆盖 | 失败判据 |
|---|---|---|---|
| L1 | rustc/clippy deny lint（@#![deny(clippy::unwrap_used, panic, index_out_of_bounds, disallowed_types, disallowed_methods)]#@ 等）+ T06 配置 | F 中“命中即 FAIL”的 panic/类型/分配/锁/TLS 类 | lint 命中即 CI FAIL |
| L2 | T06 `rt-check forbid` 自写扫描（函数/模块白名单、禁用 token/符号、调用图、unsafe/FFI/extern） | F 中绝大多数 token/符号级 | 命中即 FAIL；扫描器自身需正/负 fixture |
| S1 | 编译期静态断言（`size_of/offset_of/needs_drop/align`）+ 布局断言 | F-27/32/34 与 §11 全结构 | 断言失败即 FAIL |
| T1 | T09/T20/T21/T07/T08/T15 运行测试（mock/fixture；executed>0） | F-06/10/11/14/22/27/29/30/31/34/35/41/42/43/44 | 用例失败/缺失即 FAIL |
| R1 | **人工审查 + 审计模板**（仅审查项必须留证） | F-02/13/19/23/28/30/33/37 等 | 模板字段缺失/无审查人签名/无文件 hash 即 FAIL |
| C1 | 构建/feature/CI 环境检查（debug_assertions、panic=abort、target-cpu/FMA、feature 组合） | F-40、D-15 | 与声明不符即 FAIL |

**豁免流程（仅适用于非 DSP 区的“允许项”，不放宽 DSP 绝对禁止）**：

1. 只在 IO/CTL 代码可申请；申请必须写 `// rt-exempt: <RULE-ID> <reason> <owner> <date>` 并在 T06 证据中登记；
2. 必须附替代安全证据（预算测量/白盒审查/系统调用表）；
3. 由 integrator + 独立 reviewer 批准；DSP 区 F-01..F-44 一律**无豁免**；
4. 新增豁免必须升本文 v1.1.x 并在 §15.3 记录；不得在实现代码里静默豁免。

**仅审查项审计模板（R1 必填字段）**：审查人/日期/文件路径+SHA-256/行号/规则 ID/结论(PASS/FAIL)/替代证据（无自动工具时）/批准人。缺任一项 = 该规则 FAIL。



## 11. ABI v1（跨 RT 边界的字节契约）

### 11.1 通用规则（全部强制）

- A-ABI-1：所有跨 RT 边界结构必须 `#[repr(C)]`；共享于多线程/多核的必须 `#[repr(C, align(64))]`；不得用 Rust 默认布局、`repr(Rust)`、泛型参数、生命周期参数或 trait object。
- A-ABI-2：字段只用固定宽度整数（u8/u16/u32/u64/i8/i16）、f32、固定长度数组、`AtomicU32/AtomicU64`；**禁止** `bool`（用 u8 0/1）、带 payload 的 Rust enum、`usize/isize`、裸指针、`&T`/`&mut T`、`String/Vec/Box`。
- A-ABI-3：所有 reserved 字段必须写 0；读取时 reserved 非 0 = `RT_E_ABI_RUNTIME`（非 RT 校验阶段 = `ERR_BAD_LAYOUT`）。
- A-ABI-4：端序只支持小端（aarch64/x86 均 LE）；`cfg!(target_endian = "little")` 失败 → admission 拒绝 `ERR_BAD_LAYOUT`。f32 系数/样本在 plan 与 payload 中均为 IEEE-754 binary32 小端。
- A-ABI-5：结构版本由 `abi_version` 字段 + 全局 `ABI_VERSION=1` 双校验；不匹配 = 拒绝（非 RT）/fault（RT 运行期）。
- A-ABI-6：结构大小/偏移必须由 T06 的 `const fn` 静态断言锁定（`size_of::<T>() == 期望值`、`offset_of!` 期望值）；本文件附录 B 给出可复算的期望表（Python `struct` 独立计算）。
- A-ABI-7：所有“偏移”字段均指从 `PlanArena` 基址起的字节偏移；长度字段均为字节数；指针禁止出现在 ABI 结构里。
- A-ABI-8：系数 blob 每段起始必须 64 B 对齐；blob 之间允许 0 填充（不计入 `coeff_len_bytes`）；`coeff_offset_bytes + coeff_len_bytes` 必须落在 `total_coeff_bytes` 内。
- A-ABI-9：`graph_id` 是 plan 内容的 SHA-256（32 B），规则见 §11.10；`plan_version` 是运行期提交计数，二者都用于 stale/ack，禁止混用。
- A-ABI-10：运行期 f32 only（`precision=0`）；`precision=1`（f64）仅保留给设计器/reference 离线工具，admission 拒绝 `ERR_PRECISION`。

### 11.2 版本 / 魔数

| 项 | 值 |
|---|---|
| `ABI_VERSION` | 1 |
| `TSPI_PLAN_MAGIC` | 0x54535031（"TSP1"） |
| `TSPI_RING_MAGIC` | 0x54535052（"TSPR"） |
| `GRAPH_HASH_DOMAIN` | 字符串 `tspi-plan/v1` + 1 个 0x00 字节 |
| 结构 hash/校验 | `header_crc32c` = CRC-32C（Castagnoli，反射 0x82F63B78）覆盖 header 且自身字段置 0；`graph_id` 用 SHA-256（FIPS 180-4） |

| `PLAN_HEADER_SIZE` | 128 | TspiPlanHeaderV1 size（errata E1 后不变） |
| `PLAN_HEADER_ALIGN` | 64 | repr(C,align(64)) |
| `PLAN_HEADER_OFFSET_RESERVED1` | 94 | E1 显式 padding（u16） |
| `PLAN_HEADER_OFFSET_AFFINITY_TABLE` | 96 | affinity_table_offset（u32） |
| `PLAN_HEADER_OFFSET_AFFINITY_COUNT` | 100 | affinity_count（u16） |
| `PLAN_HEADER_OFFSET_RESERVED2` | 102 | E1 显式 padding（u16） |
| `PLAN_HEADER_OFFSET_CTL_META` | 104 | ctl_meta_offset（u32） |
| `PLAN_HEADER_OFFSET_CTL_META_LEN` | 108 | ctl_meta_len（u32） |
| `PLAN_HEADER_OFFSET_RESERVED3` | 112 | reserved3（[u32;3]） |
| `PLAN_HEADER_OFFSET_CRC` | 124 | header_crc32c（u32） |
| `RT_CHECK_ABI_LAYOUT_SCANNED` | 120 | T06 rt-check abi-layout 现行期望 scanned 数（旧 109，+11；task-105 实测）；105 是任务号，不是期望值 |

### 11.3 `TspiBlockDescV1`（64 B，align 64）

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `abi_version` | u16 | =1 |
| 2 | `block_size` | u16 | B=64/128 |
| 4 | `n_in` | u16 | 输入通道数 |
| 6 | `n_out` | u16 | 输出通道数 |
| 8 | `epoch_id` | u64 | 时间轴 ID |
| 16 | `block_seq` | u64 | epoch 内块序号 |
| 24 | `frame_index` | u64 | = block_seq × B |
| 32 | `plan_version` | u32 | 产生本块的 plan |
| 36 | `flags` | u32 | §6.2 位定义 |
| 40 | `valid_frames` | u32 | 0..=B |
| 44 | `slot_index` | u32 | payload 槽号 |
| 48 | `mono_publish_ns` | u64 | capture 发布时刻 |
| 56 | `play_deadline_ns` | u64 | 播放硬截止 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

### 11.4 `TspiEpochInfoV1`（64 B，align 64）

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `epoch_id` | u64 | 时间轴 ID |
| 8 | `plan_version` | u32 | 启动本 epoch 的 plan |
| 12 | `sample_rate` | u32 | 48000/96000 |
| 16 | `block_size` | u16 | 64/128 |
| 18 | `n_in` | u16 | 输入通道 |
| 20 | `n_out` | u16 | 输出通道 |
| 22 | `mode` | u16 | 0=usb_toslink、1=aes67、2=dante、3=file、4=other；只用于诊断 |
| 24 | `flags` | u64 | bit0=first_after_discontinuity、bit1=fallback_epoch、其余 0 |
| 32 | `start_mono_ns` | u64 | frame 0 的单调时刻 |
| 40 | `reserved` | [u64;3] | 必须 0 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

### 11.5 `TspiEventV1`（64 B，align 64）

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `seq` | u64 | lane 内序号 |
| 8 | `mono_ns` | u64 | I/O/CTL/WD 填充；DSP 事件写 0 |
| 16 | `epoch_id` | u64 | 关联 epoch |
| 24 | `block_seq` | u64 | 关联块；非块事件写 0 |
| 32 | `kind` | u16 | §8.2 |
| 34 | `lane` | u8 | 0..7 |
| 35 | `severity` | u8 | 1..4 |
| 36 | `value_a` | u32 | kind 语义 |
| 40 | `value_b` | u32 | kind 语义 |
| 44 | `reserved0` | u32 | 0 |
| 48 | `payload` | [u64;2] | kind 语义；未用 0 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

### 11.6 `TspiBlockAckV1`（64 B，align 64）

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `block_seq` | u64 | 本 ack 对应块 |
| 8 | `epoch_id` | u64 | 本 ack 对应 epoch |
| 16 | `phase_done[0]` | AtomicU32 | B1 完成位图（bit k=worker k） |
| 20 | `phase_done[1]` | AtomicU32 | B2 |
| 24 | `phase_done[2]` | AtomicU32 | B3 |
| 28 | `cutoff` | AtomicU32 | 1=无效块；worker 必须丢弃 |
| 32 | `plan_version` | u32 | 本块 plan |
| 36 | `expected_workers` | u8 | 4（2 核 profile 为 2） |
| 37 | `flags` | u8 | bit0=valid、bit1=late_seen、其余 0 |
| 38 | `reserved0` | u16 | 0 |
| 40 | `deadline_mono_ns` | u64 | 本块 hard cutoff |
| 48 | `compute_ticks` | u64 | master B1→B3 的 tick 差（预算内 2 次） |
| 56 | `reserved1` | u64 | 0 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

**v1.1/I03 注**：`expected_workers` ∈ {2,4}；phase 完成判定一律用 `ACK_MASK(expected)=(1<<expected)-1`（expected=4→0xF，expected=2→0x3）；位 0..expected-1 有效、其余必须 0。ack 槽带 (block_seq,epoch_id,plan_version) 标签；worker 写位前必须比对标签（I08，§7.3）。

### 11.7 `TspiSpscHeaderV1`（64 B，align 64）

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `magic` | u32 | `TSPI_RING_MAGIC` |
| 4 | `abi_version` | u16 | =1 |
| 6 | `entry_bytes` | u16 | 64（BlockDesc 大小） |
| 8 | `capacity` | u32 | 4 |
| 12 | `entry_stride` | u32 | 64（数组 stride） |
| 16 | `write_pos` | AtomicU64 | 生产者单调位置 |
| 24 | `read_pos` | AtomicU64 | 消费者单调位置 |
| 32 | `dropped` | AtomicU64 | 丢块计数 |
| 40 | `epoch_id` | AtomicU64 | 环当前 epoch |
| 48 | `reserved` | [u64;2] | 0 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

### 11.8 `TspiWorkerStatsV1`（64 B，align 64）

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `blocks_done` | u64 | 完成块数 |
| 8 | `busy_ticks` | u64 | 有用计算 tick（不含等待） |
| 16 | `spin_ticks` | u64 | 有界等待 tick |
| 24 | `max_block_ticks` | u64 | 最大单块 tick |
| 32 | `late_blocks` | u64 | 迟到导致 invalid 的次数 |
| 40 | `invalid_blocks` | u64 | 见到的 invalid 块次数 |
| 48 | `switches` | u64 | plan swap 次数 |
| 56 | `flags` | u64 | bit0=alive、bit1=last_block_late、bit2=cutoff_seen、其余 0 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

### 11.9 `TspiHwParamsV1`（64 B，align 64）与格式语义

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `abi_version` | u16 | =1 |
| 2 | `sample_format` | u16 | 见下表 |
| 4 | `n_in` | u16 | 设备输入通道 |
| 6 | `n_out` | u16 | 设备输出通道 |
| 8 | `sample_rate` | u32 | 48000/96000 |
| 12 | `period_frames` | u32 | ALSA period/actual chunk（B 的来源，必须 =64/128） |
| 16 | `buffer_frames` | u32 | ALSA buffer（≥ period；实际值由 T17 协商） |
| 20 | `in_stride_bytes` | u32 | 交错/平面缓冲跨通道字节步长（≥ period_frames × 样本字节） |
| 24 | `out_stride_bytes` | u32 | 同上 |
| 28 | `flags` | u32 | bit0=dither_enabled、bit1=dither_tpdf、bit2=capture_sanitize、bit3=output_clip_meter、其余 0 |
| 32 | `reserved` | [u64;4] | reserved[0]=dither_seed（非 RT 配置）；其余 0 |
| **64** | （大小） | | `size_of == 64`、`align_of == 64` |

**sample_format 枚举（v1 冻结）**：

| 值 | 名称 | 用途 | runtime 准入 |
|---:|---|---|---|
| 0 | `F32_PLANAR` | core 内部 canonical | 仅内部，不允许来自设备 |
| 1 | `S24_3LE_INTERLEAVED` | MOTU/ALSA 24-bit 常见 | 允许 |
| 2 | `S32_LE_INTERLEAVED` | ALSA 32-bit 容器 | 允许 |
| 3 | `S16_LE_INTERLEAVED` | 测试/兼容 | 允许（测试） |
| 4 | `F32_INTERLEAVED` | 文件/fake backend | 允许（file/fake） |
| 5 | `F64_PLANAR_REFERENCE` | 仅 reference/offline | 拒绝 `ERR_UNSUPPORTED` |

**stride/对齐规则**：core payload 是 planar f32；`in_stride_samples` / `out_stride_samples` 必须是 **16 的倍数** 且 ≥ B；每通道起始地址必须 64 B 对齐；不满足 = `ERR_STRIDE_ALIGN`。设备侧 interleaved→planar 的转换、符号扩展、饱和、finite 检查与 clip 统计属于 T22 format 模块（I/O 区，RT-safe 的预分配实现），其语义必须交付边界参考向量；本规范只固定“转换在 I/O 区、core 只见 f32 planar、stride 契约不变”。

**B 来源链（I11/v1.1，强制）**：`period_frames` 必须等于 T17 协商出的 **actual period**，且 ∈ {64,128}；不等于 64/128 时 T17 必须拒绝启动/重开设备并返回 `ERR_DEVICE_PARAMS`（不得静默夹取或内部拆分）；REST/T05 必须返回 supported 列表并对非 64/128 显式 400/unsupported。buffer_frames≥period_frames，实际值由 T17 记录；任何 period/rate/channel 变化必须新 epoch（§5.2）。

### 11.10 `TspiPlanHeaderV1`（128 B，align 64）与表

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `magic` | u32 | `TSPI_PLAN_MAGIC` |
| 4 | `abi_version` | u16 | =1（v1.1 additive，字段复用不得改 size；breaking 变更加 §11.11） |
| 6 | `header_size` | u16 | 128 |
| 8 | `plan_version` | u32 | 提交计数（不参与 graph_id） |
| 12 | `sample_rate` | u32 | 48000/96000 |
| 16 | `block_size` | u16 | 64/128（§3.2/Q-11） |
| 18 | `n_in` | u16 | 输入通道 |
| 20 | `n_out` | u16 | 输出（物理槽）数 |
| 22 | `low_band_count` | u16 | M（≤8） |
| 24 | `path_count` | u16 | 非零矩阵路径数（≤`MAX_MATRIX_PATHS`） |
| 26 | `diag_fir_count` | u16 | 对角 FIR 数（≤`MAX_DIAG_FIRS`） |
| 28 | `max_taps` | u32 | 本 plan 每 route 有效支撑上界（≤16384） |
| 32 | `workers` | u8 | 4 或 2（2=独立降级 profile，expected_workers 同步 2） |
| 33 | `flags` | u8 | bit0=stereo_strict、bit1=modal_support_to_other_outputs、bit2=crossfade（须配 §11.13 CrossfadeBudget）、bit3=extended_taps_admitted、bit4=fallback_graph、**bit5..7=0（reserved；v1.1.3 errata E4 取消该位的 affinity 表存在性语义）** |
| 34 | `precision` | u8 | 0=f32（v1 唯一允许） |
| 35 | `reserved0` | u8 | 0 |
| 36 | `graph_id` | [u8;32] | §11.10 规则计算 |
| 68 | `total_coeff_bytes` | u32 | 系数区总字节 |
| 72 | `path_table_offset` | u32 | Path 表偏移 |
| 76 | `diag_table_offset` | u32 | Diag 表偏移 |
| 80 | `in_stride` | u32 | 输入通道 stride（样本，16 倍数） |
| 84 | `out_stride` | u32 | 输出 stride（样本，16 倍数） |
| 88 | `role_table_offset` | u32 | Role 表偏移 |
| 92 | `role_table_count` | u16 | role 数（≤16） |
| 94 | `reserved1` | u16 | 0（**errata E1 显式 padding**：把 u32 对齐到 96） |
| 96 | `affinity_table_offset` | u32 | **v1.1/I15 + E1**：`TspiWorkerAffinityV1` 表偏移（0=无） |
| 100 | `affinity_count` | u16 | **v1.1/I15 + E1**：affinity 条目数（≤4） |
| 102 | `reserved2` | u16 | 0（**errata E1 显式 padding**：把 u32 对齐到 104） |
| 104 | `ctl_meta_offset` | u32 | role 名等 CTL 元数据偏移（E1：102→104） |
| 108 | `ctl_meta_len` | u32 | 元数据字节数（E1：106→108） |
| 112 | `reserved3` | [u32;3] | 0 |
| 124 | `header_crc32c` | u32 | 覆盖 header（本字段置 0） |
| **128** | （大小） | | `size_of == 128`、`align_of == 64` |

> v1.1 注意：原 §11.10 的 reserved 区（88–124）被重新划分为 role/affinity/ctl 字段；大小仍 128、字段语义 additive。**errata E1（2026-11-17，T105 实现确认，v1.1.2）**：v1.1 初版把 affinity_table_offset/ctl_meta_offset/ctl_meta_len 写成 u32@94/102/106——u32 在 94/102/106 非 4 字节对齐，Rust repr(C) 不可表达（必然隐式 padding）；T105 按语义顺序落地合法版：`reserved1@94` / `affinity_table_offset@96` / `affinity_count@100` / `reserved2@102` / `ctl_meta_offset@104` / `ctl_meta_len@108` / `reserved3@112` / `header_crc32c@124`，size 128/align 64 不变。**旧 94/98/102/106 作废**；B.4/abi_vectors/rt-check 全部按 96/100/104/108。T06 rt-check abi-layout 现行期望 `scanned=120`（旧 109，+11；task-105 实测日志），105 为任务号非期望值。
>
> **errata E3/E4（v1.1.3，以实现为准）**：Path/Diag 偏移 28 与 Role 偏移 24 **仍为 reserved**（`reserved1`/`reserved1`/`reserved3`，必须 0；`delay_q16` 未落地，见 §0.1 E3）；**plan flags bit5..7=0**（`affinity_table_present` 位义取消，affinity 表存在性按 `affinity_table_offset`/`affinity_count` 判定，见 §0.1 E4）；`swap.rs` 的 `PLAN_FLAG_MASK` 已在 task-207 收窄为 **0x1F**（bit5..7 置位拒收，见 §0.1 errata E5）。

**`TspiPathV1`（32 B，align 8）**：

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `in_index` | u16 | 输入通道索引（<N） |
| 2 | `out_index` | u16 | 物理输出索引（<O） |
| 4 | `taps` | u32 | 1..=max_taps |
| 8 | `delay_samples` | u32 | **整数部分**输出延迟 D_int（含在支撑内） |
| 12 | `polarity` | i8 | 1 或 -1 |
| 13 | `flags` | u8 | bit0=diag_baseline_included、bit1=fused、bit2=zero_pad_linear_conv、bit3=input_side_delay、其余 0 |
| 14 | `reserved0` | u16 | 0 |
| 16 | `coeff_offset_bytes` | u64 | 系数 blob 偏移（64B 对齐） |
| 24 | `coeff_len_bytes` | u32 | = taps × 4（f32） |
| 28 | `reserved1` | u32 | **0（v1.1.3 errata E3：`delay_q16` 未落地，与 `abi.rs` reserved1 一致；ABI v1 不承载次采样 D）** |
| **32** | （大小） | | `size_of == 32`、`align_of == 8` |

**`TspiDiagFirV1`（32 B，align 8）**：

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `out_index` | u16 | 物理输出索引 |
| 2 | `flags` | u16 | bit0=protection、bit1=limiter、bit2=crossover、bit3=target、bit4=phase_delay_polarity_only、其余 0 |
| 4 | `taps` | u32 | 1..=max_taps |
| 8 | `delay_samples` | u32 | 整数部分延迟 D_int |
| 12 | `reserved0` | u32 | 0 |
| 16 | `coeff_offset_bytes` | u64 | 系数 blob 偏移（64B 对齐） |
| 24 | `coeff_len_bytes` | u32 | = taps × 4 |
| 28 | `reserved1` | u32 | **0（v1.1.3 errata E3：同 Path，`delay_q16` 未落地）** |
| **32** | （大小） | | `size_of == 32`、`align_of == 8` |

**`TspiRoleV1`（32 B，align 8）**：

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `role_id` | u16 | 0..15（plan 内序号） |
| 2 | `out_index` | u16 | 物理输出索引（-1 无效用 0xFFFF） |
| 4 | `flags` | u16 | bit0=enabled、bit1=zero_fill（无节目时必须显式零填）、其余 0 |
| 6 | `reserved0` | u16 | 0 |
| 8 | `delay_samples` | u32 | 角色级整数延迟（0..9600@48k，200 ms 上限） |
| 12 | `gain_q15` | i16 | Q15 线性增益（1.0=32767） |
| 14 | `polarity` | i8 | 1/-1 |
| 15 | `reserved1` | u8 | 0 |
| 16 | `peq_count` | u16 | 角色级 IIR 段数（≤32；系数在 ctl_meta/节点区） |
| 18 | `reserved2` | u16 | 0 |
| 20 | `name_hash` | u32 | role 名 CRC-32C |
| 24 | `reserved3` | u32 | **0（v1.1.3 errata E3：同 Path，`delay_q16` 未落地）** |
| 28 | `reserved4` | u32 | 0 |
| **32** | （大小） | | `size_of == 32`、`align_of == 8` |

**表顺序与 graph_id 规范化（v1 冻结；v1.1.3 errata E3：delay_q16 未进 ABI）**：

1. Path 表按 `(out_index, in_index)` 升序、无重复；每个路径 blob = `taps × 4` 字节 f32 LE，tap 0..taps-1；blob 起点 64B 对齐；分数 D 已折入系数（ABI v1 无 `delay_q16` 槽，E3），`reserved1` 必须 0 且不承载语义。
2. Diag 表按 `out_index` 升序、无重复；blob/延迟规则同上。
3. Role 表按 `role_id` 升序；ctl_meta = 按 role 顺序 `[u16 name_len][UTF-8 bytes]` + 0 填充到 4B；name_hash 为 UTF-8 名字 CRC-32C；affinity 表（若 `affinity_table_offset != 0 / affinity_count > 0`；E4 取消 flags bit5 判据）按 worker_id 升序，每项 32B。
4. `graph_id = SHA-256(`GRAPH_HASH_DOMAIN` || header_with_graph_id_zeroed_and_crc_zeroed || path_table || diag_table || role_table || affinity_table || ctl_meta || 系数 blob 按表顺序拼接)`；未使用区域不计入；计算后写回 graph_id，再算 header_crc32c。**graph_id 不包含 plan_version/内存地址/时间戳**。
5. 任何 P/G/role/fs/B/D/taps/系数/worker 数/stereo mask/affinity 变化都会改变 graph_id（D 只取整数 `delay_samples`；Path/Diag/Role 的 reserved 槽必须 0，不承载语义）；stale 判定比较 graph_id（或 graph_id+plan_version），不得只比 plan_version。
6. **总时延口径（I01；v1.1.3 errata E3 以实现为准）**：`total_delay_q16(path/role) = delay_samples*65536 + delay_q16` 是内部/报告口径（见 §12.3）。**ABI v1 的 Path/Diag/Role 无 `delay_q16` 槽**（28/28/24 为 reserved，必须 0）；T12 编译 T04 半整数/分数 D 时必须在打包 ABI 前把分数部分折入 P/A 系数（或取整），`delay_samples` 只承载整数部分；T66 对非零 q16 按 `E_CA_DELAY_Q16_ABI` 拒收（GAP-T66-2）。crossfade D 对齐与 T20/报告一律用 total_delay_q16，ABI v1 下低 16 位恒 0（等价于整数 D 相等）。

**plan flags 语义（v1.1 增补；v1.1.3 errata E4 统一位表）**：bit0 stereo_strict、bit1 modal_support_to_other_outputs（默认 0）、bit2 crossfade（必须配 TspiCrossfadeBudgetV1 且 admission 通过；否则 ERR_CROSSFADE_BUDGET）、bit3 extended_taps_admitted、bit4 fallback_graph、**bit5..7=0（reserved；E4 取消该位的 affinity 表存在性语义）**。A_map/禁止上混下混规则不变（Path 表即非零路径集合；未声明输出必须零填）。

### 11.11 ABI 兼容、版本与 FFI

- 不兼容变更加 `ABI_VERSION`；plan/ring 的 `abi_version` 不匹配 = 拒绝，禁止运行期迁移或“尽力解析”。
- v1 RT 路径**无 FFI**：所有跨模块传递走上述 POD 结构/arena/文件；若未来引入 C ABI/插件：必须 `extern "C"`、固定 `repr(C)` 版本化结构、ptr+len+stride/alignment 显式、禁止 Vec/String/trait 跨界、panic 不得穿过边界（在非 RT 边界 `catch_unwind`）。引入前必须升 `1.1` 并补 T06 静态断言。
- 文件/CLI 层（WAV/JSON/YAML）不属于 RT ABI：由 T13/T54/T66 在 CTL 解析、校验、转成 plan；RT 永不解析文本。

### 11.12 ABI 检查表（B-01..B-19；v1.1 因 I02/I03/I06/I15 扩展）

| ID | 检查项 | 状态 | 证据/未来验证 |
|---|---|---|---|
| B-01 | `ABI_VERSION/魔数/域分离字符串` 固定 | PASS-DESIGN | §11.2 |
| B-02 | 全部结构大小/偏移/对齐给出并独立复算（v1.1：11 原有+7 新=18 结构） | PASS-DESIGN | §11.3–§11.10、§11.13 + evidence/T02/abi_vectors.py |
| B-03 | 无 bool/enum payload/指针/usize/字符串跨边界（指针只在明确说明的 AtomicU64 位模式中） | PASS-DESIGN | §11.1/§11.13 |
| B-04 | 原子字段类型/偏移/内存序固定 | PASS-DESIGN | §11.6/§11.7/§11.13 |
| B-05 | stride/对齐/64B blob 对齐规则固定 | PASS-DESIGN | §11.9/§11.10 |
| B-06 | f32 LE 系数/sample 规则 + 端序检查 | PASS-DESIGN | §11.1 |
| B-07 | graph_id 规范化与 hash 域固定（v1.1 纳入 affinity；v1.1.3 E3：delay_q16 未进 ABI） | PASS-DESIGN | §11.10 |
| B-08 | plan_version 与 graph_id 分工明确、stale 判定双条件 | PASS-DESIGN | §5.3/§11.10 |
| B-09 | FFI v1 无；未来规则固定 | PASS-DESIGN | §11.11 |
| B-10 | ABI 不匹配拒绝策略（非 RT/RT 两路） | PASS-DESIGN | §9.2/§11.11 |
| B-11 | sample_format 枚举与 runtime 准入固定 | PASS-DESIGN | §11.9 |
| B-12 | T06 静态断言清单（size/offset/needs_drop/align） | DEFERRED-CODE | T06；本文件给出期望值（A/B.4） |
| B-13 | `TspiRtControlV1` 128B/offset/原子语义/block_start_seq 协议 | PASS-DESIGN | §11.13 |
| B-14 | `TspiSwapAckV1` 64B/swap ack all-worker 语义 | PASS-DESIGN | §11.13 |
| B-15 | `TspiEventRingLaneV1` 64B/lane 单写者/seq 发布 | PASS-DESIGN | §11.13 |
| B-16 | `TspiWorkerRegsV1/TspiMasterRegsV1` 128B/RT 入口参数布局 | PASS-DESIGN | §11.13 |
| B-17 | `TspiCrossfadeBudgetV1` 64B/D 对齐与双峰值预算字段 | PASS-DESIGN | §11.13 |
| B-18 | `TspiWorkerAffinityV1` 32B/align 8（**align 例外，plan POD 表项**）+ PlanHeader affinity 字段（I15/T45） | PASS-DESIGN | §11.10/§11.13 |
| B-19 | ack 参数化静态断言：expected∈{2,4} 时 `ACK_MASK` 与位图常量一致；禁止硬写 0xF | PASS-DESIGN | §3.4/§7.3/§11.6 |

### 11.13 RT 共享控制结构（v1.1.1/I02/I05/I06/I15；除 `TspiWorkerAffinityV1` 32B/align8 外均 repr(C,align(64))、needs_drop=false）

> 以下结构跨 master/worker/CTL 共享，但**不是拥有型句柄**：无 Drop、无指针语义（除明确说明的 arena 偏移）；RT 只读写原子字段或按 §7.3 配方读写普通字段。所有 reserved 必须写 0；越界/非 0 = `RT_E_ABI_RUNTIME`（RT）/`ERR_BAD_LAYOUT`（非 RT）。

**`TspiRtControlV1`（128 B，align 64）**

| 偏移 | 字段 | 类型 | 语义/内存序 |
|---:|---|---|---|
| 0 | `magic` | u32 | `TSPI_RING_MAGIC`（复用，区分 ring） |
| 4 | `abi_version` | u16 | =1 |
| 6 | `size` | u16 | 128 |
| 8 | `block_start_seq` | AtomicU64 | master 每 block 结束描述写入后 Release fetch_add；worker Acquire 读 |
| 16 | `block_start_epoch` | AtomicU64 | 本 block epoch_id |
| 24 | `block_start_frame` | AtomicU64 | frame_index |
| 32 | `block_start_plan_version` | AtomicU32 | 本 block plan_version |
| 36 | `block_start_slot` | AtomicU32 | 本 block 在 RtArena 的 payload/ack 槽 |
| 40 | `current_plan_ptr` | AtomicU64 | 当前 plan arena 基址（0=null）；只 master 写、worker 读 |
| 48 | `current_plan_version` | AtomicU32 | 与 ptr 同步发布 |
| 52 | `pending_plan_version` | AtomicU32 | ARMED 时新 plan 版本 |
| 56 | `pending_plan_ptr` | AtomicU64 | ARMED 时新 plan 基址 |
| 64 | `switch_state` | AtomicU32 | 0=IDLE、1=ARMED、2=SWITCHED、3=ABORT（与 §5.3 一致） |
| 68 | `safety_request` | AtomicU32 | 0=none、1=mute、2=fault；只置位 |
| 72 | `safety_reason` | u32 | 最后一个 RtStatus 原因 |
| 76 | `flags` | u32 | bit0=rt_mode、bit1=two_worker、其余 0 |
| 80 | `heartbeat_seq` | AtomicU64 | master 每 block Release fetch_add；watchdog 只读 |
| 88 | `spin_max_iters` | AtomicU32 | admission 标定值（T18）；master 读 |
| 92 | `cutoff_drain_iters` | AtomicU32 | admission 标定值 |
| 96 | `tick_freq_hz` | u32 | tick→ns 换算（admission 标定） |
| 100 | `tick_read_budget` | u32 | ≤3（§3.4） |
| 104 | `capture_ring_offset` | u64 | RtArena 偏移 |
| 112 | `playback_ring_offset` | u64 | RtArena 偏移 |
| 120 | `reserved` | u64 | 0 |
| **128** | （大小） | | `size_of == 128`/`align_of == 64` |

**`TspiSwapAckV1`（64 B，align 64）**

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `plan_version` | u32 | 待 ack 的 plan |
| 4 | `expected_workers` | u8 | 2/4 |
| 5 | `flags` | u8 | bit0=retire_ready |
| 6 | `reserved0` | u16 | 0 |
| 8 | `ack_mask` | AtomicU32 | worker k 完成新版本首块位；用 ACK_MASK(expected) |
| 12 | `retire_ready` | AtomicU32 | 1=所有 ack 已到，可由 CTL retire |
| 16 | `retire_epoch` | u64 | 旧 plan 使用结束的 epoch |
| 24 | `next_plan_version` | u32 | 当前生效版本 |
| 28 | `reserved1` | u32 | 0 |
| 32 | `old_plan_ptr` | u64 | 旧 arena 基址（retire 目标） |
| 40 | `reserved2` | u64 | 0 |
| 48 | `reserved3` | u64 | 0 |
| 56 | `reserved4` | u64 | 0 |
| **64** | （大小） | | `size_of == 64`/`align_of == 64` |

**`TspiEventRingLaneV1`（64 B，align 64）**

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `write_seq` | AtomicU64 | 本 lane 生产者 Release 发布 |
| 8 | `read_seq` | AtomicU64 | 非 RT 读者 |
| 16 | `dropped` | AtomicU64 | 满丢弃计数 |
| 24 | `overflow` | AtomicU64 | seq 回绕/非法计数 |
| 32 | `records_offset` | u32 | RtArena 中 64B 记录数组偏移 |
| 36 | `capacity` | u32 | =128（每 lane） |
| 40 | `lane` | u8 | 0..7 |
| 41 | `flags` | u8 | bit0=single_writer（必须 1） |
| 42 | `reserved0` | u16 | 0 |
| 44 | `reserved1` | u32 | 0 |
| 48 | `reserved2` | u64 | 0 |
| 56 | `reserved3` | u64 | 0 |
| **64** | （大小） | | `size_of == 64`/`align_of == 64` |

**`TspiWorkerRegsV1`（128 B，align 64，每 worker 一份）**

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `worker_id` | u32 | 0..3 |
| 4 | `expected_workers` | u8 | 2/4 |
| 5 | `flags` | u8 | bit0=rt_worker、其余 0 |
| 6 | `reserved0` | u16 | 0 |
| 8 | `cpu_index` | u32 | T45 admission 写入的实际 CPU |
| 12 | `sched_priority` | u32 | FIFO 优先级（初值 70，T45 校正） |
| 16 | `spin_max_iters` | u32 | 标定值 |
| 20 | `cutoff_drain_iters` | u32 | 标定值 |
| 24 | `rt_control_ptr` | u64 | `TspiRtControlV1` 基址 |
| 32 | `plan_arena_base` | u64 | 当前/待用 plan arena 基址 |
| 40 | `ack_slots_offset` | u64 | RtArena ack 槽数组偏移 |
| 48 | `stats_offset` | u64 | `TspiWorkerStatsV1` 偏移 |
| 56 | `event_lane_offset` | u64 | lane header 偏移 |
| 64 | `required_ack_mask` | u32 | ACK_MASK(expected) |
| 68 | `reserved1` | u32 | 0 |
| 72 | `scratch_offset` | u64 | worker 私有 scratch |
| 80 | `scratch_len` | u64 | scratch 字节 |
| 88 | `affinity_index` | u32 | 指向 plan affinity 表 sp（0xFFFFFFFF=无） |
| 92 | `reserved2` | u32 | 0 |
| 96..128 | reserved3..6 | [u64;4] | 0 |
| **128** | （大小） | | `size_of == 128`/`align_of == 64` |

**`TspiMasterRegsV1`（128 B，align 64）**

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `rt_control_ptr` | u64 | `TspiRtControlV1` |
| 8 | `capture_ring_offset` | u64 | capture ring header 偏移 |
| 16 | `playback_ring_offset` | u64 | playback ring header 偏移 |
| 24 | `plan_registry_ptr` | u64 | CTL PlanRegistry 句柄（仅事件/诊断用，RT 不得解引用为 Drop 类型） |
| 32 | `ack_base_offset` | u64 | ack 槽数组 |
| 40 | `stats_offset` | u64 | stats 数组 |
| 48 | `event_lane_offset` | u64 | lane 0..7 header 基址 |
| 56 | `worker_regs_offset` | u64 | WorkerRegs 数组 |
| 64 | `crossfade_budget_offset` | u64 | `TspiCrossfadeBudgetV1` 偏移（0=无） |
| 72 | `tick_freq_hz` | u32 | 标定 |
| 76 | `tick_read_budget` | u32 | ≤3 |
| 80 | `expected_workers` | u8 | 2/4 |
| 81 | `flags` | u8 | bit0=crossfade_enabled、其余 0 |
| 82 | `reserved0` | u16 | 0 |
| 84 | `reserved1` | u32 | 0 |
| 88..128 | reserved2..6 | [u64;5] | 0 |
| **128** | （大小） | | `size_of == 128`/`align_of == 64` |

**`TspiCrossfadeBudgetV1`（64 B，align 64，I06）**

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `old_plan_version` | u32 | 旧 plan |
| 4 | `new_plan_version` | u32 | 新 plan |
| 8 | `old_total_delay_q16` | u32 | 旧 total_delay_q16（ABI v1 下 = 整数总延迟 << 16；E3） |
| 12 | `new_total_delay_q16` | u32 | 新 total_delay_q16；**必须相等**（ABI v1 下低 16 位恒 0，即整数 D 相等；E3） |
| 16 | `crossfade_frames` | u32 | ≤128 |
| 20 | `flags` | u32 | bit0=admission_passed、bit1=d_aligned、其余 0 |
| 24 | `peak_cpu_ns` | u64 | 双图峰值 CPU |
| 32 | `peak_ram_bytes` | u64 | 双图峰值 RAM |
| 40 | `available_cpu_ns` | u64 | admission 可用预算 |
| 48 | `available_ram_bytes` | u64 | admission 可用 RAM |
| 56 | `reserved` | u64 | 0 |
| **64** | （大小） | | `size_of == 64`/`align_of == 64` |

> **errata E3 注（v1.1.3）**：`old/new_total_delay_q16` 字段名与布局不变（与实现 `rt-host/src/swap.rs` 一致），但 ABI v1 的 Path/Diag/Role 无 `delay_q16` 槽（reserved=0），故该值 = `整数总延迟 D << 16`（低 16 位恒 0）；未来若引入分数 D 必须升 ABI 版本并同步改本表。

**`TspiWorkerAffinityV1`（32 B，align 8，I15/T45；N4 align 例外）**

| 偏移 | 字段 | 类型 | 语义 |
|---:|---|---|---|
| 0 | `worker_id` | u8 | 0..3 |
| 1 | `sched_priority` | u8 | FIFO 初值 70 |
| 2 | `flags` | u16 | bit0=rt、bit1=isolated_cpu |
| 4 | `cpu_index` | u16 | 实际拓扑 CPU |
| 6 | `reserved0` | u16 | 0 |
| 8 | `irq_dependency_mask` | u64 | 依赖的 IRQ/设备位图（T45 定义位义） |
| 16 | `reserved1` | u64 | 0 |
| 24 | `reserved2` | u64 | 0 |
| **32** | （大小） | | `size_of == 32`/`align_of == 8` |

> **align 例外（v1.1.1/N4）**：`TspiWorkerAffinityV1` 是 plan 内 POD affinity 表项，固定 **32 B / align 8**（不是跨线程共享原子结构）；T06 B-18 静态断言必须期望 `size_of=32 / align_of=8`，不得按 §11.13 标题的 align64 误期。其余 6 个共享结构按 align(64)。

**一致性与生命周期规则**：RtControl/SwapAck/EventRingLane/WorkerRegs/MasterRegs/CrossfadeBudget/WorkerAffinity 全部在 admission 时一次性分配并 mlock；地址稳定到进程退出；RT 内不得分配/释放/替换；无 Drop；只允许 §11 说明的字段读写。**align 例外**：WorkerAffinity 为 32B/align8（plan POD 表项），其余共享结构 align64（N4）。block_start 配方、ack 标签、事件 seq 发布顺序见 §6.4/§7.3/§8.3。这些结构纳入 §13.2 needs_drop=false 清单与 B-13..B-19 静态断言。

> **errata E1 后 ABI 期望**：TspiPlanHeaderV1 128/64，关键偏移 role_off=88、role_count=92、reserved1=94、affinity_off=96、affinity_count=100、reserved2=102、ctl_meta_off=104、ctl_meta_len=108、reserved3=112、crc=124；T06 rt-check abi-layout scanned=120（旧 109，+11）。详见 §11.10/B.4。

## 12. Rust API 表面（拟建，future；T06 必须以此建 stub 与 lint）

### 12.1 crate / module 映射（与 WBS T06/T26 一致）

| crate | RT 相关 module | 非 RT module | 主要任务 |
|---|---|---|---|
| `khaen-core` | `rt`、`direct`、`partitioned`、`workers` | `plan`、`compile`、`nodes`、`schedule_plan` | T07/T08/T09/T12/T23/T56 |
| `khaen-rt-host` | `spsc`、`clock`（rt_tick）、`swap`（RT 侧 handoff） | `admission`、`alsa`、`clock`（mapping）、`fault`、`format`、`aoip`、`scheduler`、`main` | T16/T17/T18/T19/T20/T21/T22/T43/T45/T26 |
| `khaen-compat` | 无 RT 执行模块（只产出 plan/配置） | `ws`（T24）、`calibration`（T13）、`content`（T25 内容状态）、`roles`（T57）、`channel_map`（T25） | T13/T24/T25/T57 |
| `khaen-designer` | 无 | 全部设计器模块 | T10/T11/T33–T41/T55 |
| `khaen-control` | 无 | `rest`、`modes`、`web` | T42/T44/T70 |
| `khaen-calibration` | 无（daemon 不在 RT 区） | `job/sweep/capture/positions/peq_fit/phase_fit/compile_apply` | T60–T66 |
| `khaen-modal-design` | 无 | `ingest/timebase/target/solve/regularize/validate/artifact` | T48–T54/T73 |
| `khaen-file-runner` | 无（file runner 模拟 I/O，不算生产 RT） | 全部 | T14 |
| `khaen-bench` | 无 | 全部（测量工具） | T47 |

> 归属：任何 crate 新增 RT 执行模块必须 §12.3 的 `[RT]` 规则 + T06 lint 白名单，否则拒绝。T06 只建 scaffold；本表是结构约束，不是实现完成声明。

### 12.2 标注规则（必须出现在文档注释/文件头）

| 标注 | 含义 | 允许调用者 | 允许被调 |
|---|---|---|---|
| `[RT]` | 可在 DSP RT 区执行 | master/worker/RT 主干 | 仅 `[RT]` + 纯计算 + 白名单原子/内存操作 |
| `[IO]` | 可在 I/O 区执行 | capture/playback | `[IO]` + `[RT]` 纯计算（不得反向） |
| `[NONRT]` | 只能在控制/CTL 执行 | T06/T12/T18/T20/T42/T60/T66 等 | 任意（但不得把 Drop/分配带进 RT） |
| `[WD]` | watchdog 区 | watchdog | 只读原子/事件 + 置位安全请求 |
| `[TEST]` | 测试/bench 专用；admission 必须拒绝进入生产 RT | 测试 | 任意 |

T06 的 `rt-check` 未来命令（实现后必须真实执行，本文不冒充已运行）：

~~~text
rt-check abi-layout      # 静态断言 size/offset/align/needs_drop 与 §11 表一致
rt-check forbid          # 扫描 [RT] 区域 F-01..F-44 + 禁止符号
rt-check drop-audit      # §13 类型清单/无 Drop/无 Arc/无 dealloc 的静态审计
rt-check plan-hash       # 用附录 B 向量验证 graph_id/CRC 规范化
rt-check state-machine   # 状态迁移表/事件完整性模型测试（T21/T26 复用）
~~~

### 12.3 关键类型（拟建，签名冻结；实现可改内部细节但不得改语义）

~~~text
// khaen-core::rt
pub type WorkerId = u8;                 // 0..=3
pub type PlanVersion = u32;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RtStatus(pub u32);           // Copy; needs_drop == false

impl RtStatus {
    pub const OK: RtStatus = RtStatus(0);
    pub const fn is_ok(self) -> bool { self.0 == 0 }
    pub const fn code(self) -> u32 { self.0 & 0xFFFF }
}

// 非拥有、无 Drop 的 plan 借用（只在 block 内有效，见 INV-L1/INV-L2）
#[derive(Clone, Copy)]
pub struct PlanRef {
    ptr: core::ptr::NonNull<PlanHeaderV1>,   // 指向 mlock 的 PlanArena
    version: PlanVersion,
}
impl PlanRef {
    // [RT] 只读 view；调用者保证 ptr/version 与 current_plan_ptr 一致
    pub unsafe fn view(self) -> PlanView<'static>;
    pub fn version(self) -> PlanVersion;
}

// [NONRT] 由 plan arena 借出的只读视图（references，无 Drop）
#[derive(Clone, Copy)]
pub struct PlanView<'a> {
    pub header: &'a PlanHeaderV1,
    pub paths: &'a [PathV1],
    pub diags: &'a [DiagFirV1],
    pub roles: &'a [RoleV1],
    pub coeffs: &'a [u8],
}

#[derive(Clone, Copy)]
pub struct BlockIo<'a> {
    pub desc: &'a TspiBlockDescV1,
    pub input: &'a [f32],                  // planar, in_stride
    pub output: &'a mut [f32],             // planar, out_stride
    pub in_stride: u32,
    pub out_stride: u32,
}

#[derive(Clone, Copy)]
pub struct WorkerRtCtx<'a> {
    pub worker: WorkerId,
    pub ack: &'a TspiBlockAckV1,           // 原子字段在内部，不拥有
    pub stats: &'a mut TspiWorkerStatsV1,
}

// [RT] 主入口：worker/master 的 block 处理
pub fn worker_block(k: WorkerId, plan: PlanRef, io: BlockIo<'_>, ctx: &mut WorkerRtCtx<'_>) -> RtStatus;
pub fn master_block(plan: PlanRef, io: BlockIo<'_>, ctl: &TspiRtControlV1, ack: &TspiBlockAckV1) -> RtStatus;
pub fn kernel_direct(plan: PlanRef, io: &BlockIo<'_>) -> RtStatus;      // T07
pub fn kernel_partitioned(plan: PlanRef, io: &BlockIo<'_>) -> RtStatus; // T08
~~~

~~~text
// khaen-rt-host::spsc  [RT] 纯无锁、固定容量、无分配
pub struct Producer<'a> { /* &'a TspiSpscHeaderV1 + &'a [TspiBlockDescV1] + &'a mut [BlockPayload] */ }
pub struct Consumer<'a> { /* 同上只读位 */ }
impl<'a> Producer<'a> {
    pub fn try_push(&mut self, desc: TspiBlockDescV1) -> Result<(), RtStatus>; // Full -> RT_E_QUEUE_OVERRUN
}
impl<'a> Consumer<'a> {
    pub fn try_pop(&mut self) -> Option<TspiBlockDescV1>;
}

// khaen-rt-host::swap  [NONRT] plan 注册/切换/退役（Drop 只在这里）
pub struct PlanRegistry { /* owns PlanOwner(s) */ }
impl PlanRegistry {
    pub fn admit(&mut self, token: PlanToken, env: &AdmissionEnv) -> Result<PlanVersion, RtStatus>;
    pub fn stage(&mut self, version: PlanVersion) -> Result<StageToken, RtStatus>;
    pub fn abort(&mut self, token: StageToken) -> RtStatus;
    // [NONRT] 轮询 retire gate；只有返回 Some 才允许 drop/unmap 旧 arena
    pub fn collect_retired(&mut self) -> Option<PlanVersion>;
}

// khaen-rt-host::admission  [NONRT]
pub struct AdmissionEnv { pub cpu_set_mask: u64, pub allowed_cpu: [u16;4], pub expected_workers: u8,
                             pub rlimit_memlock: u64, pub stack_bytes: [u32;4], pub clock_source: u8,
                             pub crossfade_budget_required: bool }
pub struct AdmissionReport { pub plan_version: PlanVersion, pub arena_bytes: u64,
                             pub mlocked_bytes: u64, pub stack_bytes: u64,
                             pub workers: u8, pub graph_id: [u8; 32],
                             pub expected_workers: u8, pub ack_mask: u32,
                             pub actual_cpu: [u16;4], pub sched_priority: [u8;4],
                             pub irq_dependency_mask: u64, pub tick_freq_hz: u32,
                             pub spin_max_iters: u32, pub cutoff_drain_iters: u32,
                             pub affinity_hash: [u8;8], pub crossfade_accepted: bool }
pub fn admit(plan: &PlanView<'_>, env: &AdmissionEnv) -> Result<AdmissionReport, RtStatus>;

// khaen-rt-host::clock
pub fn mono_now_ns() -> u64;                  // [IO]/[NONRT]
pub fn rt_tick_budgeted(ctl: &TspiRtControlV1) -> u64;     // [RT] 仅 master，≤3 次/block
pub fn ticks_to_ns(ticks: u64) -> u64;        // [NONRT] admission 标定的频率换算

// khaen-rt-host::format  [IO]
pub struct FormatConfig { pub hw: TspiHwParamsV1, pub dither: DitherCfg }
pub struct FormatStats { pub nonfinite_in: u64, pub clipped: u64, pub nonfinite_out: u64 }
pub fn convert_capture(cfg: &FormatConfig, src: &[u8], dst: &mut [f32]) -> Result<FormatStats, RtStatus>;
pub fn convert_playback(cfg: &FormatConfig, src: &[f32], dst: &mut [u8]) -> Result<FormatStats, RtStatus>;

// khaen-rt-host::fault  [WD]/[NONRT]
pub struct Watchdog { /* 观测 heartbeat/epoch/worker stats，只置位 */ }
impl Watchdog {
    pub fn observe(&mut self, ctl: &TspiRtControlV1, swap: &TspiSwapAckV1) -> WatchdogAction; // None | RequestMute | Fault
}
~~~

~~~text
// khaen-core::plan  [NONRT] 构建/校验/hash（可分配；不得被 RT 调用）
pub struct PlanBuf { /* 拥有 plan 字节；Drop 只在 CTL */ }
pub fn validate_plan(bytes: &[u8]) -> Result<PlanBuf, RtStatus>;
pub fn recompute_graph_id(bytes: &[u8]) -> Result<[u8; 32], RtStatus>;
pub fn pack_plan(header: &TspiPlanHeaderV1, tables: &PlanTables, coeffs: &[f32]) -> Result<Vec<u8>, RtStatus>;

// khaen-core::workers  [RT] 线程入口
pub fn run_worker(k: WorkerId, regs: &TspiWorkerRegsV1) -> !;
pub fn run_master(regs: &TspiMasterRegsV1, ctl: &TspiRtControlV1) -> !;
~~~

~~~text
// khaen-core::rt  [RT] v1.1 增补（I03/I04/I05/I06；I01 为内部口径，ABI v1 不承载，E3）
pub const ACK_MASK_ALL: u32 = 0xF;                       // 仅 expected=4；运行期用 ack_mask()
pub const fn ack_mask(expected_workers: u8) -> u32 { (1u32 << expected_workers) - 1 }
pub const fn total_delay_q16(int_part: u32, frac_q16: u32) -> u64 { ((int_part as u64) << 16) | frac_q16 as u64 }  // 内部/报告口径；ABI v1 下 frac_q16 恒 0（E3）

#[RT] pub fn wait_block_start(ctl: &TspiRtControlV1, last_seq: u64) -> Result<u64, RtStatus>; // Acquire; Err=cutoff/timeout
#[RT] pub fn read_block_desc(ctl: &TspiRtControlV1) -> TspiBlockDescV1;                        // 仅在 seq 递增后
#[RT] pub fn publish_block_start(ctl: &TspiRtControlV1, desc: &TspiBlockDescV1) -> RtStatus;    // 写普通字段后 Release fetch_add
#[RT] pub fn ack_phase(ack: &TspiBlockAckV1, worker: WorkerId, phase: u8) -> RtStatus;          // 先校验 ack 槽标签
#[RT] pub fn wait_phases(ack: &TspiBlockAckV1, expected: u8, spin_max_iters: u32) -> RtStatus;  // ACK_MASK(expected)

// khaen-core::rt::swap / plan  [NONRT]
impl PlanRegistry {
    pub fn admit_crossfade(&mut self, old: PlanVersion, new: PlanToken, budget: &TspiCrossfadeBudgetV1) -> Result<PlanVersion, RtStatus>;
    pub fn poll_retire(&mut self, ctl: &TspiRtControlV1, ack: &TspiSwapAckV1) -> Option<PlanVersion>; // Some 才允许 drop/unmap
}
// T04 半整数/分数 D → 内部 plan 整数+Q16（I01）；残留 <1/65536 sample
// v1.1.3 E3：ABI v1 无 delay_q16 槽；打包前必须把 q16 折入 P/A 系数或取整（T66 GAP-T66-2）
pub fn design_delay_to_plan(total_delay_samples: f64) -> Result<(u32, u32), RtStatus>; // (delay_samples, delay_q16)；仅内部模型
// T45 参数化计划（不自动写内核）：只读拓扑/IRQ 建议，生成 affinity 表
pub fn plan_affinity(env: &AdmissionEnv) -> Result<[TspiWorkerAffinityV1; 4], RtStatus>;
~~~

**API 冻结/演进规则**：

- A-API-1：`[RT]` 函数的语义、参数类别（借用/值/非拥有）与返回值类别（无 Drop）冻结；T06–T70 可改字段名/内部模块，但不得把分配/Drop/时钟/锁引入 `[RT]` 调用图。
- A-API-2：`PlanToken/PlanBuf/StageToken` 等拥有型句柄只能出现在 `[NONRT]`；不得作为 RT 函数参数/返回值/字段。
- A-API-3：所有 `[RT]` 错误返回 `RtStatus`；不得返回 `Result<_, String>` 或自定义带 Drop 错误。
- A-API-4：`[RT]` 不得暴露泛型（防止单态化+代码膨胀/未知分配）；内部可用泛型但必须被非 RT 包装。
- A-API-5：任何需要“新增线程/新增锁/新增公共注册”的实现变更必须回到 integrator（T06/T26），不能在 leaf 任务里私加。
- A-API-6：REST/WS/校准 daemon 只能通过 `[NONRT]` API 与 registry/admission 交互；不得直接写 `RtControl/RtArena`。

### 12.4 与 T06 stub 的最小验收（未来）

T06 必须为上述类型/函数建 stub 并满足：`cargo test -p khaen-rt-host abi_layout`（真实测试、executed>0）覆盖附录 B 期望值；`rt-check forbid/drop-audit` 对空 stub 也必须有可运行入口与 1 个正例/1 个负例 fixture；不得用 “0 tests/只编译” 签收（WBS §最终验收补充）。



## 13. 无 RT 析构专项评审（DoD 核心）

### 13.1 定义与判定方法

**RT 析构**指任何在 DSP RT 区（worker/master，含其调用图）内执行的资源释放，包括但不限于：`Drop::drop`、`ptr::drop_in_place`、全局/dealloc 分配器释放、`Box/Vec/String/Map` 析构、`Arc/Rc` 强/弱引用减到 0 的释放、锁/guard 的 `drop`（Drop for MutexGuard）、线程 `JoinHandle` 析构、TLS 析构器、panic unwinding 产生的 cleanup、格式化/临时对象析构。

**判定方法（review 与未来 lint 共同执行）**：对每个 `[RT]` 函数取完整调用图（含内联后的被调者）；若图中任何值满足 `core::mem::needs_drop::<T>() == true` 且在 RT 内作用域结束，或存在显式释放操作，即判 FAIL。所有 `[RT]` 参数/返回/局部必须落入 §13.2 清单；清单外类型新增必须评审。

### 13.2 RT 可见类型清单（全部必须 needs_drop == false）

| 类型 | 位置 | RT 可见 | Copy | needs_drop 期望 | 说明/证明 |
|---|---|:---:|:---:|---|---|
| `RtStatus` | core::rt | 是 | 是 | false | `repr(transparent) u32`；错误只用码。 |
| `WorkerId` / `PlanVersion` / `FrameIndex` / `Tick` | core::rt | 是 | 是 | false | 整数别名。 |
| `TspiBlockDescV1` | core::abi | 是 | 是 | false | POD（§11.3）。 |
| `TspiEpochInfoV1` / `TspiHwParamsV1` | core::abi | 是 | 是 | false | POD（§11.4/§11.9）。 |
| `TspiEventV1` | core::abi | 是 | 是 | false | POD（§11.5）。 |
| `TspiPathV1` / `TspiDiagFirV1` / `TspiRoleV1` | core::abi | 是 | 是 | false | POD（§11.10）。 |
| `TspiPlanHeaderV1` / `TspiWorkerStatsV1` | core::abi | 是 | 是 | false | POD/原子字段（§11.8/§11.10）。 |
| `TspiBlockAckV1` | core::abi | 是 | 否 | false | 含 AtomicU32/U64；原子类型无 Drop。 |
| `TspiSpscHeaderV1` | rt-host::spsc | 是 | 否 | false | 原子字段，无 Drop。 |
| `TspiRtControlV1` | core::abi（§11.13） | 是 | 否 | false | 原子字段+固定数组；I02 新增。 |
| `TspiSwapAckV1` | core::abi（§11.13） | 是 | 否 | false | swap all-worker ack；I02 新增。 |
| `TspiEventRingLaneV1` | core::abi（§11.13） | 是 | 否 | false | lane 单写者+seq；I02 新增。 |
| `TspiWorkerRegsV1` / `TspiMasterRegsV1` | core::abi（§11.13） | 是 | 否 | false | RT 入口参数；I02/I05/I15 新增。 |
| `TspiCrossfadeBudgetV1` | core::abi（§11.13） | 是 | 否 | false | 双图预算；I06 新增。 |
| `TspiWorkerAffinityV1` | core::abi（§11.13） | 是 | 是 | false | POD affinity；I15 新增。 |
| `PlanRef` | core::rt | 是 | 是 | false | NonNull+u32；非拥有、无 Drop。 |
| `PlanView<'a>` / `BlockIo<'a>` / `WorkerRtCtx<'a>` | core::rt | 是 | 是 | false | 只含引用/Copy 值；引用无 Drop。 |
| `SafetyAction` / `DitherCfg` | core::rt / rt-host::format | 是 | 是 | false | `repr(u8)/POD`，无 payload/Drop。 |
| `PlanArena` / `RtArena` | rt-host::admission | 经指针 | 否 | 不适用 | 原生 mmap/预分配字节区；不实现 Rust Drop，也不作为 owned 值进 RT。 |
| `PlanOwner` / `PlanBuf` / `PlanToken` / `StageToken` / `PlanRegistry` / `AdmissionEnv` | rt-host/core plan | **否** | 否 | 可有（仅 CTL） | 拥有型句柄，禁止出现在 `[RT]` 签名/字段/局部。 |
| `FormatConfig` / `FormatStats` | rt-host::format | **否**（IO 区） | 是否 | IO 区可持有；不得进入 DSP 调用图 | 见 §10.1。 |

**静态断言要求（T06 实现）**：对表中所有 RT 可见类型写

~~~text
const _: () = assert!(!core::mem::needs_drop::<T>());
const _: () = assert!(core::mem::size_of::<T>() == 期望字节数);
~~~

（`needs_drop` 的 const 可用性以 T06 固定 toolchain 为准；若不可 const，则用真实单元测试断言，executed test_count > 0，不允许空测试。）

### 13.3 无 RT 析构规则（R-DROP-01..R-DROP-14）

| ID | 规则 | 检查方式 |
|---|---|---|
| R-DROP-01 | `[RT]` 调用图内禁止任何 `needs_drop==true` 值的构造/作用域结束；禁止显式 `drop/drop_in_place`。 | 类型清单+lint |
| R-DROP-02 | RT 模块内禁止 `impl Drop for ...`；禁止手写 deallocator。 | grep/lint |
| R-DROP-03 | RT 签名/字段/局部禁止 Arc/Rc/Weak/Box/Vec/String/HashMap/PathBuf/TLS。 | API 快照+lint |
| R-DROP-04 | PlanArena/RtArena 地址固定；RT 只拿 PlanRef/引用；unmap 只由 CTL 在 RG-1..4 后执行。 | 代码审查+T20 |
| R-DROP-05 | 引用计数（Arc）只在 CTL 增减；RT 不得 clone/drop/upgrade。 | grep+lint |
| R-DROP-06 | quarantine 槽 all-ack 前禁止复用/释放；失联则永久保留至进程重启。 | T20/T21 负向测试 |
| R-DROP-07 | 错误路径只返回 `RtStatus`；禁止构造带 Drop 的错误对象/字符串。 | lint |
| R-DROP-08 | 事件/统计只用 POD/原子/固定数组；禁止事件内容分配/格式化。 | lint |
| R-DROP-09 | DSP 区不持有任何 fd/ALSA 设备句柄/文件句柄；设备句柄只由 IO owner 持有并在 CTL 关闭。 | 代码审查 |
| R-DROP-10 | 不依赖 RAII 清理：任何路径（含 panic/abort）都不得要求析构器保证正确性；正确性由状态机+quarantine 保证。 | 状态机审查 |
| R-DROP-11 | TLS/线程局部析构禁止；thread_local 不得出现在 RT 调用图。 | lint |
| R-DROP-12 | v1 无 FFI；未来 C ABI 结构必须 POD 无 Drop，且不得跨边界传 owned 指针。 | §11.11 |
| R-DROP-13 | 测试/bench 代码不得把 Drop 类型注入生产 RT 路径；`[TEST]` 不得被 admission 接受。 | 审查 |
| R-DROP-14 | 每个 RT 模块文件头必须写明“本文件禁止析构/分配/锁/syscall”，并引用本节与 §10；缺注释 = review 不通过。 | 文件头检查 |

### 13.4 生命周期证明骨架（为何 RT 不会析构）

- **Lemma 1（不持有）**：RT 可见类型清单（§13.2）无任何拥有型资源；PlanRef 是 raw pointer+version 的 Copy 值，引用/切片只是借用。→ RT 不可能“结束作用域触发释放”。
- **Lemma 2（不释放 plan）**：PlanArena 的强引用只存在于 CTL 的 PlanRegistry；RT 指针由 master 在 block 边界读取，非 RT 不能在 `switch_state!=ARMED` 时替换。旧 plan 的 unmap 必须通过 RG-1..4（§7.5）；在 RG-2 之前 4 worker 仍可能读旧指针，在 RG-3 之前旧版本块可能未终态；因此不存在“RT 最后引用者释放”。
- **Lemma 3（不释放 buffer）**：RtArena/ring/payload/desc 在 admission 时一次性分配并 mlock 到进程退出；RT 只迁移 slot 状态（FREE↔…↔RECYCLABLE）且回收由 CTL 执行；quarantine 槽 all-ack 前禁止复用（INV-L5），失联则永不回收。→ RT 内没有“buffer 归还分配器”的动作。
- **Lemma 4（不析构临时值）**：RT 函数签名与局部只用 §13.2 类型；禁止 `Vec/Box/String/Result<_,String>`、禁止格式化、禁止闭包捕获 Drop 值；所有循环/分支不创建 owned 临时。
- **Lemma 5（错误与 panic 不清理）**：错误路径返回 Copy 的 RtStatus，不创建 Drop 错误；panic 在 RT 内被禁止（F-21），且 admission 拒绝 debug build；即使发生 panic，进程终止而非依赖 RAII 清理（文档化例外：进程退出不属于 RT 析构路径）。
- **结论**：在上述不变量与清单被 code review/lint 证明后，RT 调用图内不存在析构器。任何一条清单外类型/任何 `needs_drop` 值进入 RT 都视为规范违反，T06 lint 必须失败。

### 13.5 无 RT 析构评审清单（D-01..D-20；v1.1 加规则 ID/工具/失败判据/豁免，I09）

| ID | 检查项 | 规则 ID | 工具/测试 | 失败判据 | 豁免 | 状态 |
|---|---|---|---|---|---|---|
| D-01 | RT 可见类型全部 needs_drop==false（§13.2 清单） | RT-DROP-01 | S1 静态断言+T06 单测 | 任一 needs_drop=true 即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-02 | RT 模块无 impl Drop | RT-DROP-02 | L2 rt-check 源扫描 | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-03 | RT 代码无 drop/drop_in_place/dealloc | RT-DROP-03 | L1+L2 | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-04 | RT 代码无 Arc/Rc/Weak | RT-DROP-04 | L2 类型级扫描 | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-05 | RT 无 Box/Vec/String/HashMap 等 owned 类型 | RT-DROP-05 | L2 签名+局部类型分析 | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-06 | PlanArena 只由 CTL unmap，且 RG-1..4 后 | RT-DROP-06 | T1（T20 swap 测试） | 任何提前 unmap/未 ack 释放即 FAIL | 无 | PASS-DESIGN |
| D-07 | 引用计数只在 CTL 增减 | RT-DROP-07 | R1 审查（模板见 §13.7） | 审查记录缺失或命中即 FAIL | 无 | PASS-DESIGN |
| D-08 | quarantine all-ack 前禁止复用/释放 | RT-DROP-08 | T1（T20/T21 负向） | 复用/释放即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-09 | worker 失联时 quarantine 永不回收 | RT-DROP-09 | T1（T21 测试） | 回收/复用即 FAIL | 安全优先，不允许豁免 | PASS-DESIGN |
| D-10 | 错误路径无带 Drop 错误/字符串 | RT-DROP-10 | L1+L2 | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-11 | 事件/统计无分配/格式化 | RT-DROP-11 | L2 模块扫描 | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-12 | DSP 区无 fd/设备句柄析构 | RT-DROP-12 | R1 审查 | 审查记录缺失或命中即 FAIL | 无 | PASS-DESIGN |
| D-13 | 无 TLS 析构依赖 | RT-DROP-13 | L2（thread_local） | 命中即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-14 | 无 FFI 跨边界 owned 指针 | RT-DROP-14 | L2（extern/FFI） | 命中即 FAIL | v1 无 FFI | PASS-DESIGN |
| D-15 | panic 不依赖 RAII；admission 拒绝 debug RT | RT-DROP-15 | C1 构建检查+T1 | debug_assertions=true 或依赖 cleanup 即 FAIL | 无 | PASS-DESIGN |
| D-16 | 测试/bench 不把 Drop 类型注入生产 RT | RT-DROP-16 | R1+CI feature 隔离 | 审查/feature 泄漏即 FAIL | 无 | PASS-DESIGN |
| D-17 | 每个 RT 模块文件头声明禁止行为 | RT-DROP-17 | L2 文件头扫描 | 缺失即 FAIL | 无 | PASS-DESIGN |
| D-18 | 状态机正确性不依赖析构器（任何路径安全） | RT-DROP-18 | T1（T21 状态测试） | 任一路径依赖 Drop 即 FAIL | 无 | PASS-DESIGN |
| D-19 | ABI 结构无 Drop（含原子结构） | RT-DROP-19 | S1 静态断言（含 §11.13 7 结构） | 缺失/失败即 FAIL | DSP 无豁免 | PASS-DESIGN |
| D-20 | 负向 fixture：故意在 RT 内分配/析构/用 Arc/Vec/String/TLS/FFI 的样例必须被 lint/CI FAIL | RT-DROP-20 | T1（T06 fixture，≥8 类） | 负向样例 PASS 即 FAIL；覆盖<8 类即 FAIL | 无 | DEFERRED-CODE |

> **P0 结论**：设计级覆盖 D-01..D-19 = PASS-DESIGN；D-20 为 DEFERRED-CODE，必须由 T06 用真实非零 fixture/executed 结果关闭。**工具类 L1/L2/S1/T1/R1/C1 定义见 §10.3**；仅审查项（D-07/D-12/D-16）必须有 §13.7 审计模板证据，否则 FAIL。本文件不把设计级 PASS 冒充代码级 PASS。

### 13.6 与 T06/T20/T21 的最小负向测试要求（未来，不执行）

- T06 fixture（D-20）：至少 **8 类**负向样例，每类必须被 lint/CI FAIL：①RT 内 Vec::new/vec!；②Arc clone/drop；③drop(Box)；④返回 Result<_,String>；⑤thread_local/TLS；⑥extern/FFI 调用；⑦Box<dyn Trait>；⑧format!/println!。正向 fixture 必须 PASS；executed test_count>0；fixture 文件 hash 进 evidence/T02 或 T06 证据。
- T20：构造“worker 迟到写入后 cutoff”场景，断言（a）该块永不发布给 playback；（b）quarantine 槽在所有 worker ack 前不复用；（c）旧 plan 在 all-ack 前不 unmap（mock 计数器）；（d）失联 worker 场景下内存保持不回收；（e）crossfade 双图峰值超限→ERR_CROSSFADE_BUDGET/安全静音；（f）old/new total_delay_q16 不相等时 crossfade 必须拒绝。
- T21：构造 watchdog 超时/worker lost/ack 槽耗尽，断言进入 SafetyMute、quarantine 保留、无释放、无新 block 启动；恢复只经完整 admission；事件 transition_key 不重复执行。
- T09：block_start_seq 配方/Acquire-Release/expected=2 与 4 的 ack_mask 测试；迟到 worker 旧 ack 槽标签检测。

### 13.7 仅审查项审计模板与豁免（I09/v1.1）

**审计模板（R1 必填，缺任一字段该规则 FAIL）**：审查人/日期/文件路径+SHA-256/行号/规则 ID/结论(PASS/FAIL)/替代证据/批准人。模板必须存档到 evidence/T02 或对应任务 evidence（例如 T20/T21 的 R1 项）。

**D 类豁免**：DSP 区 D-01..D-05/D-08/D-10/D-11/D-13/D-14/D-17/D-19 **无豁免**；仅 D-06/D-07/D-09/D-12/D-15/D-16/D-18 可由 integrator+独立 reviewer 批准“测试/审查替代”，且必须记录理由与证据。**F 类豁免见 §10.3**（DSP 区无豁免）。

## 14. 评审检查表与 P0 复核记录

### 14.1 复核信息

| 项 | 值 |
|---|---|
| 复核人 | rt-api（WBS T02 执行 agent；v1.0 自评非独立；**v1.1 按 T02-review 修复后仍待独立复审**） |
| 日期 | v1.0 2026-11-16；**v1.1 2026-11-17** |
| 方法 | ① 逐条对照 T02-review I01–I15 修 spec；② 手算时限/群延迟/容量（附录 B）；③ **文件外** evidence/T02/abi_vectors.py 独立复算 ABI/timing/GD；④ evidence/T02/check_core_spec.py 结构化自检；⑤ 输入 hash 绑定（附录 A） |
| 范围 | 仅设计/规范级；未实现代码、未编译、未链接、未跑 cargo test、未上板、未测量 |
| 独立性 | **不足**：需 lead 指派非 rt-api 的独立 reviewer 按 §14.2–§14.4 复核；T06 起必须有代码级证据。本记录不冒充独立评审 |

### 14.2 API 检查表（A-01..A-28）

| ID | 检查项 | 结果 | 证据 |
|---|---|---|---|
| A-01 | crate/module 映射与 WBS T06/T26 一致 | PASS-DESIGN | §12.1 |
| A-02 | `[RT]/[IO]/[NONRT]/[WD]/[TEST]` 标注规则固定 | PASS-DESIGN | §12.2 |
| A-03 | plan build/validate/hash 接口固定且仅 NONRT | PASS-DESIGN | §12.3 |
| A-04 | RT plan view 只读/非拥有/无 Drop | PASS-DESIGN | §4.4/§12.3/§13.2 |
| A-05 | RT 函数白名单与调用图约束固定 | PASS-DESIGN | §12.2/A-API-1..6 |
| A-06 | worker/master 入口与主循环语义固定 | PASS-DESIGN | §7.7/§12.3 |
| A-07 | barrier/ack API 与位图真值固定 | PASS-DESIGN | §7.3/§11.6 |
| A-08 | 时钟/时间戳所有权固定（master ≤3/纯 worker 0） | PASS-DESIGN | §6.4/§10.1 |
| A-09 | format 边界、stride、对齐、格式枚举固定 | PASS-DESIGN | §11.9 |
| A-10 | pipeline 顺序 A_map→G→P→保护→scatter 固定 | PASS-DESIGN | §11.10 |
| A-11 | 禁止上混/下混的校验规则固定 | PASS-DESIGN | §0/§11.10 |
| A-12 | stereo_strict 与 modal mask 默认固定 | PASS-DESIGN | §11.10 |
| A-13 | graph_id/plan_version/stale 双条件固定 | PASS-DESIGN | §11.10/§5.3 |
| A-14 | prepare/commit/abort/rollback/actual-ack 语义固定 | PASS-DESIGN | §5.3/§12.3 |
| A-15 | event/metrics 非 RT 导出接口固定 | PASS-DESIGN | §8.3/§8.4 |
| A-16 | device owner 互斥（playback vs calibration）固定 | PASS-DESIGN | §5.4 |
| A-17 | RT 错误码与动作映射固定 | PASS-DESIGN | §9.2 |
| A-18 | admission report 字段固定 | PASS-DESIGN | §12.3 |
| A-19 | last-good 保留/回滚边界固定 | PASS-DESIGN | §5.3/§5.5 |
| A-20 | API 稳定性/变更流程固定 | PASS-DESIGN | §12.3/§15 |
| A-21 | 分数/半整数 D → 折入系数（或取整）与内部 q16 残差规则（ABI v1 无 `delay_q16`，E3） | PASS-DESIGN | §3.4/§11.10/§12.3 |
| A-22 | RT 共享结构 ABI（RtControl/SwapAck/EventRingLane/WorkerRegs/MasterRegs） | PASS-DESIGN | §11.13/§13.2 |
| A-23 | 2-worker ack 参数化（expected/ack_mask） | PASS-DESIGN | §3.4/§7.3/§11.6 |
| A-24 | block 启动原语 + tick 预算（≤3） | PASS-DESIGN | §6.4/§7.3/§11.13 |
| A-25 | crossfade 双图预算契约 | PASS-DESIGN | §5.3/§7.4/§11.13/§9.2 |
| A-26 | ack 槽耗尽/迟到旧槽检测 | PASS-DESIGN | §7.3/§7.6/§9.2 |
| A-27 | T45 affinity/优先级/IRQ 字段 | PASS-DESIGN | §11.10/§11.13/§12.3 |
| A-28 | ALSA period→core B 一致性/拒绝规则 | PASS-DESIGN | §6.1/§11.9/§9.2 |

### 14.3 RT 禁止行为检查表（F-01..F-44 逐条复核）

| ID | 复核结果 | 落点（未来强制机制） |
|---|---|---|
| F-01 | PASS-DESIGN | T06 lint/RT-FORBID-01 |
| F-02 | PASS-DESIGN | T06 lint/审查 |
| F-03 | PASS-DESIGN | T06 lint+§13.2 |
| F-04 | PASS-DESIGN | T06 lint |
| F-05 | PASS-DESIGN | T06 lint |
| F-06 | PASS-DESIGN | T18 栈预触+审查 |
| F-07 | PASS-DESIGN | T06 grep+审查 |
| F-08 | PASS-DESIGN | T06 lint |
| F-09 | PASS-DESIGN | T06 lint |
| F-10 | PASS-DESIGN | T09/T20 超时测试 |
| F-11 | PASS-DESIGN | T09/T20 |
| F-12 | PASS-DESIGN | T06 lint |
| F-13 | PASS-DESIGN | T06 符号审查+T26 strace 抽检（未来） |
| F-14 | PASS-DESIGN | T06 lint+§6.4 |
| F-15 | PASS-DESIGN | T06 lint |
| F-16 | PASS-DESIGN | T06 lint |
| F-17 | PASS-DESIGN | T06 lint |
| F-18 | PASS-DESIGN | T06 lint |
| F-19 | PASS-DESIGN | T06 审查 |
| F-20 | PASS-DESIGN | T06 lint |
| F-21 | PASS-DESIGN | T06 lint |
| F-22 | PASS-DESIGN | T06 lint+属性测试 |
| F-23 | PASS-DESIGN | T06 审查 |
| F-24 | PASS-DESIGN | T06 lint |
| F-25 | PASS-DESIGN | T06 lint |
| F-26 | PASS-DESIGN | T06 lint |
| F-27 | PASS-DESIGN | §13 D 清单+T06 lint |
| F-28 | PASS-DESIGN | T06 审查 |
| F-29 | PASS-DESIGN | T20 测试 |
| F-30 | PASS-DESIGN | T20/T06 |
| F-31 | PASS-DESIGN | T20/T21 负向测试 |
| F-32 | PASS-DESIGN | §13 D 清单 |
| F-33 | PASS-DESIGN | T06 审查 |
| F-34 | PASS-DESIGN | T06/T09 布局断言 |
| F-35 | PASS-DESIGN | T07/T08/T15 对拍 |
| F-36 | PASS-DESIGN | T06 lint |
| F-37 | PASS-DESIGN | T06 审查 |
| F-38 | PASS-DESIGN | §11.11 |
| F-39 | PASS-DESIGN | T06 lint |
| F-40 | PASS-DESIGN | T18 探测+T06 lint |
| F-41 | PASS-DESIGN | T22 参考向量 |
| F-42 | PASS-DESIGN | T22 参考向量 |
| F-43 | PASS-DESIGN | T22 测试 |
| F-44 | PASS-DESIGN | T17/T20 审查 |

### 14.4 状态机/协议检查表（S-01..S-13、P-01..P-15）

| ID | 检查项 | 结果 | 证据 |
|---|---|---|---|
| S-01 | HostState 迁移表完整（含非法迁移拒绝） | PASS-DESIGN | §5.1 |
| S-02 | Epoch 新建/失效触发条件全列 | PASS-DESIGN | §5.2 |
| S-03 | PlanLifecycle 与 last-good/rollback 完整 | PASS-DESIGN | §5.3 |
| S-04 | Block/slot 状态机与 owner 完整 | PASS-DESIGN | §6.5 |
| S-05 | DeviceOwner 互斥与交换窗口 | PASS-DESIGN | §5.4 |
| S-06 | SafetyMode/fallback/静音进入退出 | PASS-DESIGN | §5.5 |
| S-07 | fault/watchdog 恢复边界（不自动恢复、不释放） | PASS-DESIGN | §5.1/§7.6 |
| S-08 | 每次迁移有且仅有一次事件 | PASS-DESIGN | §5.1/§8.2 |
| S-09 | epoch 不复用/不跨 epoch 拼接 | PASS-DESIGN | §5.2/INV-L8 |
| S-10 | 切换/回滚的 actual version 语义 | PASS-DESIGN | §5.3 |
| P-01 | SPSC 固定容量、无无界队列 | PASS-DESIGN | §6.3 |
| P-02 | SPSC 内存序与发布配方 | PASS-DESIGN | §6.3 |
| P-03 | block 身份字段（epoch/frame/valid/plan） | PASS-DESIGN | §6.1 |
| P-04 | deadline/gate/hard cutoff 公式与语义 | PASS-DESIGN | §3.5/§6.4 |
| P-05 | worker-ack 位图/all-ack 定义 | PASS-DESIGN | §7.3 |
| P-06 | 迟到/cutoff 丢弃、不读半块 | PASS-DESIGN | §6.3/§7.3 |
| P-07 | quarantine all-ack 前不复用 | PASS-DESIGN | §7.6/INV-L5 |
| P-08 | swap block 边界+实际 ack | PASS-DESIGN | §5.3/§7.5 |
| P-09 | epoch/plan mismatch 拒绝 | PASS-DESIGN | §6.3/§9.2 |
| P-10 | 事件环有界+drop 计数 | PASS-DESIGN | §8.3 |
| P-11 | 错误→动作映射完整 | PASS-DESIGN | §9.2 |
| P-12 | fallback/safety 输出预分配且准入 | PASS-DESIGN | §5.5 |
| S-11 | PlanAborted/EpochAborted 状态与迁移 | PASS-DESIGN | §5.2/§5.3 |
| S-12 | HostState 事件幂等/重复/优先级 | PASS-DESIGN | §5.1/§8.3 |
| S-13 | capture-full slot 中止路径 | PASS-DESIGN | §6.3/§6.5 |
| P-13 | ack 槽生命周期/标签/耗尽 | PASS-DESIGN | §7.3/§7.6 |
| P-14 | 事件幂等/去重/顺序 | PASS-DESIGN | §8.3 |
| P-15 | 自检可证伪（外部脚本/结构化解析） | PASS-DESIGN | 附录 C/evidence/T02 |

### 14.5 其他检查表引用

- ABI 检查表 B-01..B-19：§11.12（全部 PASS-DESIGN；B-12 DEFERRED-CODE）。
- 无 RT 析构检查表 D-01..D-20：§13.5（D-01..D-19 PASS-DESIGN；D-20 DEFERRED-CODE；v1.1 加规则 ID/工具/豁免）。

### 14.6 复核结论与残余风险

- **P0 设计级结论（v1.1.3）**：T02 DoD 六项在规范级固定；T02-review 15 项 CLOSED（I01 经 E3 回退为 NOT-LANDED）；T02-rereview N1–N8 CLOSED；**errata E1（PlanHeader 对齐 96/100/104/108）、E2（F-14 ≤3）、E3（delay_q16 回退 RESERVED-0）、E4（plan flags bit5..7=0）已落地**；无 RT 析构有 §13 专章；可判 **DESIGN-REVIEW PASS（规范级，v1.1.3）**，待独立 targeted 复审签收。
- **不是**代码/运行/板级 PASS：无任何实现或测试被执行；T06/T16/T20/T21/T26 的代码级验证是后续硬门（D-20/B-12/全部 DEFERRED-CODE）。
- 残余风险 R1：ABI 期望值需真实编译器静态断言（T06），Python 复算只证明字节布局逻辑，不证明 Rust `repr(C,align(64))` 实现无误。
- 残余风险 R2：master tick 源（A72 CNTVCT_EL0）可用性/频率稳定性未在板上验证；T18/T19/T45 必须闭环，失败则不启 RT 主模式。
- 残余风险 R3：`MAX_MATRIX_PATHS=128` 对 T56 最坏 profile 是否足够；超出必须 v1.1 升版，不得静默。
- 残余风险 R4：event/ack 的实现正确性（无锁、内存序）只能由 T09/T20 真实测试与审阅关闭；本规范不证明代码无数据竞争。
残余风险 R5：capture/playback 的 latency budget 与 play_deadline 计算归 T17/T19；若与 15/25/40 ms 预算冲突，按 T17/T19/T46 实测与 WBS 裁决。
- 残余风险 R6（v1.1）：新增共享结构/affinity/crossfade 字段需真实 Rust 静态断言（B-12..B-19）与 T20/T45 测试关闭；本文只给期望布局。
- 残余风险 R7（v1.1）：T04 §6.4 `< fs/2` 与 WBS/DEC/T05-X09 `≤fs/2` 的边界分歧登记于 §15.2 Q-12，需 lead 裁决，T11 实现前关闭。
- 残余风险 R8（v1.1.1 刷新）：T05 23 码/旧 T04 hash stale 已关闭（Q-13 RESOLVED，T05 1a46b4f8）；T04 v1.2 errata A 已改 T04:45/§7.3 的 64/128/256 措辞（N8 CLOSED）。

## 15. 下游任务契约映射、开放问题与变更流程

### 15.1 下游任务必须消费/产出的条款

| 任务 | 必须消费本文 | 必须产出/满足 | 禁止 |
|---|---|---|---|
| T06 | §3 常量、§11 ABI、§12 API、§10 F 表、§13 D 表 | Cargo 唯一根、crate stub、`rt-check` 入口、静态断言/负向 fixture、executed test_count>0 | 不改语义；不建第二个 Cargo 根；不把公共注册外包 |
| T07 | §7.1/§11.10、§13 | direct kernel `[RT]` 无分配/无 Drop；与 T03 reference 对拍；固定累加顺序 | 新增 Drop/分配/时钟；改变 reduce 顺序 |
| T08 | §6/§7/§11.10 | 共享 FFT 的 b-p 历史/scratch 预分配/短尾正确（T08 DoD） | 循环混叠、未就绪谱消费、运行期 plan |
| T09 | §7 全部、§11.6 | 4 worker 静态 DAG/ack/有界等待/固定 reduce | work stealing/动态线程/无界 spin |
| T12 | §3.4/§11.10/§12.3（内部 delay_q16/total_delay_q16；ABI v1 无该槽，E3）、§7.1 | factorized/fused 编译、长度加法、选择 plan；T04 半整数/分数 D 在打包 ABI 前折入 P/A 系数（内部 DelayPlan 可保留整数+Q16，残差 <1/65536；T66 GAP-T66-2 按 `E_CA_DELAY_Q16_ABI` 拒收非零 q16）。**T03 oracle 规则**：与 T03 reference 对拍前必须把全部分数/非整数 D 折入 P/A 系数（对拍图 q16=0），或声明 T03 schema 扩展；编译报告必须写明 selected mode | 静默截尾/假合并省算/丢弃分数 D/让 T03 oracle 吃 q16/把 q16 写进 ABI reserved 槽 |
| T13 | §11.10/§5.3 | 校准 v1/v1.1 → plan 的 hash/stale/version adapter（不改旧文件语义） | 双叠 curve/未知命令放行 |
| T16 | §6.3/§11.7 | 固定 SPSC、发布配方、over/underrun | 无界队列/半块读 |
| T17 | §5.4/§11.9/§6.1（period→B 链）/§6.4 | ALSA duplex actual、owner 独占、恢复状态机、stride 契约；actual period∉{64,128} → ERR_DEVICE_PARAMS | 在 S1 运行时开硬件；两进程抢设备；静默夹取 period |
| T18 | §5.1/§3/§12.3 | mlockall/RLIMIT/预触/CPU set/clock 源/栈；失败拒启 | warn 后自称 RT；MCL_FUTURE 后无容量纪律 |
| T19 | §6.4/§11.4 | 时钟映射/ppm/唯一 servo/discontinuity→新 epoch | 第二 clock 闭环；PTP 锁当 USB 同钟 |
| T20 | §5.3/§7.3/§7.4/§7.5/§7.6/§13 | block-version+4worker ack、ACK_MASK 参数化、crossfade 双图预算、quarantine/ack 槽耗尽、nonRT 回收、无 RT 最后 Arc drop | RT 内 drop/释放；未 ack 复用；峰值超限仍 crossfade |
| T21 | §5.5/§8/§9 | watchdog/有界直方图/fallback 预分配/安全 mute | 释放 quarantine；超时后重算昂贵 P |
| T22 | §11.9/§9.2 | S24_3LE/S32_LE/planar/饱和/finite/clip/dither + 边界参考向量 | DSP 区 syscall；未声明 dither |
| T23 | §11.10 pipeline/§0 | 节点顺序、scatter 零填、保护节点；无上混/下混 | 角色名硬编码滤波；隐式矩阵 |
| T24/T42/T44/T60/T61/T62/T66/T70 | §5.3/§5.4/§9 | prepare/commit/rollback 的 actual-version ack；校准 job 独占；Web 真实操作 | 假 ack；Camilla 回退；越权写 RT |
| T25/T57 | §11.10 stereo_strict/modal mask、role 表 | overlay 解析+hash 绑定、role→output 显式零填 | Web 模板 SUB4；runtime mask 破坏 G |
| T26 | §11/§12/§13/§10 | 串行集成、fake/file 端到端、no-RT-drop 总审计、共享结构布局断言 | 绕过 leaf 实现/抢板/自创共享布局 |
| T45 | §11.10/§11.13/§12.3（WorkerAffinity/AdmissionReport） | 实测拓扑 CPU/FIFO/IRQ 依赖回写 affinity 表与 adm report；参数化计划不自动写内核 | 失核半图；自动 rt_runtime=-1；无 affinity 证据 |
| T47 | §8.4/§3.5 | 指标定义/单位/CV/range 可复算 | 把 spin 算 busy_compute |
| T56 | §3/§7.1 | 全图静态 plan、taps 扩展准入、预算 | 只按 25 path；超 128 paths 不升版 |
| T14 | §6.3/§11 | file runner 用同一 core/plan 语义、identity 保护 | 用 file 结果冒充 ALSA 实测 |
| T15 | §11.10（E3：ABI v1 无 delay_q16）/§15.1 T12、§13 | T03 reference 只验证整数 D 图；T15 必须按 T12 的折入模式用 T03 oracle 对拍，非整数 D 的图先折入 P/A 或走 T03 扩展 schema；不得把 q16 当 T03 graph.D | 用 q16 图直接对 T03 oracle/未声明折入模式 |
| T46/T30 | §3.5/§8.4 | 1/2/4 同 kernel、worst/p99.9/0 miss 统计 | 平均 CPU/理论 GF 签字 |

### 15.2 开放问题（需 lead/T06/T18/T19/T45/T56 裁决或确认）

| ID | 问题 | 本文当前口径 | 建议/owner |
|---|---|---|---|
| Q-01 | master tick 源与预算：CNTVCT_EL0/TSC/vDSO | **RESOLVED v1.1**：预算=3（B1 进入/可选 cutoff/B3 退出，§3.4/§6.4）；tick 源由 T18/T19 选定并标定，不可用→ERR_ADMISSION_CLOCK | T18/T19 实现并测量，不得静默降级 |
| Q-02 | `MAX_MATRIX_PATHS=128` 是否覆盖 T56 最坏 profile（16 输入×8 低带）？ | 128（§3.1） | T56 建模确认；若超出升 v1.1，不得静默扩容 |
| Q-03 | 2 核 profile ack 语义 | **RESOLVED v1.1**：expected_workers=2、ACK_MASK(2)=0x3、位 0..1；2 核=独立 plan/epoch（§7.3/§11.6） | T20/T45 用真实测试确认；禁止硬写 0xF |
| Q-04 | `play_deadline_ns` 与 latency budget 的计算公式/owner | 只固定字段与 hard cutoff 用法 | T17/T19 定；不得改变“capture 写、playback 用” |
| Q-05 | dither 精确算法/种子/量化语义 | HwParams flags+seed 占位；默认 off | T22 定义并给边界向量 |
| Q-06 | 角色级 PEQ 系数存放在 ctl_meta 还是独立节点区 | ctl_meta 仅 role 名；节点参数区由 T23/T41 扩展定义，不改变 RT ABI | T23/T41 定义；若需新 ABI 字段升 v1.1 |
| Q-07 | 96k 是否 P1 必测 | core 支持 48k/96k，门按 T_block 缩放 | T17/T74/lead 裁决；缺设备则 INCOMPLETE |
| Q-08 | last-good 的粒度（plan vs 二进制/服务） | core 只负责 plan 级 last-good；二进制/服务回滚归 T28/T32 | T28/T32/lead |
| Q-09 | 事件 lane 是否需给校准 daemon 额外 lane | v1 固定 8 lane（§8.1）；校准 daemon 非 RT，走自己的日志/文件 | T60；如需 RT 事件再升版 |
| Q-10 | 本任务 evidence/<T02>/manifest 的独立文件 | 因 T02 写域仅 `spec/core-v1.md`，manifest 内嵌附录 A；未写 evidence/ 目录 | lead：如要独立 manifest，需授权写 `rt-stack/evidence/T02/`；本 agent 未越权 |
| Q-11 | block 256 与 REST chunksize | **RESOLVED v1.1**：runtime 仅 {64,128}（§3.2）；T03/T04 的 256 是参考/设计测试口径；T05/REST 对非 64/128 显式 400/unsupported；T17 actual period 不匹配→ERR_DEVICE_PARAMS | T05/T17/T42 按此更新；若确需 256 必须 v1.1 重做预算并重审 |
| Q-12 | T04 target freq 边界（magnitude/phase） | **RESOLVED（2026-11-17）**：T04 v1.2-draft errata A（97,033 B / `86fb97fd…`；RULING-fs2-2026-11-17.md `2097aca0…`）已按 Lead 裁决落地：magnitude-only 允许 `freq <= fs/2`；带 phase 必须 `< fs/2`（=fs/2 → `E_TGT_PHASE_REAL`）；`freq > fs/2` → `E_TGT_FREQ_RANGE`；T04 §6.9 仍 28 码（#12/#19 更新）。T05 X09 仍写 ≤fs/2（magnitude 相容；phase 例外经 T05 Y17“以 T04 §6 为准”）。 | T11 按 T04 §6/§6.9 实现；A.2 #14 已更新；无需 lead 再裁决 |
| Q-13 | T05 快照 23 码/旧 T04 行号 | **RESOLVED（T05 已更新，当前 99,248 B / `1a46b4f8…`）**：K34 已改为“拒收码全集见 T04 §6.9（v1.2 为 28 码，不写死码数/行号）”；Y17 以 T04 现行 §6 为准；R13/R14 chunksize 已改 runtime 仅 64/128（Q-11）。 | T11 以 T04 §6.9 为准；A.2 #15 更新；T02 不再登记该 stale |

### 15.3 变更流程

1. 任何实现任务发现本文不可实现/缺少字段 → 先在本任务证据中记录 BLOCKED 原因，通知 integrator；不得私自改 ABI/语义。
2. 编辑性澄清（错别字/交叉引用/例子）→ 本文 `1.0.x`，由 api-designer/rt-api 更新并在附录 A 记录新 hash。
3. 语义/字段/容量/状态/错误码/ABI 变化 → 必须升 `1.1`，附变更说明+影响任务清单+迁移/废弃策略，由 lead 或 integrator 批准；T06 以后实现必须同时更新静态断言与 lint。
4. 不得在实现 crate 里写“以代码为准”绕过本文；代码与本文冲突时，T06 集成测试必须失败并回到规范裁决。



## 附录 A：manifest（输入/产物/命令/未测项/假设）

### A.1 任务与状态

| 项 | 值 |
|---|---|
| WBS 任务 | T02（api-designer）；**v1.1 修复任务 task-44**（按 T02-review 15 项） |
| 执行 agent | rt-api（作者；v1.1 自评不等于独立复审） |
| 日期（本地） | v1.0 2026-11-16；**v1.1 2026-11-17** |
| 产物 | 本文件（`projects/tspi-audio-dsp/rt-stack/spec/core-v1.md`）+ `rt-stack/evidence/T02/`（abi_vectors.py、check_core_spec.py、manifest.json、结果日志） |
| 工作方式 | 只读输入 + 外部脚本验证；未创建 Cargo 根、未改其他生产代码、未 git commit、未碰板/ALSA/CueMix/服务 |
| 证据类型 | P0 人工检查表 + 手算/ABI 解析验证（**脚本在文件外**，I10）+ 输入 hash；无代码/无运行测试（本任务不产代码） |

### A.2 输入文件与 SHA-256（v1.1.1 修复时实测，2026-11-17 01:0x；上游并发更新以本节快照为准；17 输入）

| # | 文件 | 字节 | SHA-256 |
|---:|---|---:|---|
| 1 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/scratch/lead/planner/TSPI-RT-STACK-WBS.md | 35728 | ffe5cde9e27470f0c2db9c4242383478422f553defba9370c8d268d0643204b9 |
| 2 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/scratch/lead/planner/DSP-HOST-DECISION.md | 27951 | ab1056477d8e1c01dbe61df6f2d15c4bb0cca6b05f9177ef51559df3e2d25ded |
| 3 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/scratch/lead/planner/DSP-HOST-CPU-BUDGET.md | 18859 | 5954bfa2c64210e1dd4c75268aa674976c3f67fa35c2000363cc6dc5e25a2aa1 |
| 4 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/scratch/lead/planner/DSP-HOST-CPU-MODEL.json | 26674 | ffeaf8a8941ca211efcff1ad8a8b9bc3537997b215fae483b511ffa2a1d23d0e |
| 5 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/calibration-spec/CONTRACT.md（task-118 刷新） | 25438 | bec362671d645c27731eec2254b5aad1e3526e788fafbca1a0e5eb12b3dd77af |
| 6 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/calibration-spec/ACCEPTANCE.md（task-118 刷新） | 30222 | c83671a3e0f36ea6b945497744e916f2b7d1e4c2b53055a44c2afb8a744d6ffb |
| 7 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/web/docs/API.md | 26056 | ffcd0898f98ba317ab836918d7fc61fcf15f8a540ae639382c0e97b842ac6f08 |
| 8 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/audio/overlay/etc/tspi-audio/layout-presets.yml | 11228 | 4856109265371666a6001804330e2a595dad8599b3aecc2f4390be679b2427da |
| 9 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/upmix/examples/profiles/upmix-5.3.2-music.yml | 4842 | 14c00f949ad8c2952d42bc473edb13476370ce57af5356e5e1924e59de77d973 |
| 10 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/docs/MIMO-COEFF-PLAN.md | 45259 | fc832832487e73182787ad19e4fb3639edb3355584a33a5f9a4d707f539c3f30 |
| 11 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/audio/tools/mimo-engine/src/main.rs（只读原型事实） | 25273 | cf44c1e65b15fce87e3fe6b0432f72f741ae86070bcda5662cfc13a3d99d0eaa |
| 12 | C:/WorkSpace/Claw/projects/tspi-audio-dsp/scratch/lead/rt-reviews/T02-review.md（评审输入） | 36578 | ae440b999f31374ac6aba00e952ded2017d107a1bdd338e45cd940251603bc11 |
| 13 | rt-stack/reference/SPEC.md（T03 当前；N2/N3 已由其作者更新） | 22843 | f7f1a5337216138b1cffbf10f4a8652bb173f49108256d62790e0050e1b6962f |
| 14 | rt-stack/spec/fir-toolbox.md（T04 **v1.2-draft** 含 fs/2 裁决+errata A；N1/N2/N8） | 97033 | 86fb97fdd7dbbffda8207d49de0c4150e1b131fe470b7939a32b6900b777b6a5 |
| 15 | rt-stack/spec/compat-matrix.md（T05 当前；K34/R13 已修，Q-13） | 99248 | 1a46b4f86af9f38ebcd62a2f61a2929cdaf88e88cbdfbcc1a737f76e5ee0a1c1 |
| 16 | rt-stack/evidence/baseline/baseline-manifest.json（T01 baseline set：entries=137，baseline_set_sha256=`c187c752f1ef…`；MANIFEST.md 亦记） | 184583 | cbca0a102fb4c2008df7acdee2444c4621018c80bea2cb9a33243a7df7326acc |
| 17 | rt-stack/spec/mimo-t6.md（T73）；A.7/N6 | 87886 | 8ea1f61f0e4cf93bbf77d431771aa2be4135c22e781e4680144d1f273a3cc0e9 |

> v1.1.1 刷新（task-80）：T04 → v1.2-draft errata A（97,033 / 86fb97fd；N1/N2/N8），T01 → baseline set manifest（cbca0a / 184,583 B；entries=137，baseline_set=c187c752）；T03/T05/T73 保持当前快照。并发修订下结论仅对 A.2 快照成立。 **task-145 冻结刷新（2026-11-17 13:3x）**：本节跨文档 hash 为冻结基线（T04 已实测 97,033/86fb97fd）；本轮只改引用段、不改 §11/语义；互引 hash 不追随后续引用段编辑，实际值以 evidence/T02/manifest.json 与 SPEC-HASH-FREEZE.md 为准。 **v1.1.3 观测（不追改）**：A.2 #7 `web/docs/API.md` 已变为 26,790 B / `4255f1ad…`、#15 `compat-matrix.md` 当前 99,447 B / `2dcbd070…`、#17 `mimo-t6.md` 当前 88,106 B / `69ff5e9e…`；按 task-145 冻结策略本节只记冻结快照，追改待 owner（清单见 evidence/T02/ERRATA-v1.1.3.md）。

#### A.2.1 errata E1/E2 证据输入（2026-11-17，T105）

| 文件 | 字节 | SHA-256 |
|---|---:|---|
| rt-stack/evidence/INTEGRATOR-BATCH2/manifest.md（E1/E2 原始 errata） | 12175 | f86945e57f97a9c83e7187b01a3dc66716723aafe341e65e9fa993704d33db06 |
| rt-stack/crates/core/src/abi.rs（v1.1.2 E1 布局常量/断言；task-112/BATCH3 后） | 27555 | e4efa32ec53c82f6a9d011d51be962a280053b29ffdd128b2f66da4fa1c43700 |
| rt-stack/crates/rt-check/src/abi_layout.rs（RT_CHECK_ABI_LAYOUT_SCANNED=120；task-112/BATCH3 后） | 13612 | f9e29c98543563f63c882b19722842148b4589c5fcacc6106bdd97f71c6f2d7a |
| rt-stack/crates/rt-host/tests/abi_layout.rs（T06 自证偏移） | 7855 | bf4f00d749c48e6cf82f4425b48f9bf1cadb076449c05c4e32e5f3675b9b6e5c |
| rt-stack/crates/core/tests/const_contract.rs（PLAN_HEADER_* 常量断言 + tick=3；task-112/BATCH3 后） | 6806 | a173d1ce1aaa19b84e2441ed7292b78ed2de6033c521811a34bf1020f7856c07 |

**task-116 刷新（2026-11-17，BATCH3/task-112 后，只改引用/证据，不改布局语义）**：3 个代码文件按 `evidence/INTEGRATOR-BATCH3/hashes-changed-files.txt` 更新（abi.rs/abi_layout.rs/const_contract.rs）；另 2 项（INTEGRATOR-BATCH2 manifest、rt-host tests/abi_layout.rs）未变。BATCH3 证据：`INTEGRATOR-BATCH3/manifest.md`（7334 B / `0ad1365c0bf35601600c996ee287c555031b18949e867be50de730c2a56df307`）、`hashes-changed-files.txt`（1107 B / `dea4f958ff528ea0172c1028111cf78521dd26543427fd8816481c3fe9cec0ee`）、`SHA256SUMS`（1263 B / `31a2d87c280a4cf6923a69a942239f88dbf1afdb325b1d1cdf616097ec97712f`）。**历史 provenance（P3-1，non-active：只记录 task-116 刷新当时快照，不参与 fresh 判定；current spec 以 manifest/A.5 为准）**：task-116 刷新前 spec 曾为 173,986 B / 2,008 行 / `5487009cdfad530cad3d1eb422b3db89a6c316c28497fee9c1b7159ffe8b4603`（BATCH3/spec-identity.txt）；布局语义（PlanHeader 96/100/104/108、size 128、WorkerAffinity 32/8、F-14≤3、scanned=120）未变；刷新后仅 A.2.1 引用 hash 变化。

#### A.2.2 errata E3/E4 证据输入（2026-11-17，Cursor 终审；以实现为准）

| 文件 | 字节 | SHA-256（扫描时） |
|---|---:|---|
| `scratch/lead/cursor-audit/CURSOR-AUDIT-REPORT-v0.9.md`（F-10/F-11 终审报告；只读） | 34958 | 5f7ffddac0f0dea1fce00ea84ac8fbc9bd1d178e33e208c3c6206d1e39975bca |
| `scratch/lead/cursor-audit/CURSOR-AUDIT-FINDINGS-v0.9.json`（机器可读发现表；只读） | 29697 | c3f0b4a08f518d679fe26b4a9b1a984b72ca7cf1cce854074c8182cc6b616888 |
| `rt-stack/evidence/T66/manifest.md`（GAP-T66-2：非零 q16 按 `E_CA_DELAY_Q16_ABI` 拒收） | 14301 | 51c8702926b58059cee12b56c8642af35dc00ea873c4697ed8a9c3bfb1aa75db |

> E3/E4 输入 = Cursor 终审报告 + 实现现状（`abi.rs` 已列 A.2.1）+ T66 GAP-T66-2；三个文件均为只读消费，未改。旧 core-v1 hash 引用待刷新清单见本任务证据 `evidence/T02/ERRATA-v1.1.3.md`（不改他人 evidence）。

#### A.2.3 errata E5 证据输入（2026-11-18，N-09 收口；以实现为准）

| 文件 | 字节 | SHA-256（扫描时） |
|---|---:|---|
| `rt-stack/crates/rt-host/src/swap.rs`（task-207 后；`PLAN_FLAG_MASK=0x1F`、`parse_plan_arena` 拒收掩码外位） | 94212 | f325eaa6ce37b246d59bf57b8adf776906c83c8080e58e5e210572bf27f32d22 |
| `rt-stack/evidence/cursor-fix-v1-swapflag/MANIFEST.md`（task-207：swap 掩码修复与 T20 测试记录） | 7567 | ebba2a817e457130dec8587f9d5144e5d8d91a9f83a62d467ee5733b67a883dd |
| `scratch/lead/cursor-audit/CURSOR-AUDIT-REPORT-v1.1.md`（N-09/N-11 原始发现；只读） | 28567 | ca8c6e8788b32fe8bebb3bacc0b369c8a207034cfd5a7f2b5c03ade8091360e0 |
| `scratch/lead/cursor-audit/CURSOR-AUDIT-FINDINGS-v1.1.json`（机器可读发现表；只读） | 18769 | 344707c12290734a914ab0973dd4cf826be9f4fd8f3b637e103a85165fc2bf54 |

> E5 输入均为只读消费：实现现状（`swap.rs`）+ task-207 证据 + Cursor v1.1 复审计发现；本任务只改 T02 写域文档/证据，不改实现/ABI。N-11（format PUT 警告串）证据见 `evidence/cursor-fix-v1-p3/`。


### A.3 产物清单

| 文件 | 说明 | 字节/哈希 |
|---|---|---|
| rt-stack/spec/core-v1.md | 本规范（v1.1.3 + errata E5） | 见 evidence/T02/manifest.json 的 post-fill 记录（自引用文件不把最终 hash 写回正文） |
| rt-stack/evidence/T02/abi_vectors.py | 文件外 ABI/timing/GD 复算脚本（I10） | 见 manifest.json |
| rt-stack/evidence/T02/check_core_spec.py | 文件外结构化自检（I10；缺章节/行即 FAIL） | 见 manifest.json |
| rt-stack/evidence/T02/manifest.json | 输入/产物 hash、命令、exit code、post-fill 行数/字节/SHA-256、未测项 | 见自身 |
| rt-stack/evidence/T02/abi-result.txt / check-result.txt | 两个脚本的实际 stdout（含 exit code） | 见 manifest.json |

### A.4 生成/验证命令与 exit code

| 命令 ID | 命令（PowerShell，工作目录 C:/WorkSpace/Claw/projects/tspi-audio-dsp） | 期望/实际 |
|---|---|---|
| CMD-HASH | `Get-FileHash -Algorithm SHA256 -LiteralPath <A.2 的 17 个文件>` + `Get-Item .Length` | exit 0；结果=A.2 |
| CMD-ABI | `python rt-stack/evidence/T02/abi_vectors.py` | **exit 0**；`ABI_TIMING_SUMMARY fails=0`（18 结构 size/offset + timing/GD/回绕/容量） |
| CMD-DOC | `python rt-stack/evidence/T02/check_core_spec.py rt-stack/spec/core-v1.md` | **exit 0**；19 标题/6 组 139 项/40 事件/错误码/常量/14 FIXED+1 NOT-LANDED 摘要全部 PASS |
| CMD-OUT | `Get-Item rt-stack/spec/core-v1.md` + `Get-FileHash` + 行数 | exit 0；post-fill 数字写入 evidence/T02/manifest.json |
| CMD-SCOPE | `git status --porcelain -- rt-stack/spec/core-v1.md rt-stack/evidence/T02` | exit 0；只列本任务写域（真实输出见 manifest.json） |

说明：本任务没有任何 cargo test/运行测试/板端命令，故不存在“executed test_count”；P0 DoD 用人工检查表+手算+解析验证（§14、附录 B/C）。不把“未运行”写成 PASS。

### A.5 实测记录

- CMD-ABI：exit 0，`ABI_TIMING_SUMMARY fails=0`（输出见 evidence/T02/abi-result.txt；18 结构、4 组 gate、5 组 GD、2 个融合长度、3 组回绕、7 组容量断言）。
- CMD-DOC：exit 0，`SUMMARY total_items=139, v1_1_fixed=14, v1_1_not_landed=1, fails=0`（输出见 evidence/T02/check-result.txt；行数/字节/SHA-256 以 manifest.json 的 post-fill 记录为准）。
- CMD-HASH：exit 0；A.2 的 17 个文件 hash 与实测一致（含 T02-review）。
- CMD-SCOPE：exit 0；写域仅 core-v1.md + evidence/T02/*（见 manifest.json）。
- **post-fill 数字不写回正文以避免自引用循环**：最终 lines/bytes/SHA-256 由 evidence/T02/manifest.json 在本文最后一次编辑后运行生成；复审请用 `Get-FileHash -LiteralPath rt-stack/spec/core-v1.md` 复算并与 manifest.json 对照。
- v1.1.1 二修后重跑：`abi_vectors.py` exit 0；`check_core_spec.py` exit 0（139 项/15 FIXED/新增 N1–N6 回归断言）；`manifest.json` 已按 17 inputs 更新（A.2 快照）。
- v1.1.2 errata E1/E2 后重跑：`abi_vectors.py` exit 0；`check_core_spec.py` exit 0（139 项/15 FIXED/新增 E1/E2 断言）；manifest 已含 A.2.1 errata inputs。
- task-116/BATCH3 刷新后重跑：A.2.1 3 个代码 hash 更新；`abi_vectors.py`/`check_core_spec.py` exit 0（139 项/15 FIXED/fails=0）；manifest 更新（含 BATCH3 证据 hash）。
- task-118 终修复：A.2 #5 CONTRACT → 25,438 B / `bec36267…`；#6 ACCEPTANCE → 30,222 B / `c83671a3…`；P3 历史注记标 non-active；abi/check 重跑 exit0（139 项/15 FIXED/fails=0）。
- v1.1.3 errata E3/E4（Cursor F-10/F-11，以实现为准）：ABI 的 Path/Diag/Role 同槽恢复 reserved（28/28/24，必须 0）、I01 回退 NOT-LANDED；plan flags 统一 bit5..7=0。`abi_vectors.py`/`check_core_spec.py` 重跑 exit0（139 项/14 FIXED+1 NOT-LANDED/新增 E3/E4 断言）；manifest 与 `ERRATA-v1.1.3.md`（旧 hash 待刷新清单）更新。
- v1.1.3 errata E5（2026-11-18，v1.1 P3 收口，N-09）：`rt-host/src/swap.rs` 已在 task-207 收窄 `PLAN_FLAG_MASK=0x1F`；E4 行删除「0x3F 待对齐」旧述并保留历史快照；`abi_vectors.py`/`check_core_spec.py` 重跑 exit0（139 项/14 FIXED+1 NOT-LANDED/新增 E5 断言）；manifest 与 `ERRATA-v1.1.4.md` 更新。

### A.6 未测项与假设

| 项 | 状态 | 说明 |
|---|---|---|
| Rust 代码/编译/静态断言 | NOT_RUN | 无实现；T06 起真实执行（B-12..B-19/D-20） |
| cargo test / rt-check lint | NOT_RUN | 工具未实现；T06 必须有正/负 fixture 与 executed>0 |
| 板端 RT/时钟/拓扑/优先级 | NOT_RUN | 需 e2e-runner 授权专窗（T29/T45/T46/T30） |
| ALSA/CueMix/校准设备 | NOT_RUN | 本任务不碰板、不打开音频 |
| ABI 真实 Rust repr(C,align(64)) 布局 | NOT_RUN（设计期望） | 外部 Python 只复算字节布局；Rust 静态断言由 T06 关闭 |
| 独立第三方复审 | NOT_RUN | 需 lead 指派；本文件自评不等于独立复审 |

**假设（v1.1.1）**：1) runtime f32、f64 仅 reference/设计器；2) 生产 B∈{64,128}、fs∈{48k,96k}（Q-11）；3) MIMO participants≤8、P1=5；Path 表≤128，超出升版；4) master=worker0、tick 预算≤3；5) block payload 在 RtArena 固定槽、SPSC 只传 64B desc；6) graph_id=SHA-256、header CRC-32C、LE、v1 无 FFI；7) 事件 8 lane×128 槽；8) 安全输出仅 ProtectionFallback/SilenceMute；9) v1.1 新增 ABI 字段复用 reserved 区、size 不变（additive）；10) T04 v1.2-draft/T05/T03/T73 当前 hash 见 A.2，独立复审时重采。

### A.7 与 T01/T03/T04/T05/T73 的只读交叉复核（v1.1.1 重做，2026-11-17）

| 对方条款（当前 hash 见 A.2） | 本规范条款 | 结论 |
|---|---|---|
| T03 SPEC:148 “参考/测试候选含 64/128/256；runtime 只 {64,128}”；T04 v1.2 errata A 已把 T04:45/§7.3 改为“生产候选仅 64/128，256 参考/测试附加块长” | §3.2 BLOCK_SIZES {64,128}；Q-11 | **N2/N8 CLOSED**：T03/T04 现行文字均与 T02 runtime {64,128} 一致；原 A.7 旧句（T04 仍写 512/256 待改）已删除。 |
| T03:70 注“图内 D 只整数；T04 半/分数 D 由 T12 编到 P/A 或 plan delay_q16；reference 只验证整数 D” | §11.10（E3：ABI v1 无 delay_q16）、§15.1 T12/T15 | **N3 CLOSED（E3 后收窄）**：T02 硬规则——T12/T15 在 T03 reference 对拍前必须把全部分数/非整数 D 折入 P/A 系数（对拍图 q16=0），或声明 T03 schema 扩展；T15 不得把 q16 当 T03 graph.D；ABI v1 的 reserved 槽必须 0（不承载 q16）。 |
| T04 v1.2-draft（97,033 B / `86fb97fd…`）：§6.4 freq<=fs/2；带 phase <fs/2；=fs/2→E_TGT_PHASE_REAL；>fs/2→E_TGT_FREQ_RANGE；§6.9 28 码 | 本文不定义 target schema；§15.2 Q-12 | **N1 CLOSED**：Q-12 标 RESOLVED，引 T04 §6.9/§6.4 与 RULING-fs2-2026-11-17.md；A.2 #14 已更新 86fb97fd |
| T05 当前（1a46b4f8）：K34“拒收码全集见 T04 §6.9（不写死码数）”；Y17 以 T04 现行 §6 为准；R13/R14 runtime 仅 64/128；X09 ≤fs/2 | §15.2 Q-11/Q-12/Q-13 | **Q-13 RESOLVED**：T05 23 码/旧 T04 hash stale 已关闭；X09 ≤fs/2 与 magnitude-only 相容，phase 例外经 Y17 以 T04 为准。 |
| T03:129 L_F=L_A+L_G+L_P−2 vs T04:650 L_G+L_P−1 | §7.1/§11.10 融合口径 | 范围差非矛盾：T03 是完整图（含 A_i），T04 是 G/P 融合；T12 若 A_i≠δ 按 T03 补 L_A（§15.1）。 |
| T01 baseline set（baseline-manifest.json `cbca0a…` / 184,583 B；entries=137，baseline_set_sha256=`c187c752…`；A.2 #16） | 不定义板端；文本引用 | **N6 CLOSED**：补引当前 baseline set；BASELINE-INVENTORY 9f5182d1 为其摘要之一，无冲突（本任务未改 T01） |
| T73 mimo-t6（8ea1f61f，A.2 #17） | 使用 graph_id/plan hash/整数 D0（FC-03/04/05/13） | **N6 CLOSED**：不引用 T02 ABI/delay_q16/block256；无新冲突（其 D0 为整数公共延迟，q16 不影响 baseline/on 比较）。 |

## 附录 B：手算/解析验证向量与命令

> v1.1 起本附录只保留**期望值/公式**与外部脚本入口；脚本本体移至 evidence/T02（I10）。脚本执行结果见 A.5/C.3 与 manifest.json。

### B.1 时限（T_block、gate）

公式：`T_block = B/fs`；`gate = T_block/2`（50% deadline）。

| fs | B | T_block | gate_ns（=B/(2fs)×1e9） | 用途 |
|---:|---:|---|---:|---|
| 48000 | 128 | 2.666666667 ms | 1333333.333 ns | 128 满图验收门 |
| 48000 | 64 | 1.333333333 ms | 666666.667 ns | 64 独立最终窗 |
| 96000 | 128 | 1.333333333 ms | 666666.667 ns | 96k 按自身缩放 |
| 96000 | 64 | 0.666666667 ms | 333333.333 ns | 96k 64 帧 |

实现用整数 ns 时必须取更严值（gate 向下取整）；统计比较用 ticks 精确值，不先浮点四舍五入。

### B.2 线性相位群延迟 / 融合长度（48k）

| L | 群延迟 | 备注 |
|---:|---:|---|
| 4096 | 42.65625 ms | 常规上限；超 15/25/40 全部附加预算 |
| 8192 | 85.322916667 ms | extended 准入 |
| 16384 | 170.65625 ms | extended 上限 |
| 5119 | 53.3125 ms | 4096+1024−1（fused 示例） |
| 12287 | 127.979166667 ms | 8192+4096−1（fused 示例） |

### B.3 容量/回绕边界

| 项 | 值 | 说明 |
|---|---:|---|
| capture payload/block | 20×128×4 = 10240 B | 20 输入、B=128、f32 |
| playback payload/block | 22×128×4 = 11264 B | 22 输出 |
| capture ring payload | 4×10240 = 40960 B | 固定 4 槽 |
| playback ring payload | 4×11264 = 45056 B | 固定 4 槽 |
| event ring | 1024×64 = 65536 B | 8 lane×128 |
| quarantine payload | 8×11264 = 90112 B | 8 槽隔离 |
| ack 槽 | 4×64 = 256 B | §7.3 |
| worker stats | 4×64 = 256 B | 4 worker |
| hist | 6×4×32×4 = 3072 B | u32 桶 |
| u32 block_seq@750 block/s | ≈66.28 天 | 故 block_seq/epoch 用 u64 |
| u64 block_seq@750 block/s | ≈7.79×10^8 年 | 实际不可回绕 |
| u64 frame_index@48k | ≈1.22×10^7 年 | 同上 |
| delay_q16（内部模型） | 0.5 sample → 32768 | Q16 换算；残差<1/65536；ABI v1 不承载（reserved 必须 0，E3） |

### B.4 ABI 大小/偏移（v1.1.3 期望值；由 evidence/T02/abi_vectors.py 独立复算）

**v1 结构（11 个）**

| 结构 | 大小 B | align（Rust） | 关键偏移 |
|---|---:|---:|---|
| TspiBlockDescV1 | 64 | 64 | epoch=8, block_seq=16, frame=24, plan_version=32, flags=36, valid=40, slot=44, publish=48, deadline=56 |
| TspiEpochInfoV1 | 64 | 64 | plan_version=8, fs=12, B=16, n_in=18, n_out=20, mode=22, flags=24, start=32 |
| TspiHwParamsV1 | 64 | 64 | sample_format=2, n_in=4, n_out=6, fs=8, period=12, buffer=16, in_stride=20, out_stride=24, flags=28 |
| TspiPlanHeaderV1 | 128 | 64 | graph_id=36, total_coeff=68, path_off=72, diag_off=76, in_stride=80, out_stride=84, role_off=88, role_count=92, **reserved1=94, affinity_off=96, affinity_count=100, reserved2=102, ctl_off=104, ctl_len=108, reserved3=112**, crc=124 |
| TspiEventV1 | 64 | 64 | mono=8, epoch=16, block_seq=24, kind=32, lane=34, severity=35, value_a=36, value_b=40, payload=48 |
| TspiBlockAckV1 | 64 | 64 | phase_done0=16,1=20,2=24, cutoff=28, plan_version=32, expected=36, flags=37, deadline=40, compute=48 |
| TspiSpscHeaderV1 | 64 | 64 | capacity=8, entry_stride=12, write=16, read=24, dropped=32, epoch=40 |
| TspiWorkerStatsV1 | 64 | 64 | 8×u64 连续 |
| TspiPathV1 | 32 | 8 | in=0,out=2,taps=4,delay=8,polarity=12,flags=13,coeff_off=16,coeff_len=24,**reserved1=28（必须 0；E3）** |
| TspiDiagFirV1 | 32 | 8 | out=0,flags=2,taps=4,delay=8,res0=12,coeff_off=16,coeff_len=24,**reserved1=28（必须 0；E3）** |
| TspiRoleV1 | 32 | 8 | role=0,out=2,flags=4,delay=8,gain=12,polarity=14,peq=16,name_hash=20,**reserved3=24（必须 0；E3）** |

**v1.1 新增共享结构（7 个）**

| 结构 | 大小 B | align（Rust） | 关键偏移 |
|---|---:|---:|---|
| TspiRtControlV1 | 128 | 64 | block_start_seq=8, block_start_epoch=16, block_start_frame=24, plan_version=32, slot=36, current_ptr=40, switch_state=64, safety_request=68, heartbeat=80, spin=88, drain=92, tick_freq=96, tick_budget=100, capture_ring=104, playback_ring=112 |
| TspiSwapAckV1 | 64 | 64 | ack_mask=8, retire_ready=12, retire_epoch=16, next_plan=24, old_plan_ptr=32 |
| TspiEventRingLaneV1 | 64 | 64 | write_seq=0, read_seq=8, dropped=16, overflow=24, records_offset=32, capacity=36, lane=40 |
| TspiWorkerRegsV1 | 128 | 64 | cpu_index=8, sched_priority=12, spin=16, drain=20, rt_control=24, plan_arena=32, ack_slots=40, stats=48, event_lane=56, required_ack_mask=64, scratch=72, affinity_index=88 |
| TspiMasterRegsV1 | 128 | 64 | rt_control=0, capture=8, playback=16, registry=24, ack_base=32, stats=40, event_lane=48, worker_regs=56, crossfade_budget=64, tick_freq=72, tick_budget=76, expected=80 |
| TspiCrossfadeBudgetV1 | 64 | 64 | old_ver=0, new_ver=4, old_delay_q16=8, new_delay_q16=12, frames=16, flags=20, peak_cpu=24, peak_ram=32, avail_cpu=40, avail_ram=48 |
| TspiWorkerAffinityV1 | 32 | 8 | worker_id=0, sched_priority=1, flags=2, cpu_index=4, irq_mask=8 |

> Python `struct` 只验证无隐式 padding 的字节布局；Rust `repr(C,align(64))` 的 align/size 必须由 T06 静态断言验证（B-12/B-19）。本表是 T06 的期望输入。**align 例外（N4）**：`TspiWorkerAffinityV1` 固定 32B/align8（§11.13），其余 6 个共享结构 align64——T06 B-18 必须按 8 断言，不得按 intro 误期 64。

### B.5 外部复算脚本（I10 移出正文）

~~~text
命令：python rt-stack/evidence/T02/abi_vectors.py
覆盖：18 个结构 size/offset；4 组 gate；5 组线性相位 GD；2 个融合长度；3 组回绕；7 组容量/常量
结果：exit 0，ABI_TIMING_SUMMARY fails=0（见 evidence/T02/abi-result.txt 与 manifest.json）
~~~


## 附录 C：P0 评审记录与文档自检

### C.1 评审输入与方法

- 输入：A.2 的 17 个文件（hash 固定）+ T02-review 15 项。
- 方法：逐条按 §0.1/§14 表格核对；ABI/timing/GD 用 evidence/T02/abi_vectors.py 独立复算；文档结构/检查表用 evidence/T02/check_core_spec.py 结构化自检（**脚本在文件外**，故若章节/表格被删，检查会失败；I10）。
- 限制：未实现 Rust/未跑 cargo/未上板；自评不等于独立复审。

### C.2 文档自检（结构化，可证伪；I10）

~~~text
命令：python rt-stack/evidence/T02/check_core_spec.py rt-stack/spec/core-v1.md
检查：19 个章节；A-28/S-13/P-15/B-19 组连续性/状态枚举/证据列；F-44 规范表（RT-FORBID+失败判据）+ F-44 复核表；D-20（RT-DROP）；事件 kind 1..40；错误码 101..407 关键集；v1.1 摘要 I01..I15（14 FIXED + I01 NOT-LANDED/E3）；E3/E4 errata 断言（reserved 槽/bit5..7=0）；24 个常量；无未完成标记
结果：exit 0；SUMMARY total_items=139, v1_1_fixed=14, v1_1_not_landed=1, fails=0（见 evidence/T02/check-result.txt 与 manifest.json）
~~~

检查项总数 = A28 + F44 + D20 + S13 + P15 + B19 = **139**（v1.0 的 118 项经 v1.1 增补后为 139；状态分布 137 PASS-DESIGN + 2 DEFERRED-CODE（D-20/B-12），无 FAIL/空状态）。

### C.3 执行结果（post-fill）

| 命令 | 结果 |
|---|---|
| CMD-HASH | exit 0；A.2 的 17 个文件 hash/字节与记录一致 |
| CMD-ABI | exit 0；`ABI_TIMING_SUMMARY fails=0`；18 结构+timing/GD/回绕/容量全部 PASS |
| CMD-DOC | exit 0；`fails=0`；139 项结构/内容检查 PASS；事件 kind 1..40、错误码关键集、24 常量、15 项摘要 PASS |
| CMD-OUT | post-fill lines/bytes/SHA-256 记录在 evidence/T02/manifest.json（本文最后一次编辑后生成；正文不回写以避自引用） |
| CMD-SCOPE | exit 0；git status 仅列 core-v1.md + evidence/T02/*，无越写 |

**v1.1.1 二修后重跑（2026-11-17）**：CMD-ABI exit 0（fails=0）；CMD-DOC exit 0（139 项/15 FIXED/N1–N6 回归断言全 PASS）；manifest.json 按 17 inputs + 新产物 hash 重新生成（见 A.2/A.5/evidence/T02）。

**v1.1.2 errata E1/E2 后重跑**：ABI/DOC 均 exit 0；PlanHeader affinity/ctl=96/100/104/108（size 128 不变）；rt-check abi-layout scanned=120（旧109）记录于 §11.2/§11.10；manifest 更新含 A.2.1 errata inputs。

**task-116/BATCH3 刷新**：A.2.1 的 abi.rs/abi_layout.rs/const_contract.rs hash 更新为新值；spec 布局语义复核不变（原 5487009c/173986/2008 见 BATCH3/spec-identity.txt）；ABI/DOC 重跑 exit 0，manifest 更新。

**task-118 终修复**：A.2 #5 CONTRACT 25,438/`bec36267…`、#6 ACCEPTANCE 30,222/`c83671a3…` 刷新；A.2.1 历史 provenance 标 non-active（不参与 fresh 判定）；abi/check 重跑 exit0，manifest 更新。

**v1.1.3 errata E3/E4 后重跑**：ABI/DOC 均 exit 0；`TspiPathV1/TspiDiagFirV1` reserved1@28、`TspiRoleV1` reserved3@24（size/对齐/其余偏移不变）；plan flags bit5..7=0；I01 NOT-LANDED；139 项检查表不变；manifest/ERRATA-v1.1.3 更新。

**v1.1.3 errata E5 后重跑（2026-11-18，N-09）**：ABI/DOC 均 exit 0；E4 行当前事实改为 `PLAN_FLAG_MASK=0x1F`（0x3F 仅历史快照）；139 项检查表不变；manifest/ERRATA-v1.1.4 更新。

### C.4 结论

| 项 | 结论 |
|---|---|
| T02-review 15 项 | **14/15 FIXED + I01 NOT-LANDED（v1.1.3 E3 回退，以实现为准；P1 5/P2 9/P3 1；0 DECLINED）**，逐条见 §0.1 |
| T02-rereview 新问题 | **N1–N8 全部 CLOSED（T04 v1.2 errata A 已修 N8；Q-12/Q-13 RESOLVED）**；0 DECLINED
| errata E1/E2/E3/E4/E5 | **FIXED（v1.1.3）**：PlanHeader v1.1 affinity/ctl 对齐 96/100/104/108、size 128（E1）；F-14 tick ≤3（E2）；E3 delay_q16 回退 RESERVED-0（I01 NOT-LANDED）；E4 plan flags bit5..7=0；abi-layout scanned=120；A.2.1 证据；**E5（2026-11-18，N-09 文档收口）：`swap.rs` 掩码当前事实 0x1F（0x3F 仅历史快照），A.2.3 证据** |
| 文档结构/内容自检 | CMD-DOC exit 0；139 项全部 PASS（脚本文件外、可证伪） |
| ABI/timing 复算 | CMD-ABI exit 0；fails=0 |
| 代码级 | NOT_RUN（无实现）；T06 起用真实 lint/测试关闭 D-20/B-12/B-13..B-19 |
| 独立复审 | 未执行；需 lead 指派非 rt-api reviewer 按 §14.2–§14.4 与 A.2 hash 复核（作者自评不能替代） |
