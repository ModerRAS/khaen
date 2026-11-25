# khaen v2 设计说明

## 1. 目标与非目标

**目标**：一套可在 x86 小机上长期稳定运行的实时音频处理栈——内容进去、按编译好的方案处理、按房间校准后的系数出声；校准流程可复现、可验证、可回滚；设备上跑的每个二进制都能从某个提交重建。

**非目标**：不做通用 DAW；不做多房间/网络分发；v2 不实现多点校准（等麦克风可搬运再做）。

## 2. 四条不可谈判约束

1. **仓库只放源码、规格、构建脚本**。测量证据、报告、截图一律放仓库外的交接目录。
2. **设备上跑的东西必须能从某个提交重建**：CI 构建 → release 目录带 manifest + sha256 → 原子切换。禁止手改设备、禁止原地覆盖二进制。
3. **控制面只有一个后端**（HostEngine）。拿不到的真实字段返回 null + 原因，不允许用内存模拟值冒充 actual。
4. **执行图允许预编译的矩阵级**（混音只发生在编译期确定的节点），运行期仍然零分配、零锁、无 I/O。

## 3. 分层与链序

运行时（数据面）：

    内容适配 → 分频（切 lane）→ MIMO 矩阵 G → 保护（延迟/极性/电平/PEQ/HP-LP）→ 角色到 DAC 路由 → 声卡

- 内容适配：布局映射与中置折叠，在图之前（图内禁止运行期混音）。
- 分频：定义 lane 的物理含义，对角处理。
- 矩阵 G：满矩阵，跨 lane；目标域速率 3 kHz（按 150 Hz 修正带宽选的，非 12 kHz）。
- 保护：最后一层，对角，最小相位，负责不烧单元。
- 控制面（daemon）与数据面（host）分离，只经 Unix socket 通信。

## 4. 模块清单

| 模块 | 处置 | 说明 |
| --- | --- | --- |
| designer（FIR 工具箱） | 迁入 | 纯数学，测试充分 |
| calibration 数学（去卷积、PEQ 拟合、T64/T65/T66） | 迁入 | 校准核心 |
| mimo-design（solve/quality/authority/limits/objective） | 迁入 | v2 新写的求解能力 |
| modal-design / room-sim | 迁入 | 离线设计工具 |
| aoip-aes67（SDP/RTP/抖动缓冲） | 迁入 | 协议细节已踩过坑 |
| rt-check（ABI/禁止调用/析构审计） | 迁入 | 门禁工具 |
| core（方案格式/内核/RT 类型） | 部分重写 | 格式升 v3；卷积内核复用 |
| rt-host（12k 行单文件引擎） | **重写** | 四套逻辑叠加，是混乱源头 |
| control（手写 HTTP + MemoryEngine） | **重写** | 假后端冒充 actual 的源头 |
| webui（Vue 控制台） | 保留 | 与后端解耦，改 API 适配 |
| deploy | **重写** | 改为 CI 构建 + manifest + 原子切换 |

## 5. 契约

三份契约见 `CONTRACTS.md`：方案文件 ABI v3、主机控制协议、校准产物 manifest。

## 6. 工作分解（WBS）

依赖标记：→ 表示前置。估时单位为人日。

### 波次 0：仓库与契约（M0/M1，2 天）

| ID | 任务 | 依赖 | 验收 |
| --- | --- | --- | --- |
| T00 | 分支整理：历史留在 `legacy/pre-rewrite`，`main` 从骨架重开 | — | 两条分支都可拉取；main 无旧历史 |
| T01 | 设计说明（本文件）+ 三份契约 | — | 评审通过，契约含字段级定义 |
| T02 | cargo workspace 骨架（空 crate 可编译） | T01 | `cargo check --workspace` 通过 |
| T03 | CI：fmt / clippy(D) / test / 门禁 | T02 | CI 绿；未过则合并被挡 |
| T04 | 证据外置规则 + .gitignore | T01 | 仓库内无 evidence/artifacts |

### 波次 1：数学库迁入（M2，4 天）

