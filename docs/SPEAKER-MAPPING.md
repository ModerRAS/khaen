# khaen 输出通道角色与布局 profile 设计（草案 v0.1，2026-11-26）

> 目的：把"物理输出通道 ↔ 音箱角色"的配置从写死的 identity 方案里拿出来，做成可在 WebUI 编辑的布局 profile。
> 运行时（rt-host）仍只认数字：lane → DAC；角色/中文名只存在于配置层和 UI，不进实时路径。

## 1. 术语（先说清"通道"的三种含义）
| 术语 | 含义 | 当前值 |
|---|---|---|
| 设备通道（device channel） | ALSA PCM 交错帧宽度，由声卡决定；plan.content.in_channels 必须等于它 | MOTU 22 |
| 物理输出（physical output） | 设备通道的一个 DAC 序号（0..21），可能接音箱、可能是回环/虚拟/未接线 | 22 个，部分未使用 |
| lane | 方案里真正参与处理的一条声道（crossover/matrix/protection 的列/行） | 由 profile 的启用音箱数决定，通常远小于 22 |
| 角色（role） | 一条 lane/物理输出的语义标签（L/R/C/LFE/Ls/…） | profile 配置 |

**关键约束（图编译）**：`content.in_channels == 设备通道数`（22，固定）；`content.out_lanes == N`（启用音箱数）；
`crossover.lanes = matrix.cols = matrix.rows = protection.lanes = N`；`routing.dac_channels == 22`；
`routing.routes` 只有 N 条（lane → 选定 DAC），其余 DAC 不进路由。**未使用的物理通道不进 lane、不做卷积，也不做任何静音/控制面写入**（rt-host 输出缓冲照常写零，自然无信号）。

## 2. 角色枚举（canonical，UI 显示中文）
| canonical | zh-CN 显示 | en-US 显示 | 分组 |
|---|---|---|---|
| L | 左 | Left | 前置 |
| R | 右 | Right | 前置 |
| C | 中置 | Center | 前置 |
| Lw | 左前宽 | Front wide left | 前置（9.1 用） |
| Rw | 右前宽 | Front wide right | 前置（9.1 用） |
| LFE | 低音炮（LFE） | LFE | 低频 |
| Ls | 左环绕 | Left surround | 环绕 |
| Rs | 右环绕 | Right surround | 环绕 |
| Lb | 左后环绕 | Left back | 环绕 |
| Rb | 右后环绕 | Right back | 环绕 |
| Tfl | 左前天空 | Top front left | 天空（最多 6 只） |
| Tfr | 右前天空 | Top front right | 天空 |
| Tml | 左中天空 | Top middle left | 天空 |
| Tmr | 右中天空 | Top middle right | 天空 |
| Trl | 左后天空 | Top rear left | 天空 |
| Trr | 右后天空 | Top rear right | 天空 |
| Sub | 辅助超低音 | Subwoofer | 低频 |
| Custom | 自定义 | Custom | 其它（必须写 custom_label） |
| None | 未使用 | Unused | 禁用行使用 |

- **布局能力上限：天空最多 6 只**（Tfl/Tfr/Tml/Tmr/Trl/Trr，对应 9.1.6）；地面最多 9 只（L/R/C/Lw/Rw/Ls/Rs/Lb/Rb）+ LFE/Sub；角色是自助勾选，9.1.6 只是"全勾满"的一个快捷预设，不是硬性命名。
- **同一角色允许重复任意多次**（例如 8 只左主箱可以都是 `L`、多个低音炮都是 `LFE`/`Sub`）——这是 MIMO/多执行器抵消的设计输入，不是错误。
- **缺少角色不报错、不阻塞**：布局声明了某角色但实际没接线，视为"真实缺少"（只做信息提示）。校验只拒绝结构性矛盾（启用了却填 None、DAC 越界、全部禁用、Custom 无名称）。
- 重复角色在 UI/生成方案里作为**角色组**处理（如 `L ×8`、`LFE ×3`），组内按 DAC 升序排列。
- 中文/英文显示名统一走 i18n（`speakerRole.*` key），代码里只用 canonical 字符串，禁止把中文写进逻辑。

