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
`routing.routes` 只有 N 条（lane → 选定 DAC），其余 DAC 输出静音。**未使用的物理通道不进 lane、不做卷积。**

## 2. 角色枚举（canonical，UI 显示中文）
| canonical | zh-CN 显示 | en-US 显示 | 分组 |
|---|---|---|---|
| L | 左 | Left | 前置 |
| R | 右 | Right | 前置 |
| C | 中置 | Center | 前置 |
| LFE | 低音炮（LFE） | LFE | 低频 |
| Ls | 左环绕 | Left surround | 环绕 |
| Rs | 右环绕 | Right surround | 环绕 |
| Lb | 左后环绕 | Left back | 环绕 |
| Rb | 右后环绕 | Right back | 环绕 |
| Tfl | 左前天空 | Top front left | 天空 |
| Tfr | 右前天空 | Top front right | 天空 |
| Trl | 左后天空 | Top rear left | 天空 |
| Trr | 右后天空 | Top rear right | 天空 |
| Sub | 辅助超低音 | Subwoofer | 低频 |
| Custom | 自定义 | Custom | 其它（必须写 custom_label） |
| None | 未使用 | Unused | 禁用行使用 |

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
- `preset`：`2.0` / `5.1` / `7.1` / `5.1.2` / `5.1.4` / `7.1.4` / `custom`。**预设决定"期望角色集"与默认 DAC 分配**，最终以 outputs 为准；预设是布局的起点，不是硬约束。
- **首次运行默认 = 2.0**：`L=DAC0`、`R=DAC1`（MOTU Main 1/2），其余 DAC 全部 `enabled=false, role=None`；输入映射默认 流ch0=L、流ch1=R。
- 切换预设时的行为：UI 按预设的角色集与"canonical 顺序→DAC 顺序"给出建议分配，**需要用户确认**后才覆盖 outputs（不静默丢弃手工编辑）。
- 预设的角色集：2.0=[L,R]；5.1=[L,R,C,LFE,Ls,Rs]；7.1=5.1+[Lb,Rb]；5.1.2=5.1+[Tfl,Tfr]；5.1.4=5.1+[Tfl,Tfr,Trl,Trr]；7.1.4=7.1+[Tfl,Tfr,Trl,Trr]；custom=任意。
- `inputs`：AES67 输入流通道 → 角色（当前 2ch，默认 0→L、1→R）；不改变接收侧映射，只决定 content.folds。
- `outputs` 固定列出全部设备通道（22），`enabled=false` 的行 role 必须为 None。
- `gain_db`（可选，默认 0）：该物理输出的电平微调；生成方案时写入对应 lane 的 protection gain。

## 4. 方案生成规则（profile → plan）
1. 启用行按 canonical 角色顺序排序为 lane 0..N-1（同角色内按 DAC 升序）；同一角色出现多次是正常情况（L×8、R×8、LFE×3 等）；
2. `content`: `in_channels=22`，`out_lanes=N`，`folds`: 输入流通道（AES67 map 后的引擎通道）→ **该角色的每一条 lane 都连一条 fold**（fan-out，增益 1.0，最终电平再由每 lane 的 protection gain/gain_db 调整）；输入角色缺失时该组静音（不报错）；
3. MIMO 背景：重复角色 = 同一角色的多个执行器，后续 MIMO/modal 设计可按角色组拿到多条 IR，用于多音箱抵消；plan 本身不需要额外字段。
4. `crossover`: 每 lane 的滤波器（identity 或校准 FIR）；`matrix`: N×N（identity 或设计结果）；`protection`: N lane（gain = profile gain_db）；
5. `routing`: `dac_channels=22`，`routes = [[lane_i, dac_i], ...]`（profile 的启用行）；未启用 DAC 不列，输出为静音；
6. 生成后必须走 `inspect`（准入预算/图编译）→ `prepare` → `commit`，失败/mismatch 如实报错，不自动回退到旧方案。