| ID | 任务 | 依赖 | 验收 |
| --- | --- | --- | --- |
| T10 | core：方案文件 v3 + 卷积内核 + RT 类型 | T02 | 解析/序列化往返一致；内核与旧实现逐样本一致（对拍） |
| T11 | designer 迁入 | T02 | 原测试全绿 |
| T12 | calibration 数学迁入（去掉仓库外证据依赖） | T02 | 原测试全绿；无外部路径引用 |
| T13 | mimo-design 迁入（含 quality/authority/limits） | T02 | 2699 基线对应用例全绿 |
| T14 | modal-design / room-sim 迁入 | T02 | 原测试全绿 |
| T15 | aoip-aes67 迁入 | T02 | 原测试全绿 |
| T16 | rt-check 迁入 + 接 CI | T03 | 门禁可跑；故意违规能红 |

### 波次 2：新 host 引擎（M3，6 天）

| ID | 任务 | 依赖 | 验收 |
| --- | --- | --- | --- |
| T20 | 块引擎骨架：单设备 owner、128 帧、SPSC、零分配门禁 | T03 | 假后端跑通；RT 路径零分配/零锁有测试 |
| T21 | 图执行器：content → crossover → matrix → protection | T10,T20 | identity 系数下逐样本等价于直通 |
| T22 | 矩阵级：3 kHz 域、卷积方式可选（直接/分区）、块边界切换 | T21 | 频响与离线参考一致；延迟实测上报 |
| T23 | 准入：内存/延迟/系数预算 | T22 | 超预算 fail-closed |
| T24 | 故障隔离：last-good / safe-mute / xrun 处理 | T20 | 故障注入后进安全态，不崩 |
| T25 | 主机控制 socket：status/mute/prepare/commit/abort/coeff | T20 | 协议一致性测试；未知 op 拒绝 |
| T26 | 性能与 RT 门禁：延迟、CPU、xrun 长稳 | T22,T25 | 8 小时无 xrun；延迟在预算内 |

### 波次 3：daemon 与界面（M4/M5，5 天）

| ID | 任务 | 依赖 | 验收 |
| --- | --- | --- | --- |
| T30 | daemon 骨架 + HostEngine（唯一后端） | T25 | 无 MemoryEngine；能力上报真实 |
| T31 | REST 契约实现（null + 原因规则） | T30 | 每个字段有来源；假值零容忍 |
| T32 | 单点校准作业编排（测量→设计→编译→apply） | T13,T31 | 端到端跑通；产物带 manifest |
| T66（v2 部分） | 设计产物 → plan ABI v3 编译：`khaen_calibration::plan_compile` 把 `PeqDesign` 的模态 PEQ 链编译成保护段 + 身份 lane 链，输出**确定性** bytes/sha256/graph_id，测试证明往返、可复现、host 准入与图编译接受 | T32 | 测量→设计→编译一段已接通；capture/apply/remeasure 仍是 v1 遗留，标 NOT_RUN |
| T66v3 | 单点可复现 artifact 包（`single_point_bundle`）：params/report/plan/manifest + 整包 sha256，不可变写入与磁盘校验 | T66（v2） | 同输入同字节/同摘要；删改可检测；包内方案被 host 接受 |
| T66v4 | artifact 包进校准 job 证据链（`single_point_job`）：类别映射、幂等、不可变、Prepared 阶段磁盘复核 | T66v3 | job 终态 manifest 含四类 artifact；篡改在 Prepared 被稳定码抓住 |
| T66v5 | 单点场景作业桥（`single_point_scene`）：`khaen-mimo-design` 场景级单点求解（质量门 4.1 / 权威加权 4.4 / 安全钳位三态 4.4）→ report/metrics/FIR artifact 登记进 job | T66v4 | 三态与 `measured/applied=false` 原样透传；同输入逐字节复现；job 证据链可复核 |
| T33 | webui 适配 + 单点向导 | T31 | 向导可点完；无 not_wired 假徽标 |
| T34 | 认证与会话（沿用 v1 契约） | T30 | 登录/重启持久/错密码拒绝 |

**T66v5 的依赖与诚实边界**：`khaen-calibration` 现在依赖 `khaen-mimo-design`，后者是**合成场景沙盒**（其 lib 文档与报告都写 SIMULATED / UNPROVEN）。桥只做"场景结论 → 证据链文件"，不把 predicted 写成 measured（`judgement.measured/applied` 透传，恒 false），也不自己判定 FULL/PARTIAL/NONE。真实测量输入、apply 到设备、闭环复测仍属 T42/v1 遗留，标 **NOT_RUN**，需要出声窗口。

### 波次 4：部署与闭环（M6，4 天）