## 3. Profile 数据模型（JSON，daemon 状态目录保存）
```json
{
  "schema": "khaen-output-profile/v1",
  "device": { "channels": 22 },
  "preset": "custom",
  "outputs": [
    { "dac": 0, "enabled": true,  "role": "L", "gain_db": 0.0 },
    { "dac": 1, "enabled": true,  "role": "L", "gain_db": 0.0 },
    { "dac": 2, "enabled": true,  "role": "R", "gain_db": 0.0 },
    { "dac": 3, "enabled": true,  "role": "R", "gain_db": 0.0 },
    { "dac": 4, "enabled": true,  "role": "LFE", "gain_db": 0.0 },
    { "dac": 5, "enabled": false, "role": "None" }
  ],
  "inputs": [
    { "stream_channel": 0, "role": "L" },
    { "stream_channel": 1, "role": "R" }
  ]
}
```
- **角色自选模型（主模型）**：角色枚举按 **9.1.6 为上限**设计——地面 9 个（L/R/C/Lw/Rw/Ls/Rs/Lb/Rb）、低频 LFE/Sub（可多只）、天空 6 个（Tfl/Tfr/Tml/Tmr/Trl/Trr）。用户勾选需要的角色与数量，再把每一只分配到物理输出 DAC；布局名字是**派生结果**，不是输入约束。
- **首次运行默认 = 2.0**：`L=DAC0`、`R=DAC1`（MOTU Main 1/2），其余 DAC 全部 `enabled=false, role=None`；输入映射默认 流ch0=L、流ch1=R。
- **快捷预设仅作填充器**：2.0 / 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.4 / 9.1.6 只是"一键勾选对应角色集并给建议 DAC 分配"的按钮；用户可以在此基础上增删角色、给同一角色加多只、改任何 DAC。预设不写入方案、不构成约束。9.1.6 = 上面 9 个地面 + LFE + 6 个天空全勾。
- **布局标签自动派生**：例如勾选 L/R/C/LFE/Ls/Rs + Tfl/Tfr 时显示「5.1.2（8 只）」；9.1.6 全勾显示「9.1.6（16 只）」；若同一角色多只（L×8/R×8）则显示「自定义：地面 8 + 天空 0 + 低音炮 3」；不强制用户先选一个标准 layout。
- **输入源两类**（不再单独区分光纤/模拟，统一叫"声卡输入"）：
  1. `aes67`：网络 AES67 RTP 输入（当前 2ch；以后可多流）；
  2. `device`（声卡输入）：声卡自己的采集通道（MOTU 模拟输入 + 光口；光口 S/PDIF/Toslink 模式 2ch，ADAT 模式 48k 8ch、96k 4ch；实际通道列表由设备能力只读探测给出）。
- 每个输入通道用**同一套角色标签**标注（L/R/C/…/Tfl…，或 None）；引擎按"输入通道角色 → 该角色的所有输出 lane"自动 fan-out（content.folds）——这就是"写上标签，引擎自动映射"。
- **AES67 预设**：选择 AES67 时可一键带入标准通道顺序（Stereo / 5.1 / 7.1 / 7.1.4 / 9.1.6；ITU 与 SMPTE 两种顺序都提供），预设只填角色，之后仍可逐个改。
- 输入源可分别启用；同一角色若被多个已启用输入通道标注，按**和（sum）**进入该角色组（页面明示）。至少启用一个输入源。
- 当前默认：AES67 启用、ch0→L、ch1→R；声卡输入未启用、全部 None。
- `outputs` 固定列出全部设备通道（22），`enabled=false` 的行 role 必须为 None；同一 role 可出现在任意多行（多只同类音箱），组内按 DAC 升序。
- `inputs` 数据形态：`{sources:[{kind:"aes67"|"device", enabled, channels:[{channel, role}], preset?}]}`；`folds` 只由"已启用输入通道的角色"决定。
- `gain_db`（可选，默认 0）：该物理输出的电平微调；生成方案时写入对应 lane 的 protection gain。