## 5. API 增量（加到 docs/API.md，均为加法）
| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/output-profile` | `{schema, profile, validation:{ok, errors[], warnings[]}, plan:{lanes, enabled_outputs, dac_channels}}`（read-only） |
| PUT | `/api/output-profile` | 两阶段：`{stage:"prepare", profile}` → `{ok, profile, diff, validation, token}`（不碰引擎）；`{stage:"commit", token}` 保存；`{stage:"abort", token}` 丢弃 |
| POST | `/api/output-profile/plan` | `{}` → 用已保存 profile + 当前滤波器状态生成方案 `{ok, plan_path, sha256, lane_count, inspect}`；之后走现有 `/api/prepare`+`/api/commit` |
| GET | `/api/channels` | 加法字段 `speaker_roles:[{dac, role, enabled}]`（给 routing/channels 页显示角色名） |
- 错误体沿用 M1；保存 profile 不改数据面，只有"应用"才 prepare/commit 方案。

## 6. UI 设计（新页面"输出映射 / Speaker Layout"）
- 顶部：设备/引擎信息（设备通道数 22、当前引擎 22ch、当前方案 graph/version）、**布局预设选择**（2.0/5.1/7.1/5.1.2/5.1.4/7.1.4/custom，首次默认 2.0）、输入映射（AES67 ch→角色）。
- 选择预设时自动给出建议的 DAC→角色分配（按 canonical 顺序），弹确认后再应用；用户可再手工改任意行/重复角色。
- 表格：22 行物理输出，列：物理通道号（1..22）｜当前方案 lane/DAC（来自 /api/channels mapping，未知则"—"）｜启用开关｜角色下拉（中文显示）｜自定义名称（仅 Custom）｜备注。
- 实时校验区：**重复角色是正常的**（不报错，显示组统计如「左 ×8 / 右 ×8 / 低音炮 ×3」）；只报结构性矛盾（启用但角色为未使用、DAC 越界、全部禁用、Custom 无名称）；缺少角色只做信息提示；显示「将生成 N lane 方案，未选通道静音」。
- 角色分组视图：按角色汇总（L、R、LFE…）列出各组的物理输出号与数量，方便确认「8 只左主箱都标成 L」这类配置。
- 底部：保存草稿（PUT prepare → 展示 diff/校验）→ 应用（POST output-profile/plan → inspect 预览 → prepare/commit/abort）；取消/丢弃。
- routing 页：显示角色名（如"左环绕 → MOTU 输出 5"），并标注当前值来源（plan mirror/未知）。
- 未来：通道识别测试音（需出声窗口，≤10 s/次、结束静音）。

## 7. 分阶段
- P0 设计定稿（本文）+ API 合同（docs/API.md 增量）
- P1 后端：profile store/校验/GET/PUT（两阶段）+ 生成方案接口 + 合同测试（全离线）
- P2 前端：页面/预设/校验/两阶段保存 + routing 显示角色 + i18n/契约测试
- P3 方案生成接线：profile → content/routing（复用 plan-builder）；inspect→prepare→commit；真机验证需窗口（NOT_RUN 到窗口）
- P4 通道识别测试音（窗口内）

## 8. 待确认
1. 预设集合是否就是：2.0 / 5.1 / 7.1 / 5.1.2 / 5.1.4 / 7.1.4 / custom（首次默认 2.0）？以后可加法扩 9.1.6 等。
2. 应用方式：保存 profile 后必须手动"应用"才重建方案（建议）；还是保存自动重建？
3. 输入映射（AES67 2ch→L/R）是否同页（建议是）？
4. 通道识别测试音是否纳入首版（需窗口）？
5. 同角色 fan-out 的默认电平：每条 lane 都拿满电平（1.0，靠 gain_db 微调，适合 MIMO 单独驱动），还是按组做 1/√N 归一化（适合被动并联音箱）？（建议默认 1.0，保留 group 归一化作为可选项）