# 需求文档

状态：**已定稿（2026-11-22）**。技术方案见同目录 `design.md`。

## 介绍

NSP 以后用原生布局 `5.1.2` 发一路 8 声道 AES67（`L, R, C, LFE, Ls, Rs, Ltf, Rtf`，48 kHz）。dsp-host 在进现有 4.1.2 播放图之前，把中置等能量折进左、右声道，其余声道按物理 4.1.2 的 7 个音箱槽位送出。低音炮逐样本保留，不参与折叠。

折叠发生在已经存在的 plan 前内容适配层（`[content]` + `CenterDownmixPlan`）。播放图本身仍然禁止混音。本需求不改 NSP，不部署，不出声，不碰正在 soak 的 9.1.6/16ch 生产。

## 已确认

1. 下混只复用 `rt-host` 自持的 `content_map::CenterDownmixPlan`。系数沿用已冻结的 `CENTER_DOWNMIX_GAIN_LINEAR = 0.7079458`（`10^(−3/20)`，正好 −3.000 dB）。不另引入 `0.70710678`（`1/√2`，−3.0103 dB）。两者差约 0.01 dB，证据里写明即可。
2. 新的输入布局 id 定为 `nsp-5.1.2`：单流 `streams: [8]`，角色顺序 `FL FR C LFE SL SR TFL TFR`，通道 0..7。现有 `h16-5.1.2`（`streams: [6, 2]`）保持原样。
3. 输出布局继续用现有 `4.1.2`（无 C；`FL/FR/SL/SR/HFL/HFR` 占槽 0..5，`SUB1` 占槽 7，槽 6 空着）。不改它的角色 id 和物理槽位。
4. 内容适配作用在 **5.1.2 顺序的 8 声道源缓冲**上，发生在 plan 取通道之前。适配之后，新 plan 按下面的 `capture_index` 送进现有 4.1.2 的 8 槽容器。现有 7 声道 identity plan（`capture_index = [0,1,2,3,4,5,null,6]`）不能拿来接这路 8 声道。
5. 8 声道不能单靠通道数猜布局。未声明本路径时，现有 `8ch = 4.1.2-strict` 行为保持不变。
6. 采样率 48 kHz。本需求不接受 192 kHz，也不做出声或板端验收。
7. 当前生产 16ch/9.1.6 里中置被送到未接音箱的槽位，不在本需求内。soak 结束前不改生产 toml、不碰 sender-host / N150 / MOTU。

## 已核对的程序现状

这些事实来自 2026-11-22 的仓库阅读和 `scratch/lead/dsp-host-downmix-cursor/TRACE-REPORT.md`。标 PENDING 的项不当作已验证。

| 项 | 现状 |
| --- | --- |
| 折叠实现 | `content_map.rs`：输出无 C 且输入有 C 时，`FL += g·C`、`FR += g·C`，然后 `C = 0`。其余源通道不动。输出有 C 时缓冲逐位不变。 |
| 余量 | D7：最坏相干叠加约 +4.649 dB。`program_peak_dbfs = -18` 时预测峰值约 −13.351 dBFS，不触发整体 trim。超 −6 dBFS 门时对全部源等量 trim。 |
| 接线 | `[content]` 的 `input_layout` / `output_layout` 都空则不启用；只填一个则拒绝启动。源通道数必须等于 `plan.n_in`。输出布局最大槽位 + 1 必须等于 `plan.n_out`。`4.1.2` 的容器宽度是 8。 |
| 已有用例 | `h16-5.1.2` → `4.1.2` 已断言 C 在扁平下标 2、TFL/TFR 在 6/7，且高度样本不被改写。输入布局计数断言目前是 8。 |
| 8 声道捕获 | `capture_layout_roles(8)` 固定返回 `4.1.2-strict`（`FL FR SL SR TFL TFR 空 LFE`）。flow 名现有 `2.0-2ch`、`4.1.2-7ch-sMPTE2110(41,2)`、`4.1.2-8ch`、`h16-dual-flow`、`custom-slots`。没有 5.1.2 单流名。此项在 trace 里标过 PENDING，本次阅读 `main.rs` 后已看到常量，仍没有 5.1.2 分支。 |
| 图的混音禁令 | plan 的 A_map 每角色至多一个源，源不可重复。中置扇出不能写进 plan。 |
| NSP | 原生 `5.1.2` 的契约、枚举和 SDP 标签已在 trace 中标 VERIFIED。真实 5.1.2 会话仍是 PENDING。本需求不改 NSP。 |