## 4. 方案生成规则（profile → plan）
1. 启用行按 canonical 角色顺序排序为 lane 0..N-1（同角色内按 DAC 升序）；同一角色出现多次是正常情况（L×8、R×8、LFE×3 等）；
2. `content`: `in_channels=22`，`out_lanes=N`，`folds`: 已启用输入源的通道（AES67 RTP 或声卡采集，经 map 后的引擎通道）按角色 → **该角色的每一条 lane 都连一条 fold**（fan-out，增益 1.0，最终电平再由每 lane 的 protection gain/gain_db 调整；多源同角色求和）；输入角色缺失时该组静音（不报错）；
3. MIMO 背景：重复角色 = 同一角色的多个执行器，后续 MIMO/modal 设计可按角色组拿到多条 IR，用于多音箱抵消；plan 本身不需要额外字段。
4. `crossover`: 每 lane 的滤波器（identity 或校准 FIR）；`matrix`: N×N（identity 或设计结果）；`protection`: N lane（gain = profile gain_db）；
5. `routing`: `dac_channels=22`，`routes = [[lane_i, dac_i], ...]`（profile 的启用行）；未启用 DAC 不列路由。**未使用的 DAC 不做任何静音/控制面写入**：rt-host 输出缓冲照常写零（自然无信号），不折腾 MOTU/CueMix；它们就"放在那儿"，不会影响任何功能，也不会有人用；
6. 生成后必须走 `inspect`（准入预算/图编译）→ `prepare` → `commit`，失败/mismatch 如实报错，不自动回退到旧方案。

### 4.1 MOTU/CueMix 控制边界（硬规则）
CueMix/控制面写入**只有 4 类用途**：
1. 切换采样率；
2. 切换光口模式 ADAT ↔ Toslink(S/PDIF)；
3. 整体静音；
4. 整体音量。

除此之外**不通过 CueMix 写任何东西**：不做 per-channel mute/gain/路由，不用它给未使用 DAC 静音，也不用它给测试音选通道（测试音走 rt-host 的 ALSA 输出）。读状态（当前采样率、光口模式、全局音量/静音）允许，用于能力探测与输入通道数判断。