| ID | 任务 | 依赖 | 验收 |
| --- | --- | --- | --- |
| T40 | 构建流水线：CI → release 目录 + manifest + sha256 | T03 | 产物可从提交重建；manifest 可校验 |
| T41 | 设备安装/回滚脚本（原子切换） | T40 | 安装与回滚各演练一次 |
| T42 | 单点闭环复测（需出声窗口） | T32,T33 | 预测 vs 实测 ≤2 dB；失败如实记录 |
| T43 | 交接与文档 | 全部 | HANDOFF 完整；CHANGE-LOG 追加 |

## 7. 里程碑

| 里程碑 | 内容 | 出口判据 |
| --- | --- | --- |
| M0 | 契约定稿 | 三份契约可评审 |
| M1 | 骨架 + CI | CI 绿、空仓可编译 |
| M2 | 数学库就位 | 数学 crate 测试全绿 |
| M3 | 新 host 可出声 | 假后端 + 真 ALSA 各跑通一次 |
| M4 | 控制面可用 | 五个页面无 not_wired |
| M5 | 单点向导可用 | 向导端到端可点完 |
| M6 | 闭环验证 | 预测 vs 实测 ≤2 dB、可复现、可回滚 |

## 8. 风险

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| 数学库迁移带入旧依赖（外部证据路径） | 新仓不可独立构建 | 迁移时逐个 crate 断外部依赖，测试用合成夹具 |
| 3 kHz 矩阵域重解算 | 系数不兼容 | 重新求解，不复用 12 kHz 库 |
| 真实闭环依赖出声窗口 | 排期不确定 | 离线部分先行，窗口只用于 T42 |
| 未来源码与部署再次漂移 | 回到今天的困境 | 约束 2 用 CI + manifest 强制 |


## 设备生命周期与采样率归属（T70，系统级，不靠补丁）

### 为什么定这一节

MOTU UltraLite-mk5 的采样率是**设备自身保存的状态**：空闲或 USB 重枚举后回落到上次速率（历史现场是 192 kHz），而 192 kHz 下 TOSLink 带不动 PCM、AES67/桥接链路也可能整体错位。v1 的做法是"48k keeper 定时灌 1 秒静音"+"面板把默认值改成 48k"，这两者都是**补丁**：

- keeper 是定时器，抓不住"刚枚举、还没开流"的窗口；
- 面板默认值是人工状态，换设备/换人/恢复出厂就丢；
- 两者都没有把"速率"纳入系统的准入与观测，出错时是静默的。

### 设计：宿主拥有速率，设备只是执行者

1. **速率是方案数据**：`Plan.sample_rate`（ABI v3 已有）是唯一事实来源；daemon 在 `prepare` 时校验"方案速率 == 播放链路速率 == AES67 流速率"，不一致直接 `admission_rejected`，不允许靠重采样掩盖配置错误。
2. **设备监督器（device supervisor）持有设备**：khaen-host 内唯一所有者，监听 USB/网络设备事件（udev/netlink 或轮询 + 去抖），而不是等定时器。
3. **状态机（每一步都有进入条件与失败码）**：

   | 状态 | 进入条件 | 失败码 |
   | --- | --- | --- |
   | Absent | 设备不在总线上 | `device_absent` |
   | Enumerated | USB 音频设备出现 | `device_open_failed` |
   | ControlReady | 控制通道可用（MOTU：CDC 网口 up + 地址 + ping/WS；ALSA：控制项可读） | `control_channel_unavailable` |
   | RateAsserted | **写入/设置速率并回读一致** | `rate_assert_failed` |
   | Streaming | 按方案速率打开音频流 | `stream_open_failed` |

4. **每次枚举都重新断言速率**：`RateAsserted` 不是一次性动作；设备事件触发后自动重跑。速率写不进去就 **fail-closed**（进安全静音 + 状态上报），**绝不静默接受设备保存的旧速率**。
5. **控制通道归监督器管**：v1 的 `motu-net-up` 本质就是这一步，但它挂在 oneshot unit 上、不在设备事件上触发，所以每次重插都要人工补 —— 这是"补丁感"的根源。v2 把它并进监督器状态机。
6. **观测**：`status` 必须暴露 `device_rate` / `plan_rate` / `rate_asserted` / 失败原因；出现不一致时上报 `rate_mismatch`，而不是让用户靠面板去猜。
7. **不保底也不猜**：没有"保持 48k 静音流"这类隐藏动作；需要占住设备时由监督器的 `Streaming` 状态显式声明（例如 idle 时保持一条静音流，理由与状态一起上报）。