## 通道契约

源缓冲（内容适配前后，5.1.2 顺序）：

| 下标 | 角色 | 适配 |
| --- | --- | --- |
| 0 | FL | `FL + g·C` |
| 1 | FR | `FR + g·C` |
| 2 | C | 置 0 |
| 3 | LFE | 不折叠 |
| 4 | SL | 不动 |
| 5 | SR | 不动 |
| 6 | TFL | 不动 |
| 7 | TFR | 不动 |

`g = 0.7079458`。默认 `program_peak_dbfs = -18` 时没有整体 trim，上表里「不动」的样本与输入逐位相同。

plan 把适配后的源送进 4.1.2 的 8 槽容器。容器顺序与现有 4.1.2 strict 一致：`FL, FR, SL, SR, 高度左, 高度右, 空, LFE`。

| 容器槽 | 角色 | 源下标 | 物理输出 |
| --- | --- | --- | --- |
| 0 | FL | 0 | 0 |
| 1 | FR | 1 | 1 |
| 2 | SL | 4 | 2 |
| 3 | SR | 5 | 3 |
| 4 | 高度左（现布局 id `HFL`） | 6 | 4 |
| 5 | 高度右（现布局 id `HFR`） | 7 | 5 |
| 6 | 空 | 无 | 不接 |
| 7 | LFE（现布局 id `SUB1`） | 3 | 7 |

因此这张 plan 的 `capture_index = [0, 1, 4, 5, 6, 7, null, 3]`。源下标 2 不出现在任何槽上。

## 需求

### 需求 1 - 单流 5.1.2 输入布局

**用户故事：** 作为接收端，我能声明一路 8 声道的 NSP 5.1.2，并且不改掉已经写给双流 H16 的那份 5.1.2 预设。

#### 验收标准

1. When 读取 layout-presets 时，tspi-rt-host shall 接受新增输入布局 `nsp-5.1.2`：`streams` 为 `[8]`，角色依次为 `FL, FR, C, LFE, SL, SR, TFL, TFR`，通道依次为 0..7。
2. When 读取既有 `h16-5.1.2` 时，tspi-rt-host shall 仍得到 `streams: [6, 2]`，以及原来的角色到流/通道映射。
3. When `nsp-5.1.2` 对 `4.1.2` 构建 `CenterDownmixPlan` 且 center 为 `auto` 时，tspi-rt-host shall 得到 `Applied`，源通道数为 8，C 的通道为 2，FL 为 0，FR 为 1。

### 需求 2 - 中置折叠与直通

**用户故事：** 作为听音者，5.1.2 里的对白进左右音箱，环绕、高度和低音炮留在各自的位置。

#### 验收标准

1. While 输出布局是 `4.1.2`、`program_peak_dbfs` 为 −18，when 对 `nsp-5.1.2` 的平面缓冲调用已构建的 `CenterDownmixPlan` 时，tspi-rt-host shall 使 `FL' = FL + 0.7079458·C`、`FR' = FR + 0.7079458·C`，并将 C 置 0。浮点比较容差与现有 content_map 用例相同（1e-6）。
2. While 同上，when 折叠执行后，tspi-rt-host shall 使 LFE、SL、SR、TFL、TFR 与输入逐位相同。
3. While `program_peak_dbfs` 为 −18，when 折叠执行后，tspi-rt-host shall 不应用 D7 整体 trim。
4. When 同一块已折叠的缓冲被再次送入同一个 Applied 计划时，tspi-rt-host shall 不再把中置加进 FL/FR。
5. When 输出布局含 C 角色时，tspi-rt-host shall 保持现有 passthrough：状态为 `disabled_center_present`，缓冲逐位不变。
6. The tspi-rt-host shall 继续只在这个内容适配里做中置扇出，shall 不在 plan 的 A_map 里把同一个源送给两个角色。