## 5. API 增量（加到 docs/API.md，均为加法）
| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/audio-profile` | `{schema:"khaen-audio-profile/v2", profile, validation:{ok, errors[], warnings[]}, capability:{aes67:{channels,map}, device_inputs:{available, optical_mode, sample_rate, channels[]}}, plan:{lanes, enabled_outputs, dac_channels}}`（read-only） |
| PUT | `/api/audio-profile` | 两阶段：`{stage:"prepare", profile}` → `{ok, profile, diff, validation, token}`（不碰引擎）；`{stage:"commit", token}` 保存；`{stage:"abort", token}` 丢弃 |
| POST | `/api/audio-profile/plan` | `{}` → 用已保存 profile + 当前滤波器状态生成方案 `{ok, plan_path, sha256, lane_count, inspect}`；之后走现有 `/api/prepare`+`/api/commit` |
| GET | `/api/device/inputs` | 声卡输入能力（只读）：`{available, optical_mode:"spdif"|"adat"|null, sample_rate, channels:[{index, label?}], source:"motu-alsa", reason?}`；取不到给 `available:false` + 原因，不编数值 |
| GET | `/api/channels` | 加法字段 `speaker_roles:[{dac, role, enabled}]`、`input_roles:[{source, channel, role}]`（给 routing/channels 页显示角色名） |
| POST | `/api/speaker-test` | 通道识别测试音：`{dac:0..21, ms:500..10000, kind:"noise"\|"sine", freq_hz?:number, db?:-60..-12, audible:true, confirm:true}` → `{ok, dac, ms, kind, level_db, remaining_ms}`；默认 db=-30（很轻，有声即可）；经 daemon 校验后转发 rt-host 新控制 op `test_tone` |
| POST | `/api/speaker-test/stop` | 立即停止当前测试音（幂等） |
| GET | `/api/status` | 加法字段 `speaker_test:{active, dac, remaining_ms, kind}` |
- 错误体沿用 M1；保存 profile 不改数据面，只有"应用"才 prepare/commit 方案。

## 6. UI 设计（新页面"输出映射 / Speaker Layout"）
- 顶部：设备/引擎信息（设备通道数 22、当前引擎 22ch、当前方案 graph/version）、布局摘要（自动派生，如「5.1.2（8 只）」）、快捷预设按钮（仅填充用）。
- **输入区（同页）**：两个输入源分块——「AES67」和「声卡输入」；每块可单独启用，逐通道用角色下拉标注（同一套角色标签 + 未使用），AES67 另有标准顺序预设按钮（Stereo/5.1/7.1/7.1.4/9.1.6，ITU/SMPTE 可切换）；声卡输入显示只读能力（光口模式、采样率、通道数：Toslink 2ch / ADAT 48k 8ch / 96k 4ch），取不到显示 null+原因。页面实时显示"输入角色 → 输出角色组"自动映射预览。
- **第 1 步「选择角色」**：按组展示可选项——地面 9 个（左 L / 右 R / 中置 C / 左前宽 Lw / 右前宽 Rw / 左侧环绕 Ls / 右侧环绕 Rs / 左后环绕 Lb / 右后环绕 Rb）、天空 6 个（Tfl/Tfr/Tml/Tmr/Trl/Trr，对应 9.1.6 上限）、低频（低音炮 LFE / 辅助超低音 Sub）。每个角色旁有数量（默认 1，可加多只）；自定义角色可加名称。
- **第 2 步「分配到输出」**：每个角色实例一行 → 选择物理输出通道（DAC 下拉，显示通道号/当前 lane/来源）；同一角色多只时逐只分配；未分配的实例不生成 lane，页面提示"还有 N 只未分配"。
- 分配完成后显示：将生成 N lane、角色组统计（左 ×8 / 右 ×8 / 低音炮 ×3）、未使用 DAC 列表（保持无信号：不做静音操作、不写 MOTU/CueMix，就放在那里）、各输入角色映射到哪些输出组。
- 物理输出表格视图（可选切换）：22 行，列：物理通道号（1..22）｜当前方案 lane/DAC（来自 /api/channels mapping，未知则"—"）｜启用开关｜角色下拉（中文显示，可重复）｜自定义名称｜gain_db｜识别按钮。
- 实时校验区：**重复角色是正常的**（不报错，显示组统计如「左 ×8 / 右 ×8 / 低音炮 ×3」）；只报结构性矛盾（启用但角色为未使用、DAC 越界、全部禁用、Custom 无名称）；缺少角色只做信息提示；显示「将生成 N lane 方案，未选通道静音」。
- 角色分组视图：按角色汇总（L、R、LFE…）列出各组的物理输出号与数量，方便确认「8 只左主箱都标成 L」这类配置。
- 底部：保存草稿（PUT prepare → 展示 diff/校验）→ 应用（POST output-profile/plan → inspect 预览 → prepare/commit/abort）；取消/丢弃。
- routing 页：显示角色名（如"左环绕 → MOTU 输出 5"），并标注当前值来源（plan mirror/未知）。
- **通道识别测试音（首版做）**：表格每行一个「识别」按钮；点击后弹确认框（显示将出声的物理通道、时长、电平），确认后 POST `/api/speaker-test`，运行时显示「正在出声 1.0 s（第 3 号输出）」，按钮可「立即停止」。
- 测试音语义：**直接写 rt-host 的 ALSA 输出通道（原始 DAC 号），不经过 plan routing**——目的是识别"物理输出号线 ↔ 实际音箱"，与当前方案路由无关；测试期间输出只在该通道放测试音、其余静音，结束后自动恢复方案输出，plan/图/状态不变。
- 安全约束（服务端强制）：每次 ≤10 s（默认 1 s，UI 上限 3 s）、`audible:true`+`confirm:true` 必须同时给、同一时刻只允许一个测试音、`/api/speaker-test/stop` 与 `/api/mute` 都能立即打断、结束自动静音；忙（校准/热重配）时拒绝 409。
- **音量要轻**：默认 **-30 dBFS**（"有声就行"，不是演示音量），三档可选 -40 / -30 / -20，服务端硬上限 **-12 dBFS**；首尾各 20 ms 淡入/淡出，避免"咔哒"声比测试音还响；噪声用带限粉噪（500 Hz–4 kHz，不刺耳）。
- 出声窗口：daemon 配置 `KHAEN_SPEAKER_TEST=on` 才启用；可选 `KHAEN_SOUND_WINDOW=08:30-12:00,14:30-18:00` 服务端校验，窗口外 403 `sound_window_closed`（与工作区出声规则一致，需用户明确授权）。

## 7. 分阶段
- P0 设计定稿（本文）+ API 合同（docs/API.md 增量）
- P1 后端：profile store/校验/GET/PUT（两阶段，含输入源与声卡输入能力只读探测）+ 生成方案接口 + 合同测试（全离线）
- P2 前端：页面/输入区/AES67 预设/校验/两阶段保存 + routing 显示角色 + i18n/契约测试
- P3 方案生成接线：profile → content/routing（复用 plan-builder）；inspect→prepare→commit；真机验证需窗口（NOT_RUN 到窗口）
- P4 声卡输入采集（数据面新功能，独立任务）：rt-host 增加 ALSA capture 输入（MOTU 采集通道）→ 引擎输入块，能力探测（光口模式/采样率→通道数），与 AES67 源的启用/求和策略；真机采集验证需窗口（NOT_RUN 到窗口）
- P3.5 通道识别测试音：rt-host `test_tone` 控制 op（块边界覆盖原始 DAC，自动停止/状态上报）+ daemon `/api/speaker-test` 代理+安全校验 + UI 按钮/确认/停止；本地用 fake 输出测试，真机出声验证留给窗口（NOT_RUN 到窗口）

## 8. 待确认
1. ~~角色集/预设~~ → **已定**：角色按 9.1.6 上限（地面 9 + LFE + 天空 6），预设含 9.1.6，首次默认 2.0。
2. 应用方式：保存 profile 后必须手动"应用"才重建方案（建议）；还是保存自动重建？
3. 输入源：能否同时启用 AES67 + 声卡输入（同角色求和），还是同一时刻只允许一个？（建议都可启用、默认只开 AES67）
4. 声卡输入通道范围：包含模拟输入 2ch + 光口（Toslink 2 / ADAT 8@48k、4@96k）全部列出声卡采集通道，还是只列光口的 2/8/4？（建议：按设备实际采集通道全列，只读探测）
5. AES67 预设顺序：ITU（L,R,C,LFE,Ls,Rs…）和 SMPTE（C,L,R,Ls,Rs,LFE…）都提供，默认用哪个？
6. ~~通道识别测试音是否纳入首版~~ → **已确认纳入首版**（受 `KHAEN_SPEAKER_TEST`/出声窗口/≤10 s 约束，真机出声验证留窗口）。
7. 同角色 fan-out 的默认电平：每条 lane 都拿满电平（1.0，靠 gain_db 微调，适合 MIMO 单独驱动），还是按组做 1/√N 归一化（适合被动并联音箱）？（建议默认 1.0，保留 group 归一化作为可选项）
8. 测试音默认参数：带限粉噪 1 s、**-30 dBFS**（三档 -40/-30/-20，硬上限 -12，20 ms 淡入淡出）是否合适？