### 与现有 v1 部署的关系

v1 的 `tspi-motu-48k-keeper` 与 `tspi-motu-net.service` 在 v2 里被**监督器取代**，不再是独立服务；迁移期保留它们作为只读诊断（`WRITE_BACK=0`），不再靠它们保证正确性。

### T70 子项进度（如实）

| 子项 | 内容 | 状态 |
| --- | --- | --- |
| T70a | ALSA 输出后端（真实 PCM 播放 + xrun 恢复），平台无关逻辑 + 可注入 PCM | 已提交 `9b0604d`；真实 ALSA 播放 **NOT_RUN**（Linux 上待设备窗口） |
| T70b | AES67 输入后端（RTP/L24 收包 + jitter + SSRC 切换） | 已提交 `ae825e1`；真实 UDP 收包在 `Runtime` 里接线（T70d） |
| T70c | 设备监督器状态机（枚举→控制通道→速率断言→流式 + 插拔自恢复） | 已提交 `291a263` |
| T70d | 单进程运行时：AES67 收包线程 → SPSC → SlotEngine → ALSA 输出 | 已提交 `af72fff`（13 个运行时测试，含零分配块路径）；真实 UDP/ALSA 出声 **NOT_RUN** |
| T70f | `khaen-rt-host` 进程：把 T70d 的 `Runtime` 做成可执行文件（环境变量配置、方案文件加载、统计输出） | 已提交；Windows 只能编译（stub），Linux 真机运行 **NOT_RUN** |
| T70e | daemon 接线：`/api/device`、`/api/device/retry`、安全锁存 fail-closed 静音、`Box<dyn>` 组件注入 | 已提交 `22cc206` |

## 进程与线程模型

当前形态（T70d 交付的最小可运行单元）：**一个 `Runtime` 对象**把三段串起来，线程布局如下。

| 线程 | 职责 | 实时约束 |
| --- | --- | --- |
| 收包线程（非 RT） | `UdpSocket` 组播 join → `Aes67Input::feed_packet` → SPSC producer | 允许分配/IO，但不碰引擎与设备 |
| 实时块线程（RT） | `Runtime::run_block`：SPSC consumer → SlotEngine（含方案图）→ ALSA 输出 | 零分配/零锁/无 IO/不 panic；缓冲全部构造期分配 |
| daemon 轮询线程（非 RT） | 设备监督器 `poll`、安全锁存联动 | 不碰 RT 缓冲；锁只保护控制面 |

约束与未决：

1. **设备单一 owner**：设备监督器在 daemon 里（`/api/device` 是其快照）；实时侧打开 PCM 之前，必须由监督器把设备推进到 `Streaming` 且速率已回读断言——否则 fail-closed。
2. **进程边界未定（T70f）**：当前 `Runtime` 与 daemon 在同一台机器上、进程边界待定；两进程之间的方案切换通道（共享内存 ring 或 unix socket）**尚未实现**。在它有之前，daemon 的 `prepare/commit` 只作用于 daemon 自己的 `SlotEngine` 副本，**不宣称**已影响正在出声的 `Runtime`；`/api/status` 不会把这种状态报成"已生效"。
3. **单进程模式可行**：若短期先要一处出声，可把 `Runtime` 跑在独占设备的进程里、daemon 只做只读观测；这条路径不需要 IPC，但方案切换要重启该进程（UX 上明确标注）。

## 五个语义界面（surface）进度（如实）

| 界面 | 状态 | 说明 |
| --- | --- | --- |
| 音量 | **两阶段接通**（`surface=volume`） | 以最近一次 commit 生效的方案为基线，patch protection 逐 lane 增益；commit 在块边界生效 |
| 路由 | **两阶段接通**（`surface=routing`） | patch lane→DAC 路由与 dac_channels |
| 内容布局 | **两阶段接通**（`surface=layout`） | patch content 的 in_channels / out_lanes / folds |
| 格式 | **NOT_WIRED**（返回 `unsupported` + 原因） | 采样率/块长/通道变更需要重开音频设备，不能表示为方案补丁；需与设备监督器一起设计 |
| 模式 | **NOT_WIRED**（返回 `unsupported` + 原因） | 输入模式（off/toslink/aes67/…）是采集源与设备监督层决策，不是方案字段 |

`status` 的 `surfaces` / `surface_reasons` 逐项如实上报；未接通的界面不得返回假成功。