### 需求 3 - 8 入 7 角色播放 plan

**用户故事：** 作为播放链，我把折叠后的 8 声道按物理 4.1.2 送进现有图，空槽仍然空着。

#### 验收标准

1. When 为 `nsp-5.1.2` → `4.1.2` 装载 plan 时，tspi-rt-host shall 使用 `n_in = 8`、容器宽度 `n_out = 8`、7 个角色，且 `capture_index = [0, 1, 4, 5, 6, 7, null, 3]`。
2. When 该 plan 与 `[content] input_layout = nsp-5.1.2`、`output_layout = 4.1.2` 一起启动时，tspi-rt-host shall 通过现有的源通道数和输出容器宽度校验。
3. When 该 plan 路由一帧时，tspi-rt-host shall 把容器槽 0..5 和 7 分别接到物理输出 0, 1, 2, 3, 4, 5, 7，槽 6 保持无源。
4. When 使用现有 4.1.2 的 7 声道 identity plan 时，tspi-rt-host shall 保持其 `capture_index = [0, 1, 2, 3, 4, 5, null, 6]`。本需求不改那张 plan 的语义。

### 需求 4 - 显式选择 5.1.2 捕获

**用户故事：** 作为配置者，我显式打开这条 5.1.2 路径；没打开时，今天的 8 声道 4.1.2 接收照旧。

#### 验收标准

1. When 捕获配置声明本路径的 flow 布局时，tspi-rt-host shall 把这一路 8 声道解释为 `nsp-5.1.2` 的通道顺序，并在报告里使用独立的 flow 布局名。该名字不得是 `4.1.2-8ch` 或 `4.1.2-7ch-sMPTE2110(41,2)`。
2. When 8 声道捕获没有声明本路径时，tspi-rt-host shall 继续使用 `4.1.2-strict` 的角色表（`FL FR SL SR TFL TFR 空 LFE`）。
3. When `[content]` 未配置时，tspi-rt-host shall 不启用中置折叠。
4. When `[content]` 只填写 `input_layout` 或只填写 `output_layout` 时，tspi-rt-host shall 拒绝启动。
5. When 本路径的 `[content]` 为 `input_layout = nsp-5.1.2`、`output_layout = 4.1.2`、`center = auto`、`program_peak_dbfs = -18` 时，tspi-rt-host shall 在进图前完成需求 2 的折叠，再按需求 3 的 plan 取通道。

### 需求 5 - 离线验证与交付边界

**用户故事：** 作为后续接手的人，我能凭测试和一页说明知道这条路径做了什么，以及还没在设备上跑过。

#### 验收标准

1. When 运行与本改动相关的 `cargo test` 时，执行环境 shall 使用 `TEMP`/`TMP` = `C:/WorkSpace/Claw/scratch/lead/testtmp`。相关套件 shall 全绿。新增用例至少覆盖需求 1 的布局、需求 2 的折叠与直通、需求 3 的 `capture_index`。
2. When 写证据时，交付物 shall 放在 `rt-stack/evidence/2026-11-22-centerdownmix-512/`，其中包含通道映射表、系数说明（含与 `0.70710678` 的 0.01 dB 差异）和测试日志。未运行的项 shall 写成 NOT_RUN。
3. When 完成本需求的代码改动时，仓库 shall 只做本地 commit，不 push。实现放在新分支上，不改写其他分支的历史。
4. The tspi-rt-host 本次改动 shall 不修改 NSP 仓库，不部署，不连接 sender-host、N150、MOTU 或其他远端主机，不把采样率设为 192 kHz。
5. When 写出任务总结时，总结 shall 写明接口落点、task-200 为复用而非新算法，以及离「NSP 改为 `outputLayout: 5.1.2`」仍缺的步骤。NSP 侧动作不在本需求的实现范围内。

### 需求 6 - 输出通道设置

**用户故事：** 作为操作者，我在设置页上看到每个物理输出槽，自己指定它是哪个位置，并选择有没有中置、有几个低音炮。

#### 验收标准

1. When 打开声道设置页时，页面 shall 列出物理输出槽，每个槽显示当前角色；角色用中文和英文简写同时标出，词表至少包括前左 FL、前右 FR、中置 C、低音 LFE、环绕左 SL、环绕右 SR、后左 BL、后右 BR、高度前左 TFL、高度前右 TFR、高度后左 TRL、高度后右 TRR。
2. When 选择「无中置」时，页面 shall 清掉输出侧的 C，并要求前左、前右都还有槽。When 选择「有中置」时，页面 shall 要求恰好一个输出槽是 C。
3. When 把低音炮数量设为 0、1、2 或 3 时，页面 shall 使输出侧的低音角色个数与该数量一致。低音 1 与现有布局里的 `SUB1` / 输入侧 `LFE` 是同一个语义角色。
4. When 为一个输出槽选择角色时，页面 shall 一个槽一个角色。同一非空角色出现在两个槽上时，页面 shall 拒绝保存。
5. When 套用输出预设 `4.1.2` 时，页面 shall 填入现有物理接法：槽 0..5 为前左、前右、环绕左、环绕右、高度前左、高度前右，槽 6 空，槽 7 为低音 1，无中置，低音炮数量为 1。填入之后仍可逐槽改。
6. The 页面 shall 不提供折叠系数 `0.7079458` 的编辑控件。plan 的 path 行由槽位角色编译出来，不另给一张下标编辑表。

### 需求 7 - 输入通道设置

**用户故事：** 作为操作者，我用同一套角色词，逐个声明每个输入通道是什么。

#### 验收标准

1. When 打开输入设置时，页面 shall 按通道顺序列出输入槽，每个槽用需求 6 的同一词表选择角色或空。
2. When 套用输入预设 `5.1.2` 时，页面 shall 把 8 个输入槽填为前左、前右、中置、低音、环绕左、环绕右、高度前左、高度前右。填入之后仍可逐槽改。
3. When 两个输入槽选择同一非空角色时，页面 shall 拒绝保存。
4. When 输入侧有中置、输出侧无中置、且前左和前右都在时，派生映射 shall 把该中置标为自动下混。When 输出侧有中置时，派生映射 shall 把输入中置标为直通到那个中置槽。

### 需求 8 - 映射图与未实现策略的口子

**用户故事：** 作为操作者，我看到输入每个角色流向哪些输出，像动力流一样从左到右；没有音箱的声道和还没做的上混，都在图上留着位置。

#### 验收标准

1. When 输入和输出角色都已设好时，页面 shall 画一张从左到右的流向图：左边是输入槽，右边是输出槽，连线表示派生策略。直通是一条线。中置下混是从中置分出的两条线，分别进入前左和前右，并标出 −3 dB。
2. While 某个输入角色在输出侧没有同名槽、且该角色不是正在下混的中置，when 显示映射时，页面 shall 把该角色画成未连接，策略名为 `reserved_downmix`。本次播放 shall 不把这个角色混进其它音箱。
3. While 某个输出角色没有同名输入，when 显示映射时，页面 shall 把该输出画成无输入。策略字段 shall 能表示 `silence` 与 `upmix`。本次引擎 shall 只执行 `silence`。配置里若写了 `upmix`，启动 shall 拒绝，并说明上混口子尚未接入。
4. When 输入角色集合是 5.1.2、输出角色集合是需求 6 的 4.1.2 预设时，派生结果 shall 与需求 2、需求 3 一致：中置下混，其余共有角色直通，槽 6 无源。
5. When 向 `/api/channelmap/draft` 提交这份由角色绑定生成的 5.1.2→4.1.2 时，服务 shall 拒绝用 remap 矩阵表达它。播放配置走内容适配和按角色编译的 plan。

## 不在本次范围

- 给正在 soak 的 16ch/9.1.6 生产配置打开 `[content]`。
- 把 `reserved_downmix` 做成真正的混音（包括 7.1.4 折进 5.1.2，以及一份低音拷贝到多只低音炮）。
- 实现 `upmix` 的信号生成。口子只留在映射策略字段和设置页的空位上。
- 用 channelmap remap 或 trim 表达中置扇出。
- 真实 AES67 会话、SDP/RTP 抓包和音箱出声。这些在总结里保持 PENDING 或 NOT_RUN。
